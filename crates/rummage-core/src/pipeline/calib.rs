//! 分数校准的后台计算：专用线程每 `CHECK` 检查一次，有“空间 × 模态”需要（重新）计算背景分布时
//! 交给 `rummage_index::calib::update`（何时重算见该模块）。检查只读内存向量索引的计数，代价很小。
//! - 文字查询的背景要用当前推理引擎的文本空间：后台检查只用引擎槽中已有的引擎，不为此新建（避免空闲时加载模型）；
//!   没有引擎时这一轮只算以图搜图的背景。通用查询的向量只在文本空间变化后算一次，
//!   以 `Priority::Index` 排队，查询照常插队。
//! - 暂停建引擎期间（引导、设备检测、模型下载、清空索引）不计算；手动暂停、电池等暂停条件不影响，计算量很小。
//! - 推理出现设备丢失或后端失效时与建索引一样处理（`Shared::engine_failed`），下一轮换用新引擎再算。
//! - `Pipeline::calibrate` 立即算一遍（需要时新建引擎），评测在跑查询前调用；与后台线程互斥，不会同时计算。
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::time::Duration;

use rummage_embed::{Error as EmbedError, Input, Priority};
use rummage_index::calib::{self, CalibError, Rng, TextQueries};

use super::{Engine, Msg, Pipeline, Shared, lock};

/// 后台检查的间隔
const CHECK: Duration = Duration::from_secs(60);

pub(super) fn run(shared: &Shared, out: &Sender<Msg>) {
    loop {
        {
            let stopped = lock(&shared.calib_stop);
            if !*stopped {
                drop(
                    shared
                        .calib_cv
                        .wait_timeout(stopped, CHECK)
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                );
            }
        }
        if shared.stop.load(Ordering::Relaxed) {
            return;
        }
        if shared.held() {
            continue;
        }
        match shared.calibrate(false, out) {
            Ok(0) => {}
            Ok(n) => log::info!("更新了 {n} 组分数校准的背景分布。"),
            Err(err) => log::warn!("无法计算分数校准的背景分布：{err}"),
        }
    }
}

impl Shared {
    /// 算一遍需要更新的背景分布；`create` 为真时没有引擎就新建一个
    fn calibrate(&self, create: bool, out: &Sender<Msg>) -> Result<usize, CalibError> {
        let _serial = lock(&self.calib_lock);
        let engine: Option<Arc<Engine>> = if create {
            self.engine(out)
        } else {
            lock(&self.engine).clone()
        };
        let mut rng = Rng::from_time();
        let Some(e) = engine.filter(|e| !e.failed.load(Ordering::Relaxed)) else {
            return calib::update(&self.db, &self.index, None, &mut rng);
        };
        let start = self.start();
        let mut failure: Option<EmbedError> = None;
        let mut embed = |q: &str| {
            e.backend
                .embed(Input::Query(q.to_string()), Priority::Index)
                .map(|v| v.vector)
                .map_err(|err| {
                    let msg = err.to_string();
                    failure = Some(err);
                    msg
                })
        };
        let text = TextQueries {
            space: &e.text_space,
            embed: &mut embed,
        };
        let res = calib::update(&self.db, &self.index, Some(text), &mut rng);
        if let Some(err) = failure {
            self.engine_failed(&e, &err, start);
        }
        res
    }
}

impl Pipeline {
    /// 立即算一遍需要更新的背景分布（评测用），返回更新的个数
    pub fn calibrate(&self) -> Result<usize, String> {
        self.shared
            .calibrate(true, &self.out)
            .map_err(|err| err.to_string())
    }
}
