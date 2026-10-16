//! 自检结果的缓存与显卡的屏蔽、未通过记录，都存在 `<数据目录>\cache\selftest.json`、用同一套键。
//! - 缓存是否可用、是否过期只按设备身份（描述与 PCI 号，`DeviceKey::same_device`）、运行库与模型判断，**不看驱动版本**：
//!   llama.cpp 在 Vulkan 上的问题多由它自身的版本或特定硬件引起，驱动导致的多为崩溃，已由子进程与屏蔽兜住。
//!   显卡有增减或更换（DXGI 列出的适配器，`gpu_adapters`，同型号的按数量比较）、运行库或模型变化、要处理图片而上次只测了文本，
//!   或有暂时性失败、被跳过的显卡（下次检测时重测）时才算过期。
//!   屏蔽与未通过记录仍按含驱动版本的完整键匹配，驱动或运行库更新后用户手动检测时可以再试；自动重新检测时仍跳过屏蔽过的显卡
//!   （`Options::auto`）。运行中屏蔽时检测时与现在的驱动版本不同则两个键都写（`block`），驱动再次更新后自动选用也不会选回它，
//!   等用户手动检测。
//! - 设备相同、且测过本次需要的模态时直接用缓存的结果；只测了文本的结果不能用来处理图片、音频或视频（`Cached::covers`）。
//!   自动检测按设备沿用仍然有效的结果（`previous`）。
//! - 建索引只用 `cached_gpu` / `preferred_gpu` 读记录，不测试也不初始化任何 GPU；按性能顺序选第一个能用的设备，
//!   本次运行停用的显卡由调用方传入（`disabled`），按同一设备（描述与 PCI 号）比较，不看驱动版本：外接显卡拔出时
//!   查不到驱动，插回后本次运行中也不选回。
//! - 用户在设备页解除屏蔽（`unblock_gpu`）时删除这块显卡的全部屏蔽记录（不论驱动与运行库版本），缓存的结果中它改为“已跳过”，
//!   之后的检测（核心解除后随即发起的重测，或下次手动、自动检测）只重测它；手动检测中通过时同样删除。“重新检测”不清除其他记录。
//! - 文件的读改写（自检保存结果、建索引时屏蔽、解除屏蔽）都在 `CACHE_LOCK` 内进行；先写临时文件再改名。
use std::fs;
use std::io::{self, ErrorKind};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};

use super::{
    CandidateReport, DeviceKey, SelftestReport, Status, UNKNOWN_DRIVER, by_speed, gpu_id, models,
    select,
};
use crate::space::{self, Backend};
use crate::{Adapter, platform};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Cached {
    pub(super) keys: Vec<DeviceKey>,
    pub(super) report: SelftestReport,
    /// 测了图像、音频与视频样本（带 mmproj）
    pub(super) mmproj: bool,
    /// 检测时系统中的显卡（`gpu_adapters`），与现在不同就算过期
    pub(super) adapters: Vec<String>,
}

impl Cached {
    /// 测过本次需要的样本：`mmproj` 为是时要测过图像、音频与视频
    fn covers(&self, mmproj: bool) -> bool {
        !mmproj || self.mmproj
    }

    /// 描述为 `description` 的显卡现在仍在系统中（`adapters` 为现在的 `gpu_adapters`）。检测时 DXGI 中就找不到同名的
    /// （描述与 Vulkan 不一致）时无法判断是否拔出，算作在：拔出后由流水线按找不到显卡改用下一个
    fn present(&self, adapters: &[String], description: &str) -> bool {
        adapters.iter().any(|a| a == description) || !self.adapters.iter().any(|a| a == description)
    }
}

