//! 推理子进程的监督：只在调度线程中使用，一次只处理一个请求。
//! - 启动：第一次请求时拉起；可见的 GPU 不符时换一个推理进程重试一次（`retry_mismatch`）。遇到图像、音频或视频而当前进程
//!   没有加载 mmproj 时结束它并带上 mmproj 重新拉起；空闲卸载（`unload`）与这种重启都是主动的，不计入连续失败次数。
//! - 失败（`fail`）：CPU 模式连续失败 `MAX_FAILURES` 次后判失效；GPU 模式任何失败都结束进程且不再拉起，
//!   例外是本次请求期间 stderr 中有显存或内存分配失败（单个输入过大），连续 `MAX_FAILURES` 次才判为显存不足
//!   （`Error::OutOfMemory`）。设备丢失返回 `Error::DeviceLost`，找不到显卡返回 `Error::GpuNotFound`，
//!   之后的请求都直接返回失效原因（`Dead`）。
//! - 结束推理进程（`stop`）最多等 `KILL_GRACE`：结束不了时记为 `Dead::Stuck`（GPU、CPU 模式都是），不再拉起。
//! - 唤醒后验证用 `restart` 清除失败次数与“不可用”状态；设备丢失、结束不了的状态保留。
use std::io::ErrorKind;
use std::process::ExitStatus;
use std::sync::{Arc, Mutex};

use rummage_llama::PATCH_PIXELS;
use rummage_media::video::VideoPart;

use super::process::{Proc, Recv};
use super::{
    ERROR_LINES, Limits, MAX_AUDIO_SAMPLES, Status, WorkerSpec, device_lost, lock, out_of_memory,
    prefixed, retry_mismatch, video_request,
};
use crate::frame::{self, FromWorker, ToWorker};
use crate::space::{SpaceConfig, SpaceId};
use crate::{Embedding, Error, Input, KILL_GRACE, MAX_FAILURES, prefix, vector};

/// 推理引擎不再可用的原因；之后的请求都直接返回它，不再拉起推理进程。
pub(super) enum Dead {
    DeviceLost(String),
    Unavailable(String),
    /// 推理进程找不到选中的显卡
    Gone(String),
    /// GPU 模式连续 `MAX_FAILURES` 次显存或内存分配失败
    OutOfMemory(String),
    /// 推理进程结束不了（`stop`）
    Stuck(String),
}

impl Dead {
    fn error(&self) -> Error {
        match self {
            Dead::DeviceLost(detail) => Error::DeviceLost(detail.clone()),
            Dead::Unavailable(reason) => Error::Unavailable(reason.clone()),
            Dead::Gone(msg) => Error::GpuNotFound(msg.clone()),
            Dead::OutOfMemory(msg) => Error::OutOfMemory(msg.clone()),
            Dead::Stuck(msg) => Error::Stuck(msg.clone()),
        }
    }
}

pub(super) struct Supervisor {
    pub(super) spec: WorkerSpec,
    pub(super) limits: Limits,
    pub(super) proc: Option<Proc>,
    /// 连续失败次数（GPU 模式只计显存或内存分配失败），成功一次清零。
    pub(super) failures: u32,
    pub(super) dead: Option<Dead>,
    pub(super) status: Arc<Mutex<Status>>,
    /// 当前的推理进程加载了 mmproj。
    pub(super) with_mmproj: bool,
}

impl Supervisor {
    /// 结束推理进程并读完 stderr；返回退出状态与 stderr 末尾。没有运行中的进程时 stderr 为空。
    /// `KILL_GRACE` 内结束不了时不再等它，记为 `Dead::Stuck`（由上层屏蔽该显卡）
    fn stop(&mut self) -> (Option<ExitStatus>, String) {
        {
            let mut s = lock(&self.status);
            s.pid = None;
            s.job = None;
        }
        let Some(mut p) = self.proc.take() else {
            return (None, String::new());
        };
        let (status, tail, ended) = p.kill(KILL_GRACE);
        if !ended {
            let secs = KILL_GRACE.as_secs();
            self.dead = Some(Dead::Stuck(format!("推理进程在 {secs} 秒内没有结束。")));
            log::error!("推理进程在 {secs} 秒内没有结束，不再等它。");
        }
        (status, tail)
    }

