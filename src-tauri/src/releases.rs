//! 版本列表：启动时的自动检查、设置“关于”页的检查更新与更新日志从 GitHub Releases API 读取版本列表，
//! 解析、缓存与比较版本都在前端（`src/lib/releaseUpdates.ts`）。请求由 Rust 侧发出，CSP 的 `connect-src` 不必放开外部地址。
//! 前端只给页码，地址由这里的常量拼出；重定向只跟随到 api.github.com 的仓库地址（仓库改名后 GitHub 会跳到 `/repositories/<id>/…`）。
//! 正文最多读 2 MB；仓库不存在（404）按没有发布的版本返回空列表。连接失败与 GitHub 的错误状态转换成中文整句。
//! 请求都走网络代理设置：每次从后台服务读取；读不到时报错，不绕过用户的代理设置直接联网。
use std::time::Duration;

use reqwest::{Client, StatusCode, Url, redirect};
use rummage_core::proxy::Route;
use rummage_proto::{ApiError, Reply, Request};
use tauri::{AppHandle, Runtime};

use crate::core;
use crate::search::{error, unexpected};

/// 与 `src/lib/releaseUpdates.ts` 的 `RELEASE_REPO` 保持一致
const RELEASE_REPO: &str = "CookSleep/Rummage";
/// 与 `src/lib/releaseUpdates.ts` 的 `RELEASES_PER_PAGE` 保持一致，前端据此判断是否还有下一页
const PER_PAGE: u32 = 10;
const MAX_BODY: usize = 2 << 20;
const USER_AGENT: &str = concat!("rummage/", env!("CARGO_PKG_VERSION"));
pub const NETWORK: &str = "network";

/// 读取后台服务中的代理设置
pub async fn proxy_route<R: Runtime>(app: &AppHandle<R>) -> Result<Route, ApiError> {
    match core::request(app, Request::SetupStatus).await {
        Ok(Reply::Setup(s)) => Ok(Route::from_setting(&s.proxy)),
        Ok(other) => Err(unexpected(&other)),
        Err(err) if err == core::unavailable() => Err(error(
            ApiError::UNAVAILABLE,
            "后台服务未连接，无法读取代理设置，请稍后再试。",
        )),
        Err(err) => Err(err),
    }
}

/// 读取 GitHub Releases API 的第 `page` 页（从 1 开始），返回响应正文
#[tauri::command]
pub async fn fetch_releases<R: Runtime>(app: AppHandle<R>, page: u32) -> Result<String, ApiError> {
    let url = Url::parse(&format!(
        "https://api.github.com/repos/{RELEASE_REPO}/releases?per_page={PER_PAGE}&page={}",
        page.max(1)
    ))
    .expect("常量拼出的地址总是合法的");
    let route = proxy_route(&app).await?;
    let policy = redirect::Policy::custom(|attempt| {
        let u = attempt.url();
        let ok = attempt.previous().len() < 5
            && u.scheme() == "https"
            && u.host_str() == Some("api.github.com")
            && (u.path().starts_with("/repos/") || u.path().starts_with("/repositories/"));
        if ok { attempt.follow() } else { attempt.stop() }
    });
    let builder = Client::builder()
        .user_agent(USER_AGENT)
        .redirect(policy)
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30));
    let client = route
        .apply(builder)
        .and_then(|b| b.build())
        .map_err(|err| {
            log::error!("无法创建检查更新用的网络客户端：{err}");
            error(NETWORK, "无法创建网络连接，请检查代理设置后重试。")
        })?;
    let connect_error = |err: reqwest::Error| {
        log::warn!("检查更新时网络出错：{err:?}");
        let msg = if route.proxies(&url) && err.is_connect() {
            "无法连接代理服务器，请检查代理设置。"
        } else if err.is_timeout() {
            "连接 GitHub 超时，请检查网络或代理设置。"
        } else if err.is_body() || err.is_decode() {
            "读取版本信息时连接中断，请重试。"
        } else {
            "无法连接 GitHub，请检查网络或代理设置。"
        };
        error(NETWORK, msg)
    };
    let mut res = client
        .get(url.clone())
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(connect_error)?;
    let status = res.status();
    // 仓库还不存在或尚未公开时没有任何发布的版本
    if status == StatusCode::NOT_FOUND {
        return Ok("[]".into());
    }
    // 403 也可能是其他原因（如被拒绝访问），只有剩余次数为 0 时才是限流
    let exhausted = res
        .headers()
        .get("x-ratelimit-remaining")
        .is_some_and(|v| v.as_bytes() == b"0");
    if status == StatusCode::TOO_MANY_REQUESTS || (status == StatusCode::FORBIDDEN && exhausted) {
        return Err(error(NETWORK, "GitHub 请求过于频繁，请稍后再试。"));
    }
    if !status.is_success() {
        log::warn!("检查更新时 GitHub 返回 {status}");
        return Err(error(
            NETWORK,
            format!("GitHub 返回错误（HTTP {}），请稍后再试。", status.as_u16()),
        ));
    }
    let too_large = || {
        log::warn!("版本信息超过 {MAX_BODY} 字节，已放弃读取");
        error(NETWORK, "版本信息过大，无法读取。")
    };
    if res.content_length().is_some_and(|n| n > MAX_BODY as u64) {
        return Err(too_large());
    }
    let mut body = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(connect_error)? {
        if body.len() + chunk.len() > MAX_BODY {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|_| error(NETWORK, "版本信息的格式不正确。"))
}