/// 记录的种类（见模块说明的三级规则）；两种都使该显卡不再自动测试或选用
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Record {
    /// 设备丢失或有驱动事件：持久屏蔽
    Blacklisted,
    /// 余弦不达标、NaN 等确定的结果
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct Blacklisted {
    pub(super) key: DeviceKey,
    pub(super) reason: String,
    pub(super) kind: Record,
}

/// `<数据目录>\cache\selftest.json` 的内容。
#[derive(Debug, Default, Serialize, Deserialize)]
pub(super) struct CacheFile {
    pub(super) cached: Option<Cached>,
    pub(super) blacklist: Vec<Blacklisted>,
}

impl CacheFile {
    /// 文件不存在或内容无效时返回空记录。
    pub(super) fn load(path: &Path) -> Self {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(err) if err.kind() == ErrorKind::NotFound => return CacheFile::default(),
            Err(err) => {
                log::warn!("无法读取自检记录 {}：{err}", path.display());
                return CacheFile::default();
            }
        };
        serde_json::from_slice(&bytes).unwrap_or_else(|err| {
            log::warn!("自检记录 {} 无效，已忽略：{err}", path.display());
            CacheFile::default()
        })
    }

    /// 先写临时文件再改名，写到一半退出也不会留下损坏的记录。
    pub(super) fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        fs::write(&tmp, json)?;
        fs::rename(&tmp, path)
    }

    /// 自动检测可以沿用的这个设备上次的结果（见上级模块说明）：同一设备（`same_device`，运行库与模型也相同）上次测过、
    /// 不是暂时性失败或被跳过的，且测过本次需要的模态（CPU 也是）
    pub(super) fn previous(&self, key: &DeviceKey, images: bool) -> Option<CandidateReport> {
        let c = self.cached.as_ref().filter(|c| c.covers(images))?;
        let i = c.keys.iter().position(|k| k.same_device(key))?;
        let r = c.report.candidates.get(i)?;
        (!r.transient && r.status != Status::Skipped).then(|| r.clone())
    }

    /// 设备相同（不看驱动版本）、且测过本次需要的模态时返回上次的结果
    pub(super) fn cached_report(&self, keys: &[DeviceKey], images: bool) -> Option<SelftestReport> {
        self.cached
            .as_ref()
            .filter(|c| {
                c.keys.len() == keys.len()
                    && c.keys.iter().zip(keys).all(|(a, b)| a.same_device(b))
                    && c.covers(images)
            })
            .map(|c| c.report.clone())
    }

    /// 该键的屏蔽或未通过记录
    pub(super) fn blacklisted(&self, key: &DeviceKey) -> Option<&Blacklisted> {
        self.blacklist.iter().find(|b| &b.key == key)
    }

    /// 同一块显卡（不论驱动与运行库版本）有过屏蔽记录：自动重新检测不测它（驱动或运行库更新后重测须征得用户同意）
    pub(super) fn ever_blacklisted(&self, key: &DeviceKey) -> bool {
        self.blacklist.iter().any(|b| {
            b.kind == Record::Blacklisted
                && b.key.description == key.description
                && b.key.pci_id == key.pci_id
        })
    }

    /// 记录一次屏蔽或未通过；同一个键已有记录时更新，但屏蔽不降为未通过（屏蔽的证据更重）
    pub(super) fn record(&mut self, key: DeviceKey, kind: Record, reason: &str) {
        match self.blacklist.iter_mut().find(|b| b.key == key) {
            Some(b) if b.kind == Record::Blacklisted && kind == Record::Failed => {}
            Some(b) => {
                b.kind = kind;
                b.reason = reason.to_string();
            }
            None => self.blacklist.push(Blacklisted {
                key,
                reason: reason.to_string(),
                kind,
            }),
        }
    }

    /// 运行中屏蔽 `gpu`：按现在的键（`now`，生产中为 `report_key`）记录；与检测时的键（报告中记下的驱动版本）不同时
    /// 两个都记。只记现在的键时，驱动从检测时的 D1 升到 D2 时出错、再升到 D3 后，检测时的键与 D3 都不在记录中，
    /// 自动选用会直接选回它；记下 D1 后它要等用户手动检测才重测
    pub(super) fn block(&mut self, gpu: &CandidateReport, now: DeviceKey, reason: &str) {
        let then = key_at(gpu, gpu.driver.clone());
        if then != now {
            self.record(then, Record::Blacklisted, reason);
        }
        self.record(now, Record::Blacklisted, reason);
    }

    /// `device` 指定的、上次检测通过的显卡；`device` 为空时按性能顺序（`by_speed`）取第一个能用的设备，是 CPU 时为空。
    /// 显卡能用：没有被屏蔽（检测时的键与现在的 `report_key` 都查），运行库与模型与当时相同，检测时在的现在仍在（`adapters` 为现在的 `gpu_adapters`，拔出后没有它），
    /// 不是本次运行停用的 `disabled` 中的设备（同一设备即算，不看驱动版本）。要处理图片而上次只测了文本时为空。
    /// 缓存是否可用不看驱动版本（见模块说明）
    pub(super) fn usable_gpu(
        &self,
        adapters: &[String],
        images: bool,
        device: Option<&str>,
        disabled: &[DeviceKey],
    ) -> Option<CandidateReport> {
        let cached = self.cached.as_ref().filter(|c| c.covers(images))?;
        let cands = &cached.report.candidates;
        let usable = |i: usize| {
            let (r, key) = (cands.get(i)?, cached.keys.get(i)?);
            let now = report_key(r);
            if r.backend != Backend::Vulkan
                || self.blacklisted(key).is_some()
                || self.blacklisted(&now).is_some()
                || disabled.iter().any(|d| d.same_device(&now))
            {
                return None;
            }
            let present = cached.present(adapters, &r.description);
            (key.runtime == space::RUNTIME && key.model == models() && present).then(|| r.clone())
        };
        if let Some(id) = device {
            let i = cands
                .iter()
                .position(|r| r.status == Status::Passed && r.device_id() == id)?;
            return usable(i);
        }
        by_speed(cands, cached.mmproj)
            .into_iter()
            .find_map(|i| match cands[i].backend {
                Backend::Cpu => Some(None),
                Backend::Vulkan => usable(i).map(Some),
            })
            .flatten()
    }
}