    /// 处理一次失败。`kill` 表示推理进程已不可用（超时、崩溃、协议错误），必须结束。
    /// - stderr 或错误信息中有设备丢失：结束进程，返回 `DeviceLost`，本次运行不再使用该设备。
    /// - GPU 模式：任何失败都结束进程且不再拉起，返回 `Unavailable`。超时很可能就是 TDR，
    ///   重启会再次初始化显卡。推理出错或崩溃、且本次请求的 stderr 中有显存或内存
    ///   分配失败时例外：结束进程但不判失效，返回 `Inference`，下一次请求重新拉起；连续 `MAX_FAILURES` 次才判为显存不足
    ///   （`OutOfMemory`）。
    /// - 推理进程结束不了（`stop`）：返回 `Stuck`。
    /// - CPU 模式：记一次连续失败，达到 `MAX_FAILURES` 次后结束进程，之后返回 `Unavailable`。
    pub(super) fn fail(&mut self, err: Error, kill: bool) -> Error {
        let gpu = self.spec.gpu.is_some();
        // 已就绪才是处理某个请求时出的错；启动时分配失败与输入无关，按启动失败处理
        let ready = lock(&self.status).pid.is_some();
        let (status, tail) = if kill || gpu {
            self.stop()
        } else {
            (
                None,
                self.proc.as_ref().map_or_else(String::new, Proc::tail),
            )
        };
        log::warn!("推理失败：{err}；退出状态 {status:?}；stderr 末尾：\n{tail}");
        // 推理进程结束不了（`stop`）
        if let Some(dead @ Dead::Stuck(_)) = &self.dead {
            return dead.error();
        }
        let mut last: Vec<&str> = tail.lines().rev().take(ERROR_LINES).collect();
        last.reverse();
        let last = last.join("\n");
        if device_lost(&tail) || device_lost(&err.to_string()) {
            self.stop();
            if let Some(dead @ Dead::Stuck(_)) = &self.dead {
                return dead.error();
            }
            let detail = if last.is_empty() {
                err.to_string()
            } else {
                last
            };
            self.dead = Some(Dead::DeviceLost(detail));
        } else if let Error::GpuNotFound(msg) = err {
            self.dead = Some(Dead::Gone(msg));
        } else if gpu
            && ready
            && matches!(err, Error::Inference(_) | Error::Crashed(_))
            && out_of_memory(&tail)
        {
            // 单个输入用尽了显存或内存，显卡本身没有出错：不改用 CPU，下一次请求重新拉起推理进程。
            // 超时仍按 TDR 处理；连续这样失败 MAX_FAILURES 次（如显存一直被占满）时判为显存不足，免得每个文件都重新初始化显卡，
            // 与驱动无关，上层换下一块显卡
            self.failures += 1;
            if self.failures < MAX_FAILURES {
                return Error::Inference(crate::OUT_OF_MEMORY.to_string());
            }
            self.dead = Some(Dead::OutOfMemory(format!(
                "连续 {MAX_FAILURES} 次显存或内存分配失败"
            )));
        } else if gpu {
            self.dead = Some(Dead::Unavailable(format!(
                "显卡后端失效，本次运行不再使用它（{err}）"
            )));
            // 数值异常是确定的结果，原样返回：自检据此记为“未通过”，不当作驱动错误
            if matches!(err, Error::BadVector(_)) {
                return err;
            }
        } else {
            self.failures += 1;
            if self.failures >= MAX_FAILURES {
                self.stop();
                if let Some(dead @ Dead::Stuck(_)) = &self.dead {
                    return dead.error();
                }
                self.dead = Some(Dead::Unavailable(format!(
                    "推理进程连续失败 {MAX_FAILURES} 次。"
                )));
            }
            return match err {
                Error::Crashed(msg) => {
                    let code = status.map_or("未知".to_string(), |s| s.to_string());
                    Error::Crashed(format!("{msg}（{code}）。\n{last}"))
                }
                err => err,
            };
        }
        self.dead.as_ref().expect("上面已设置").error()
    }

