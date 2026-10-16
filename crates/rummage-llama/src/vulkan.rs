//! 直接用系统的 Vulkan 加载器按 `vkEnumeratePhysicalDevices` 的原始顺序列出物理设备，推理进程在加载 ggml 之前
//! 据此算出 `GGML_VK_VISIBLE_DEVICES` 的取值（见 rummage-embed 的 `worker`）。
//!
//! - 只建一个临时实例（API 1.1，不开层和扩展）、查询属性，不创建逻辑设备；返回前销毁实例。
//! - PCI 号来自 `VK_EXT_pci_bus_info`，格式与 ggml 的 `device_id` 相同（b11461 `ggml-vulkan.cpp`
//!   `ggml_backend_vk_get_device_id`：`%04x:%02x:%02x.%x`）；设备不支持该扩展或 API 低于 1.1 时为空，与 ggml 一致。
//!   名称即 `deviceName`，也就是 ggml 的设备描述。另取 `deviceUUID`，用来认出重复出现的同一块卡。
//! - 加载器动态加载（Windows `vulkan-1.dll`，其他平台 `libvulkan.so.1`），与 ggml-vulkan 链接的是同一个；
//!   结构体只声明用到的字段，`VkPhysicalDeviceLimits` 及之后的部分用足够大的缓冲区代替（见 `Properties`）。

use std::ffi::{CStr, c_char, c_void};
use std::ptr;

use libloading::Library;

use crate::Error;

#[cfg(windows)]
const LOADER: &str = "vulkan-1.dll";
#[cfg(target_os = "android")]
const LOADER: &str = "libvulkan.so";
#[cfg(not(any(windows, target_os = "android")))]
const LOADER: &str = "libvulkan.so.1";

// vulkan_core.h 中的取值。
const STRUCTURE_TYPE_APPLICATION_INFO: i32 = 0;
const STRUCTURE_TYPE_INSTANCE_CREATE_INFO: i32 = 1;
const STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2: i32 = 1000059001;
const STRUCTURE_TYPE_PHYSICAL_DEVICE_PCI_BUS_INFO_PROPERTIES_EXT: i32 = 1000212000;
const STRUCTURE_TYPE_PHYSICAL_DEVICE_ID_PROPERTIES: i32 = 1000071004;
const SUCCESS: i32 = 0;
const INCOMPLETE: i32 = 5;
/// `VK_MAKE_API_VERSION(0, 1, 1, 0)`：`vkGetPhysicalDeviceProperties2` 从 1.1 起是核心函数。
const API_1_1: u32 = (1 << 22) | (1 << 12);

type Handle = *mut c_void;

#[repr(C)]
struct ApplicationInfo {
    s_type: i32,
    p_next: *const c_void,
    application_name: *const c_char,
    application_version: u32,
    engine_name: *const c_char,
    engine_version: u32,
    api_version: u32,
}

#[repr(C)]
struct InstanceCreateInfo {
    s_type: i32,
    p_next: *const c_void,
    flags: u32,
    application_info: *const ApplicationInfo,
    enabled_layer_count: u32,
    enabled_layer_names: *const *const c_char,
    enabled_extension_count: u32,
    enabled_extension_names: *const *const c_char,
}

/// `VkPhysicalDeviceProperties`：`limits`（504 字节）与 `sparseProperties`（20 字节）不读，用 1 KiB 的缓冲区代替，
/// 对齐与 `limits` 相同（含 `VkDeviceSize`，8 字节）；驱动只写实际大小，多出的部分不碰。
#[repr(C)]
struct Properties {
    api_version: u32,
    driver_version: u32,
    vendor_id: u32,
    device_id: u32,
    device_type: i32,
    device_name: [c_char; 256],
    pipeline_cache_uuid: [u8; 16],
    rest: [u64; 128],
}

#[repr(C)]
struct Properties2 {
    s_type: i32,
    p_next: *mut c_void,
    properties: Properties,
}

#[repr(C)]
struct PciBusInfo {
    s_type: i32,
    p_next: *mut c_void,
    domain: u32,
    bus: u32,
    device: u32,
    function: u32,
}

/// `VkPhysicalDeviceIDProperties`
#[repr(C)]
struct IdProperties {
    s_type: i32,
    p_next: *mut c_void,
    device_uuid: [u8; 16],
    driver_uuid: [u8; 16],
    device_luid: [u8; 8],
    device_node_mask: u32,
    device_luid_valid: u32,
}

#[repr(C)]
struct ExtensionProperties {
    name: [c_char; 256],
    spec_version: u32,
}

type CreateInstance =
    unsafe extern "system" fn(*const InstanceCreateInfo, *const c_void, *mut Handle) -> i32;
type DestroyInstance = unsafe extern "system" fn(Handle, *const c_void);
type EnumeratePhysicalDevices = unsafe extern "system" fn(Handle, *mut u32, *mut Handle) -> i32;
type GetProperties = unsafe extern "system" fn(Handle, *mut Properties);
type GetProperties2 = unsafe extern "system" fn(Handle, *mut Properties2);
type EnumerateExtensions =
    unsafe extern "system" fn(Handle, *const c_char, *mut u32, *mut ExtensionProperties) -> i32;