/// `selftest.json` 的读改写锁（见模块说明）
static CACHE_LOCK: Mutex<()> = Mutex::new(());

pub(super) fn cache_lock() -> MutexGuard<'static, ()> {
    CACHE_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 报告中某个候选现在的键，与现在检测时的键（`device_key`）相同：驱动版本、运行库与模型取当前值，
/// 报告是驱动更新前缓存的也一致。驱动版本查不到（如外接显卡已拔出）时用检测时记下的。
/// 运行中屏蔽时连同检测时的键一起写（`CacheFile::block`），驱动或运行库更新后要等用户手动检测才重测
pub fn report_key(r: &CandidateReport) -> DeviceKey {
    let now = match r.backend {
        Backend::Cpu => None,
        Backend::Vulkan => platform::driver_version(&r.description),
    };
    key_at(r, now.or_else(|| r.driver.clone()))
}

/// 报告中的候选在驱动版本为 `driver` 时的键；运行库与模型取当前值
pub(super) fn key_at(r: &CandidateReport, driver: Option<String>) -> DeviceKey {
    DeviceKey {
        description: match r.backend {
            Backend::Cpu => String::new(),
            Backend::Vulkan => r.description.clone(),
        },
        pci_id: r.pci_id.clone(),
        driver: match r.backend {
            Backend::Cpu => String::new(),
            Backend::Vulkan => driver.unwrap_or_else(|| UNKNOWN_DRIVER.to_string()),
        },
        runtime: space::RUNTIME.to_string(),
        model: models(),
    }
}

impl CacheFile {
    /// 缓存的结果没有过期（见模块说明）：测过本次需要的模态，每个键的运行库与模型不变，
    /// 系统中的显卡（`adapters`，现在的 `gpu_adapters`）与检测时相同，选中的显卡也没有被屏蔽；不看驱动版本。
    /// `cpu_only`（用户选了只用 CPU）时只核对 CPU 的运行库与模型，显卡有变化也不算。
    /// `retry` 时有暂时性失败或被跳过的显卡也算过期（下次检测时重测它们；本次运行中已检测过时调用方不再算）。
    /// 过期时核心在后台自动重新检测（`server::setup`）；实际用哪个设备由 `cached_gpu` 只按那一个设备判断
    pub(super) fn current(
        &self,
        adapters: &[String],
        images: bool,
        cpu_only: bool,
        retry: bool,
    ) -> bool {
        let Some(c) = self.cached.as_ref().filter(|c| c.covers(images)) else {
            return false;
        };
        let pending = c
            .report
            .candidates
            .iter()
            .any(|r| r.transient || r.status == Status::Skipped);
        if retry && !cpu_only && pending {
            return false;
        }
        let keys_ok = c.keys.iter().all(|k| {
            let cpu = k.description.is_empty() && k.driver.is_empty();
            (cpu_only && !cpu) || (k.runtime == space::RUNTIME && k.model == models())
        });
        // 检测时的键与现在的键都查（运行中屏蔽的两种键，见 `block`）
        let selected_ok = cpu_only
            || c.report
                .selected
                .and_then(|i| Some((c.keys.get(i)?, c.report.candidates.get(i)?)))
                .is_none_or(|(k, r)| {
                    self.blacklisted(k).is_none() && self.blacklisted(&report_key(r)).is_none()
                });
        keys_ok && selected_ok && (cpu_only || c.adapters == adapters)
    }
}