    fn start(&mut self, mmproj: bool) -> Result<(), Error> {
        let res = retry_mismatch(|| self.spawn(mmproj));
        res.map_err(|err| self.fail(err, true))
    }

    /// 启动推理进程并等它就绪。出错时进程留给 `fail` 结束并读取 stderr；可见的 GPU 不符时在这里结束，以便重试。
    fn spawn(&mut self, mmproj: bool) -> Result<(), Error> {
        // 上一个推理进程结束不了（`stop`）时不再拉起
        if let Some(dead) = &self.dead {
            return Err(dead.error());
        }
        let args = self.spec.args(mmproj);
        let proc = Proc::spawn(&self.spec.exe, args, self.limits.mem_bytes)?;
        self.with_mmproj = mmproj && self.spec.mmproj.is_some();
        let pid = proc.child.id();
        lock(&self.status).job = Some(proc.job.clone());
        let first = proc.recv(self.limits.init_timeout);
        self.proc = Some(proc);
        match first {
            Recv::Frame(FromWorker::Ready { device }, _) => {
                let mut s = lock(&self.status);
                s.pid = Some(pid);
                s.device = Some(device);
                Ok(())
            }
            Recv::Frame(
                FromWorker::Error {
                    message,
                    not_found: true,
                    ..
                },
                _,
            ) => Err(Error::GpuNotFound(message)),
            Recv::Frame(
                FromWorker::Error {
                    message,
                    mismatch: true,
                    ..
                },
                _,
            ) => {
                let (_, tail) = self.stop();
                log::warn!("{message}\n{tail}");
                Err(Error::GpuMismatch(message))
            }
            Recv::Frame(FromWorker::Error { message, .. }, _) => Err(Error::InitFailed(message)),
            Recv::Frame(other, _) => Err(Error::Protocol(format!("启动时收到 {other:?}"))),
            Recv::Timeout => Err(Error::InitTimeout(self.limits.init_timeout)),
            Recv::Closed(msg) => Err(Error::Crashed(msg)),
        }
    }

    /// 确保推理进程在运行：已失效时返回原因；进程已退出时按失败处理后重新拉起（GPU 模式不再拉起）。
    /// `mmproj` 为真时需要加载了 mmproj 的进程：当前进程没有加载就结束它重新拉起，不计失败。
    pub(super) fn ensure_started(&mut self, mmproj: bool) -> Result<(), Error> {
        if let Some(dead) = &self.dead {
            return Err(dead.error());
        }
        // 进程已经退出（崩溃或被结束）：CPU 模式记一次失败，本次请求重新拉起；GPU 模式不再拉起。
        if let Some(p) = &mut self.proc
            && !matches!(p.child.try_wait(), Ok(None))
        {
            let err = self.fail(Error::Crashed("推理进程已退出".to_string()), true);
            if self.dead.is_some() {
                return Err(err);
            }
        }
        if mmproj && self.proc.is_some() && !self.with_mmproj {
            log::info!("遇到图像或音频，带上多模态模型重启推理进程。");
            self.stop();
            if let Some(dead) = &self.dead {
                return Err(dead.error());
            }
        }
        if self.proc.is_none() {
            self.start(mmproj)?;
        }
        Ok(())
    }

    /// 空闲超时：结束推理进程，不计失败；下一次请求重新拉起，只加载主模型。
    pub(super) fn unload(&mut self) {
        log::info!("推理引擎空闲超时，结束推理进程。");
        self.stop();
        self.with_mmproj = false;
    }