/// 一块 Vulkan 物理设备；在返回列表中的位置就是原始序号。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhysicalDevice {
    /// `deviceName`，即 ggml 的设备描述。
    pub name: String,
    /// 形如 `0000:06:00.0`；不支持 `VK_EXT_pci_bus_info` 时为空。
    pub pci_id: Option<String>,
    /// `deviceUUID`：同一块卡经两个驱动（或重复注册的同一驱动）出现两次时相同，ggml 默认列表据此去重。
    /// API 1.0 的设备为空。
    pub uuid: Option<[u8; 16]>,
}

fn err(msg: impl Into<String>) -> Error {
    Error::Vulkan(msg.into())
}

/// 取加载器导出的函数。
///
/// # Safety
/// `T` 必须是该符号的函数指针类型。
unsafe fn sym<T: Copy>(lib: &Library, name: &[u8]) -> Result<T, Error> {
    // SAFETY: 由调用方保证类型正确。
    unsafe { lib.get::<T>(name) }.map(|s| *s).map_err(|e| {
        let name = String::from_utf8_lossy(&name[..name.len() - 1]);
        err(format!("Vulkan 加载器缺少 {name}：{e}"))
    })
}

/// 按原始顺序列出物理设备（见模块说明）。
pub fn physical_devices() -> Result<Vec<PhysicalDevice>, Error> {
    // SAFETY: 加载的是系统的 Vulkan 加载器，它的初始化例程不依赖本进程的状态。
    let lib = unsafe { Library::new(LOADER) }
        .map_err(|e| err(format!("无法加载 Vulkan 加载器 {LOADER}：{e}")))?;
    // SAFETY: 函数类型与 vulkan_core.h 中的声明一致（VKAPI_CALL 在 Windows x64 与其他 64 位平台上即 `system`
    // 调用约定）；取出的函数指针只在 `lib` 存活期间使用。
    let (create, destroy, enumerate, props, props2, exts) = unsafe {
        (
            sym::<CreateInstance>(&lib, b"vkCreateInstance\0")?,
            sym::<DestroyInstance>(&lib, b"vkDestroyInstance\0")?,
            sym::<EnumeratePhysicalDevices>(&lib, b"vkEnumeratePhysicalDevices\0")?,
            sym::<GetProperties>(&lib, b"vkGetPhysicalDeviceProperties\0")?,
            sym::<GetProperties2>(&lib, b"vkGetPhysicalDeviceProperties2\0")?,
            sym::<EnumerateExtensions>(&lib, b"vkEnumerateDeviceExtensionProperties\0")?,
        )
    };

    let app = ApplicationInfo {
        s_type: STRUCTURE_TYPE_APPLICATION_INFO,
        p_next: ptr::null(),
        application_name: c"Rummage".as_ptr(),
        application_version: 0,
        engine_name: ptr::null(),
        engine_version: 0,
        api_version: API_1_1,
    };
    let info = InstanceCreateInfo {
        s_type: STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
        p_next: ptr::null(),
        flags: 0,
        application_info: &app,
        enabled_layer_count: 0,
        enabled_layer_names: ptr::null(),
        enabled_extension_count: 0,
        enabled_extension_names: ptr::null(),
    };
    let mut instance: Handle = ptr::null_mut();
    // SAFETY: 创建信息与其中的指针在调用期间有效；不传分配回调。
    let res = unsafe { create(&info, ptr::null(), &mut instance) };
    if res != SUCCESS || instance.is_null() {
        return Err(err(format!("无法创建 Vulkan 实例（VkResult {res}）")));
    }
    // 提前返回时也销毁实例。
    struct Instance(Handle, DestroyInstance);
    impl Drop for Instance {
        fn drop(&mut self) {
            // SAFETY: 实例由上面创建、只销毁一次；由它取得的物理设备句柄此后不再使用。
            unsafe { (self.1)(self.0, ptr::null()) }
        }
    }
    let instance = Instance(instance, destroy);

    let mut count = 0u32;
    // SAFETY: 实例有效；先只取数量。
    let res = unsafe { enumerate(instance.0, &mut count, ptr::null_mut()) };
    if res != SUCCESS {
        return Err(err(format!("无法列出 Vulkan 物理设备（VkResult {res}）")));
    }
    let mut handles: Vec<Handle> = vec![ptr::null_mut(); count as usize];
    // SAFETY: 缓冲区可容纳 `count` 个句柄，加载器最多写入这么多并更新 `count`。
    let res = unsafe { enumerate(instance.0, &mut count, handles.as_mut_ptr()) };
    if res != SUCCESS && res != INCOMPLETE {
        return Err(err(format!("无法列出 Vulkan 物理设备（VkResult {res}）")));
    }
    handles.truncate(count as usize);

    let mut out = Vec::with_capacity(handles.len());
    for &dev in &handles {
        // SAFETY: 物理设备句柄来自本实例；`Properties` 不小于 `VkPhysicalDeviceProperties`（见其说明）。
        let base = unsafe {
            let mut p: Properties = std::mem::zeroed();
            props(dev, &mut p);
            p
        };
        // SAFETY: 驱动写入的 `deviceName` 是以空字符结尾的 UTF-8 字符串（数组长度 256 含结尾）。
        let name = unsafe { CStr::from_ptr(base.device_name.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        // API 1.0 的设备查不到这两项，ggml 也不会选它（要求 1.2）。
        let (uuid, pci_id) = if base.api_version >= API_1_1 {
            let pci_ext = has_pci_bus_info(dev, exts)?;
            let mut pci = PciBusInfo {
                s_type: STRUCTURE_TYPE_PHYSICAL_DEVICE_PCI_BUS_INFO_PROPERTIES_EXT,
                p_next: ptr::null_mut(),
                domain: 0,
                bus: 0,
                device: 0,
                function: 0,
            };
            let mut id = IdProperties {
                s_type: STRUCTURE_TYPE_PHYSICAL_DEVICE_ID_PROPERTIES,
                p_next: if pci_ext {
                    (&mut pci as *mut PciBusInfo).cast()
                } else {
                    ptr::null_mut()
                },
                device_uuid: [0; 16],
                driver_uuid: [0; 16],
                device_luid: [0; 8],
                device_node_mask: 0,
                device_luid_valid: 0,
            };
            // SAFETY: 设备支持 API 1.1（ID 属性是 1.1 的核心结构）；只在支持扩展时链上 `pci`；
            // pNext 链上的结构体在调用期间有效。
            unsafe {
                let mut p2 = Properties2 {
                    s_type: STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2,
                    p_next: (&mut id as *mut IdProperties).cast(),
                    properties: std::mem::zeroed(),
                };
                props2(dev, &mut p2);
            }
            let pci_id = pci_ext.then(|| pci_id(pci.domain, pci.bus, pci.device, pci.function));
            (Some(id.device_uuid), pci_id)
        } else {
            (None, None)
        };
        out.push(PhysicalDevice { name, pci_id, uuid });
    }
    Ok(out)
}

/// ggml 的写法（见模块说明）；function 只取低 8 位。
fn pci_id(domain: u32, bus: u32, device: u32, function: u32) -> String {
    format!("{domain:04x}:{bus:02x}:{device:02x}.{:x}", function as u8)
}

fn has_pci_bus_info(dev: Handle, exts: EnumerateExtensions) -> Result<bool, Error> {
    let mut count = 0u32;
    // SAFETY: 物理设备句柄有效；不指定层，先只取数量。
    let res = unsafe { exts(dev, ptr::null(), &mut count, ptr::null_mut()) };
    if res != SUCCESS {
        return Err(err(format!("无法列出设备扩展（VkResult {res}）")));
    }
    // SAFETY: 全零是 `ExtensionProperties` 的合法值。
    let mut list: Vec<ExtensionProperties> =
        (0..count).map(|_| unsafe { std::mem::zeroed() }).collect();
    // SAFETY: 缓冲区可容纳 `count` 项，加载器最多写入这么多并更新 `count`。
    let res = unsafe { exts(dev, ptr::null(), &mut count, list.as_mut_ptr()) };
    if res != SUCCESS && res != INCOMPLETE {
        return Err(err(format!("无法列出设备扩展（VkResult {res}）")));
    }
    list.truncate(count as usize);
    Ok(list.iter().any(|e| {
        // SAFETY: 扩展名是以空字符结尾的字符串。
        let name = unsafe { CStr::from_ptr(e.name.as_ptr()) };
        name == c"VK_EXT_pci_bus_info"
    }))
}

#[cfg(test)]
mod tests {
    use std::mem::offset_of;

    use super::*;

    #[test]
    fn layouts() {
        // 与 vulkan_core.h 的布局一致（64 位）。
        assert_eq!(offset_of!(ApplicationInfo, api_version), 44);
        assert_eq!(size_of::<InstanceCreateInfo>(), 64);
        assert_eq!(offset_of!(Properties, device_name), 20);
        assert_eq!(offset_of!(Properties, rest), 296);
        // 实际的 VkPhysicalDeviceProperties 为 824 字节。
        assert!(size_of::<Properties>() >= 824);
        assert_eq!(offset_of!(Properties2, properties), 16);
        assert_eq!(size_of::<PciBusInfo>(), 32);
        assert_eq!(offset_of!(IdProperties, device_luid_valid), 60);
        assert_eq!(size_of::<IdProperties>(), 64);
        assert_eq!(size_of::<ExtensionProperties>(), 260);
    }

    #[test]
    fn pci_format() {
        assert_eq!(pci_id(0, 6, 0, 0), "0000:06:00.0");
        assert_eq!(pci_id(0x1a, 0xab, 0x1f, 7), "001a:ab:1f.7");
    }
}