/// 系统中的显卡：DXGI 适配器的描述，排序，同型号的保留重复项（再加一块同型号的显卡也算变化）；不含微软的软件与
/// 远程桌面适配器（厂商 0x1414），它们随远程连接出现或消失，不是显卡变化。只读属性，不创建设备
pub fn gpu_adapters() -> Vec<String> {
    adapter_names(platform::adapters())
}

pub(super) fn adapter_names(adapters: Vec<Adapter>) -> Vec<String> {
    let mut v: Vec<String> = adapters
        .into_iter()
        .filter(|a| a.vendor_id != 0x1414)
        .map(|a| a.description)
        .collect();
    v.sort();
    v
}

/// 缓存中最近一次的自检结果是否没有过期（见 `CacheFile::current`）；没有结果时为否。
/// `adapters` 为现在系统中的显卡（生产中为 `gpu_adapters`，下同）
pub fn cache_current(
    cache_file: &Path,
    adapters: &[String],
    images: bool,
    cpu_only: bool,
    retry: bool,
) -> bool {
    CacheFile::load(cache_file).current(adapters, images, cpu_only, retry)
}

/// 按用户选择的设备（`auto`、`cpu` 或显卡的 `CandidateReport::device_id`）取建索引用的 GPU，为空时用 CPU：
/// 选了显卡但它不能用（没有通过检测、被屏蔽、已拔出或本次运行中停用）时按自动选择。`disabled` 为本次运行中停用的显卡
pub fn preferred_gpu(
    cache_file: &Path,
    adapters: &[String],
    images: bool,
    preferred: &str,
    disabled: &[DeviceKey],
) -> Option<CandidateReport> {
    if preferred == rummage_proto::CPU_DEVICE {
        return None;
    }
    if preferred != rummage_proto::AUTO_DEVICE {
        if let Some(r) = cached_gpu(cache_file, adapters, images, Some(preferred), disabled) {
            return Some(r);
        }
        log::warn!("所选的显卡（{preferred}）目前不能使用，改为自动选择。");
    }
    cached_gpu(cache_file, adapters, images, None, disabled)
}

/// 建索引时选用的 GPU：`device` 为用户指定、上次检测通过的显卡（标识见 `CandidateReport::device_id`），
/// 它能用时返回它；`device` 为空时按性能顺序取自检记录中第一个能用的设备，是显卡时返回它。能用：没有被屏蔽、
/// 运行库与模型没有变化、现在仍在系统中、不在本次运行停用的 `disabled` 中（不看驱动版本）。为空时由调用方用 CPU
/// 或改为自动选择。只读记录、用 DXGI 列出显卡，不测试也不初始化任何 GPU。
/// `images` 为是时（要加载 mmproj，处理图片、音频或视频），没有测过图像、音频与视频的记录不算。
pub fn cached_gpu(
    cache_file: &Path,
    adapters: &[String],
    images: bool,
    device: Option<&str>,
    disabled: &[DeviceKey],
) -> Option<CandidateReport> {
    CacheFile::load(cache_file).usable_gpu(adapters, images, device, disabled)
}

/// 缓存中的最近一次自检结果，不核对键（引导与设置页显示用）；没有时为空
pub fn last_report(cache_file: &Path) -> Option<SelftestReport> {
    CacheFile::load(cache_file).cached.map(|c| c.report)
}