    /// 结束推理进程并清除失败次数与“不可用”“找不到显卡”状态；设备丢失保留，不再拉起（见模块说明）。
    pub(super) fn restart(&mut self) {
        self.stop();
        self.with_mmproj = false;
        self.failures = 0;
        if matches!(self.dead, Some(Dead::Unavailable(_) | Dead::Gone(_))) {
            self.dead = None;
        }
    }

    /// 发送一个请求并等待回复。超时、崩溃、推理出错都在这里按失败处理，返回的是 `Vector` 或 `Counted` 帧。
    fn send(&mut self, req: &ToWorker, payload: &[u8]) -> Result<(FromWorker, Vec<u8>), Error> {
        self.ensure_started(matches!(
            req,
            ToWorker::Image { .. } | ToWorker::Audio | ToWorker::Video { .. }
        ))?;
        let proc = self.proc.as_mut().expect("上面已启动推理进程");
        // 失败原因只看本次请求期间的 stderr：之前可恢复的分配失败警告（如 pinned 内存分配失败后改用普通内存）不算在这次头上
        lock(&proc.tail).clear();
        let stdin = proc.stdin.as_mut().expect("运行中的推理进程持有 stdin");
        // 写 stdin 不会卡住：子进程空闲时总在读下一帧，父进程一次只发一个请求、收到回复或结束进程后才发下一个，
        // 所以写入时对端一定在读；子进程已退出时写入立即返回管道错误。
        if let Err(err) = frame::write(stdin, req, payload) {
            // 超限时什么都没写，进程仍可用。
            if err.kind() == ErrorKind::InvalidInput {
                return Err(Error::InvalidInput(err.to_string()));
            }
            return Err(self.fail(Error::Crashed(format!("无法发送请求：{err}")), true));
        }
        match proc.recv(self.limits.infer_timeout) {
            Recv::Frame(msg @ (FromWorker::Vector | FromWorker::Counted { .. }), bytes) => {
                Ok((msg, bytes))
            }
            // 输入本身无效（含视频窗口超过 token 上限），与后端是否正常无关。
            Recv::Frame(
                FromWorker::Error {
                    message,
                    input: true,
                    too_long,
                    ..
                },
                _,
            ) => Err(if too_long {
                Error::TooLong(message)
            } else {
                Error::InvalidInput(message)
            }),
            Recv::Frame(FromWorker::Error { message, .. }, _) => {
                Err(self.fail(Error::Inference(message), false))
            }
            Recv::Frame(other, _) => {
                Err(self.fail(Error::Protocol(format!("推理时收到 {other:?}")), true))
            }
            Recv::Timeout => Err(self.fail(Error::InferTimeout(self.limits.infer_timeout), true)),
            Recv::Closed(msg) => Err(self.fail(Error::Crashed(msg), true)),
        }
    }

    /// 返回经过数值检查、已归一化的向量。
    fn infer(&mut self, req: &ToWorker, payload: &[u8]) -> Result<Vec<f32>, Error> {
        let (msg, bytes) = self.send(req, payload)?;
        if msg != FromWorker::Vector {
            return Err(self.fail(Error::Protocol(format!("推理时收到 {msg:?}")), true));
        }
        let Some(raw) = frame::f32_from_bytes(&bytes) else {
            let err = Error::Protocol("向量的字节数不是 4 的倍数".to_string());
            return Err(self.fail(err, true));
        };
        match vector::normalize(raw) {
            Ok(v) => {
                self.failures = 0;
                Ok(v)
            }
            // 不合格的向量计入连续失败。
            Err(err) => Err(self.fail(err, false)),
        }
    }

