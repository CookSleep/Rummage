//! Android 上用 LiteRT 自检与建索引时的文件（`LiteRtFiles`）：按后端给出推理进程的规格（`spec`）。
//! NPU 用本机芯片的 NPU 模型；高通的分发库目录是下载的 QNN 运行库所在的目录，随应用分发的分发库先复制过去（`copy_dispatch`）。

use std::path::{Path, PathBuf};

use super::accel_name;
use crate::local::{Gpu, WorkerSpec};
use crate::space::Backend;
use crate::{Accel, Error, LiteRtArgs, npu};

/// Android 上用 LiteRT 自检时的文件（`Options::litert`）
#[derive(Debug, Clone)]
pub struct LiteRtFiles {
    /// `liblitert-lm.so` 与 GPU 加速器库所在的目录
    pub runtime_dir: PathBuf,
    pub tokenizer: PathBuf,
    /// 权重与程序缓存的目录
    pub cache_dir: PathBuf,
    /// 本机芯片的 NPU 模型与分发库目录；没有 NPU 候选时为空
    pub npu: Option<(PathBuf, PathBuf)>,
}

impl LiteRtFiles {
    /// 分发库目录不是运行库目录时（高通：与下载的 QNN 运行库放在一起），把随应用分发的分发库复制过去；大小相同时不复制
    fn copy_dispatch(&self, dir: &Path) -> Result<(), Error> {
        let Some(m) = npu::local() else {
            return Ok(());
        };
        if dir == self.runtime_dir {
            return Ok(());
        }
        let name = m.vendor.dispatch_lib();
        let (src, dst) = (self.runtime_dir.join(name), dir.join(name));
        let size = |p: &Path| std::fs::metadata(p).map(|m| m.len()).ok();
        if size(&src).is_some() && size(&src) == size(&dst) {
            return Ok(());
        }
        std::fs::create_dir_all(dir)
            .and_then(|()| std::fs::copy(&src, &dst))
            .map(|_| ())
            .map_err(|err| Error::InitFailed(format!("无法准备 NPU 的分发库 \"{name}\"：{err}")))
    }

    /// 后端 `b`（CPU、GPU 或 NPU）的推理进程规格：NPU 用 NPU 模型，其余用通用版 `model`；
    /// GPU、NPU 按 GPU 模式监督（`WorkerSpec::gpu` 为后端名）
    pub fn spec(
        &self,
        exe: &Path,
        model: &Path,
        b: Backend,
        images: bool,
        threads: u32,
    ) -> Result<WorkerSpec, Error> {
        let (model, accel) = match b {
            Backend::Npu => {
                let (npu, dir) = self
                    .npu
                    .as_ref()
                    .ok_or_else(|| Error::InitFailed("没有本机芯片的 NPU 模型。".to_string()))?;
                self.copy_dispatch(dir)?;
                (npu.clone(), Accel::Npu(dir.clone()))
            }
            Backend::Gpu => (model.to_path_buf(), Accel::Gpu),
            _ => (model.to_path_buf(), Accel::Cpu),
        };
        Ok(WorkerSpec {
            exe: exe.to_path_buf(),
            runtime_dir: self.runtime_dir.clone(),
            mmproj: images.then(|| model.clone()),
            model,
            threads,
            gpu: b.accel().then(|| Gpu {
                description: accel_name(b),
                pci_id: None,
            }),
            litert: Some(LiteRtArgs {
                accel,
                tokenizer: self.tokenizer.clone(),
                cache_dir: Some(self.cache_dir.clone()),
            }),
        })
    }
}