/// 建索引时 `cached_gpu` 选中的 GPU 出现设备丢失、推理进程结束不了，或后端失效且这段时间系统日志中有驱动事件：
/// 立即按与自检相同的键写入屏蔽记录（检测时与现在的键见 `CacheFile::block`），`cached_gpu` 不再选它，
/// 用户手动检测且驱动或运行库已更新时，或解除屏蔽（`unblock_gpu`）后才重测。
pub fn blacklist_gpu(cache_file: &Path, gpu: &CandidateReport, reason: &str) -> io::Result<()> {
    let _guard = cache_lock();
    let mut cache = CacheFile::load(cache_file);
    cache.block(gpu, report_key(gpu), reason);
    cache.save(cache_file)
}

/// 被屏蔽的显卡（设备页列出，可以解除屏蔽）：同一设备（描述与 PCI 号）的多条屏蔽记录合为一项，原因取最后一条非空的
#[derive(Debug, Clone, PartialEq)]
pub struct BlockedGpu {
    pub description: String,
    pub pci_id: Option<String>,
    /// 记下的原因；没有时为空
    pub reason: Option<String>,
}

impl BlockedGpu {
    /// 与 `CandidateReport::device_id` 相同
    pub fn device_id(&self) -> String {
        gpu_id(&self.description, self.pci_id.as_deref())
    }
}

/// 设备页用的显卡记录：被屏蔽的显卡，以及最近一次检测中或被屏蔽的显卡中现在不在系统中的（`CandidateReport::device_id`）
#[derive(Debug, Default)]
pub struct GpuRecords {
    pub blocked: Vec<BlockedGpu>,
    pub absent: Vec<String>,
}

impl CacheFile {
    /// 检测完成、保存前合并本次的屏蔽与未通过记录。全部重测的手动检测（`manual`，见 `Options::manual`）中通过的显卡
    /// 视为用户同意解除屏蔽：删除它的屏蔽记录，不再列为已屏蔽（未通过记录不动）
    pub(super) fn merge(
        &mut self,
        records: Vec<(DeviceKey, Record, String)>,
        report: &SelftestReport,
        manual: bool,
    ) {
        for (key, kind, reason) in records {
            self.record(key, kind, &reason);
        }
        if !manual {
            return;
        }
        for r in report
            .candidates
            .iter()
            .filter(|r| r.backend == Backend::Vulkan && r.status == Status::Passed)
        {
            self.unblock(&r.device_id());
        }
    }

    /// 缓存中同一块设备（描述与 PCI 号，不看驱动、运行库与模型）上次的结果，原样返回（`Options::only` 时显示其余设备）
    pub(super) fn shown(&self, key: &DeviceKey) -> Option<CandidateReport> {
        let c = self.cached.as_ref()?;
        let i = c
            .keys
            .iter()
            .position(|k| k.description == key.description && k.pci_id == key.pci_id)?;
        c.report.candidates.get(i).cloned()
    }

    /// 只替换缓存中这一块设备的结果（`Options::only`），其余照旧；缓存中没有它时追加。本次没测图片与音视频时
    /// 整份缓存不再算测过（`mmproj`），需要时重新检测
    pub(super) fn replace(&mut self, key: DeviceKey, r: CandidateReport, images: bool) {
        let Some(c) = &mut self.cached else {
            return;
        };
        let i = c
            .keys
            .iter()
            .position(|k| k.description == key.description && k.pci_id == key.pci_id);
        match i.filter(|&i| i < c.report.candidates.len()) {
            Some(i) => {
                c.keys[i] = key;
                c.report.candidates[i] = r;
            }
            None => {
                c.keys.push(key);
                c.report.candidates.push(r);
            }
        }
        c.mmproj &= images;
        c.report.selected = select(&c.report.candidates, c.mmproj);
    }