    fn check_image(&self, rgb: &rummage_media::Rgb, tokens: u32) -> Result<(), Error> {
        if self.spec.mmproj.is_none() {
            return Err(Error::NoMmproj);
        }
        let n = u64::from(rgb.width / PATCH_PIXELS) * u64::from(rgb.height / PATCH_PIXELS);
        if n > u64::from(tokens) {
            return Err(Error::InvalidInput(format!(
                "图像 {}×{} 有 {n} 个视觉 token，超过 {tokens} 档。",
                rgb.width, rgb.height
            )));
        }
        Ok(())
    }

    pub(super) fn count(&mut self, input: &Input) -> Result<usize, Error> {
        let text = prefixed(input).expect("`LocalBackend::count_tokens` 已挡下文本以外的输入");
        let (msg, _) = self.send(&ToWorker::Count { text }, &[])?;
        let FromWorker::Counted { tokens } = msg else {
            return Err(self.fail(Error::Protocol(format!("计数时收到 {msg:?}")), true));
        };
        // 只分词不推理，成功不能说明推理正常，不清零连续失败次数。
        Ok(tokens)
    }

    pub(super) fn handle(
        &mut self,
        input: Input,
        text_space: &SpaceId,
        cfg: &SpaceConfig,
    ) -> Result<Embedding, Error> {
        // 文字查询，或带文字描述的以图搜图
        let query = matches!(&input, Input::Query(_))
            || matches!(&input, Input::ImageQuery { text, .. } if !text.trim().is_empty());
        let (req, payload, space) = match input {
            Input::Image { rgb, tokens } => {
                self.check_image(&rgb, tokens)?;
                let req = ToWorker::Image {
                    width: rgb.width,
                    height: rgb.height,
                };
                // 档位决定向量，向量属于该档位的图像空间；长图切片的档位可以与整图不同。
                (req, rgb.data, cfg.image(tokens))
            }
            Input::ImageQuery { rgb, tokens, text } => {
                self.check_image(&rgb, tokens)?;
                let space = cfg.image(tokens);
                if text.trim().is_empty() {
                    let req = ToWorker::Image {
                        width: rgb.width,
                        height: rgb.height,
                    };
                    (req, rgb.data, space)
                } else {
                    // 图文交错的输入与视频窗口走同一条路径（文本块 + 媒体块，一个序列），只受上下文上限约束
                    let parts = [
                        VideoPart::Text(prefix::query(&text)),
                        VideoPart::Frame(Arc::new(rgb)),
                    ];
                    let (req, payload) = video_request(&parts, usize::MAX)?;
                    (req, payload, space)
                }
            }
            Input::Audio {
                samples,
                preprocess,
            } => {
                if self.spec.mmproj.is_none() {
                    return Err(Error::NoMmproj);
                }
                if samples.is_empty() || samples.len() > MAX_AUDIO_SAMPLES {
                    return Err(Error::InvalidInput(format!(
                        "音频有 {} 个采样，应在 1 到 {MAX_AUDIO_SAMPLES} 之间。",
                        samples.len()
                    )));
                }
                // 音频不加前缀
                (
                    ToWorker::Audio,
                    frame::f32_bytes(&samples),
                    cfg.media(&preprocess),
                )
            }
            Input::Video {
                parts,
                preprocess,
                max_tokens,
            } => {
                if self.spec.mmproj.is_none() {
                    return Err(Error::NoMmproj);
                }
                let (req, payload) = video_request(&parts, max_tokens)?;
                (req, payload, cfg.media(&preprocess))
            }
            text => {
                let text = prefixed(&text).expect("图像、音频与视频以外的输入都是文本");
                (ToWorker::Text { text }, Vec::new(), text_space.clone())
            }
        };
        // 查询的输入错误基本只会是超过上下文长度（以图搜图的文字描述过长时推理进程报告超过 token 上限）或含 NUL。
        // 给用户的整句与文件内容的不同
        let vector = self.infer(&req, &payload).map_err(|err| match err {
            Error::InvalidInput(msg) | Error::TooLong(msg) if query => Error::QueryTooLong(msg),
            err => err,
        })?;
        Ok(Embedding { vector, space })
    }
}