    /// 原因取最后一条非空的
    pub(super) fn records(&self, adapters: &[String]) -> GpuRecords {
        let mut blocked: Vec<BlockedGpu> = Vec::new();
        for b in self
            .blacklist
            .iter()
            .filter(|b| b.kind == Record::Blacklisted)
        {
            let reason = Some(b.reason.trim().to_string()).filter(|r| !r.is_empty());
            match blocked
                .iter_mut()
                .find(|x| x.description == b.key.description && x.pci_id == b.key.pci_id)
            {
                Some(x) => x.reason = reason.or(x.reason.take()),
                None => blocked.push(BlockedGpu {
                    description: b.key.description.clone(),
                    pci_id: b.key.pci_id.clone(),
                    reason,
                }),
            }
        }
        // 被屏蔽的显卡也算（解除屏蔽时据此决定能否当场重测）；没有缓存时无从比较，算作在
        let absent = self.cached.as_ref().map_or_else(Vec::new, |c| {
            let gpus = c
                .report
                .candidates
                .iter()
                .filter(|r| r.backend == Backend::Vulkan)
                .map(|r| (r.device_id(), r.description.as_str()));
            let blocked = blocked
                .iter()
                .map(|b| (b.device_id(), b.description.as_str()));
            let mut v: Vec<String> = Vec::new();
            for (id, desc) in gpus.chain(blocked) {
                if !c.present(adapters, desc) && !v.contains(&id) {
                    v.push(id);
                }
            }
            v
        });
        GpuRecords { blocked, absent }
    }

    /// 解除屏蔽：删除这块显卡（`device` 为 `CandidateReport::device_id`，同一设备不论驱动与运行库版本）的全部屏蔽记录，
    /// 含运行中双键写入的两条与驱动版本未知的；未通过记录不动。缓存的结果中这块卡改为“已跳过”，
    /// 之后的检测会重测它（核心解除后随即只测它，`Options::only`），测之前不会被选用。没有屏蔽记录时返回否
    pub(super) fn unblock(&mut self, device: &str) -> bool {
        let before = self.blacklist.len();
        self.blacklist.retain(|b| {
            b.kind != Record::Blacklisted
                || gpu_id(&b.key.description, b.key.pci_id.as_deref()) != device
        });
        if self.blacklist.len() == before {
            return false;
        }
        if let Some(c) = &mut self.cached {
            for r in c
                .report
                .candidates
                .iter_mut()
                .filter(|r| r.backend == Backend::Vulkan && r.device_id() == device)
            {
                unblocked(r);
            }
            c.report.selected = select(&c.report.candidates, c.mmproj);
        }
        true
    }
}

/// 解除屏蔽后的检测结果：记为“已跳过”、不带耗时，下次检测时重测
pub(super) fn unblocked(r: &mut CandidateReport) {
    r.status = Status::Skipped;
    r.reason = Some(UNBLOCKED_REASON.to_string());
    r.samples.clear();
    (r.init_ms, r.text_ms, r.image_ms, r.audio_ms, r.video_ms) = (None, None, None, None, None);
}

/// 解除屏蔽后、重新检测前的原因
pub const UNBLOCKED_REASON: &str = "已解除屏蔽，尚未重新检测。";

/// 被屏蔽的显卡与现在不在系统中的显卡（见 `GpuRecords`）；`adapters` 为现在的 `gpu_adapters`
pub fn gpu_records(cache_file: &Path, adapters: &[String]) -> GpuRecords {
    CacheFile::load(cache_file).records(adapters)
}

/// 缓存的结果测过本次需要的模态（`images` 时要测过图片与音视频）；没有缓存时为是。解除屏蔽后只测一块
/// （`Options::only`）的前提：否则其余设备的结果不够用，要整体重新检测
pub fn cache_covers(cache_file: &Path, images: bool) -> bool {
    CacheFile::load(cache_file)
        .cached
        .as_ref()
        .is_none_or(|c| c.covers(images))
}

/// 用户在设备页解除屏蔽（见 `CacheFile::unblock`）；没有这块显卡的屏蔽记录时返回否、不写文件
pub fn unblock_gpu(cache_file: &Path, device: &str) -> io::Result<bool> {
    let _guard = cache_lock();
    let mut cache = CacheFile::load(cache_file);
    if !cache.unblock(device) {
        return Ok(false);
    }
    cache.save(cache_file)?;
    Ok(true)
}
