//! 网络代理：跟随系统、不使用代理或自定义 HTTP 代理。
//! 设置保存在索引数据库的 `meta` 表（只由 Rust 侧读写），模型下载（`models`）每次开始时读取，修改后对之后的请求
//! 立即生效，进行中的下载不受影响。
//! 自定义代理时本机地址（`localhost`、`127.0.0.1`、`::1`）仍然直连，代理服务器通常访问不到本机上的服务。
use reqwest::{ClientBuilder, NoProxy, Url};
use rummage_index::{Db, DbError};
use rummage_proto::{ProxyMode, ProxySetting};

const META_KEY: &str = "proxy";
const SOCKS: &str = "暂不支持 SOCKS 代理，请使用代理软件提供的 HTTP 端口。";
const EXAMPLE: &str = "例如 http://127.0.0.1:7890";
const LOOPBACK: &str = "localhost,127.0.0.1,::1";

/// 当前生效的连接方式
#[derive(Debug, Clone)]
pub enum Route {
    System,
    Direct,
    Via(Url),
}

impl Route {
    pub fn from_setting(setting: &ProxySetting) -> Route {
        match setting.mode {
            ProxyMode::System => Route::System,
            ProxyMode::None => Route::Direct,
            ProxyMode::Custom => match parse(&setting.url) {
                Ok((_, url)) => Route::Via(url),
                // 只会来自手动改过的数据库，按默认的跟随系统处理
                Err(err) => {
                    log::warn!("代理设置无效，改为跟随系统：{err}");
                    Route::System
                }
            },
        }
    }

    /// 按连接方式配置 reqwest 客户端。不调用 proxy 时 reqwest 会读取系统代理设置（system-proxy feature）
    pub fn apply(&self, builder: ClientBuilder) -> reqwest::Result<ClientBuilder> {
        Ok(match self {
            Route::System => builder,
            Route::Direct => builder.no_proxy(),
            Route::Via(url) => builder
                .proxy(reqwest::Proxy::all(url.clone())?.no_proxy(NoProxy::from_string(LOOPBACK))),
        })
    }

    /// 访问 url 时是否经过自定义代理，用于把连接失败归因到代理服务器
    pub fn proxies(&self, url: &Url) -> bool {
        matches!(self, Route::Via(_))
            && !matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
    }
}

/// 读取保存的设置；没有保存过或无法解析时跟随系统
pub fn load(db: &Db) -> Result<ProxySetting, DbError> {
    Ok(match db.meta_get(META_KEY)? {
        Some(raw) => serde_json::from_str(&raw).unwrap_or_else(|err| {
            log::warn!("代理设置无法解析，改为跟随系统：{err}");
            ProxySetting::default()
        }),
        None => ProxySetting::default(),
    })
}

/// 校验并保存设置，返回保存后的设置（地址已规范化）。地址不合法时不保存，错误为给用户看的整句
pub fn save(db: &Db, setting: ProxySetting) -> Result<ProxySetting, SaveError> {
    let url = match setting.mode {
        ProxyMode::Custom => parse(&setting.url).map_err(SaveError::Invalid)?.0,
        // 其他模式只记住地址文本，切回自定义时再校验
        _ => setting.url.trim().to_string(),
    };
    let setting = ProxySetting {
        mode: setting.mode,
        url,
    };
    let raw = serde_json::to_string(&setting).expect("代理设置只含字符串，序列化不会失败");
    db.meta_set(META_KEY, &raw)?;
    Ok(setting)
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Db(#[from] DbError),
}

/// 校验自定义代理地址，返回规范化后保存的文本和解析结果。
/// 只接受 HTTP 代理，可以省略 http://；必须带主机和端口，可以带用户名和密码，不能带路径
pub fn parse(input: &str) -> Result<(String, Url), String> {
    let text = input.trim().trim_end_matches('/');
    if text.is_empty() {
        return Err("请填写代理地址。".into());
    }
    let text = match text.split_once("://") {
        None => format!("http://{text}"),
        Some((scheme, _)) if scheme.eq_ignore_ascii_case("http") => text.to_string(),
        Some((scheme, _)) if scheme.to_ascii_lowercase().starts_with("socks") => {
            return Err(SOCKS.into());
        }
        Some(_) => {
            return Err(format!(
                "只支持 HTTP 代理，地址需以 http:// 开头，{EXAMPLE}。"
            ));
        }
    };
    let url = Url::parse(&text).map_err(|_| {
        let hint = if text.contains('@') {
            "。用户名或密码含特殊字符时请使用百分号编码"
        } else {
            ""
        };
        format!("代理地址格式不正确，{EXAMPLE}{hint}。")
    })?;
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        return Err(format!("代理地址只需填写主机和端口，{EXAMPLE}。"));
    }
    // Url 会省略默认端口 80，从原文判断是否写了端口；IPv6 地址形如 [::1]:7890
    let authority = text["http://".len()..]
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let has_port = host
        .rsplit_once(':')
        .is_some_and(|(_, port)| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
        && url.port() != Some(0);
    if !has_port {
        return Err(format!("代理地址需包含端口，{EXAMPLE}。"));
    }
    Ok((text, url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_http_proxy() {
        let ok = |input: &str| parse(input).unwrap().0;
        assert_eq!(ok("http://127.0.0.1:7890"), "http://127.0.0.1:7890");
        assert_eq!(ok("  127.0.0.1:7890/ "), "http://127.0.0.1:7890");
        assert_eq!(ok("HTTP://Proxy.local:8080"), "HTTP://Proxy.local:8080");
        assert_eq!(ok("localhost:80"), "http://localhost:80");
        assert_eq!(ok("[::1]:7890"), "http://[::1]:7890");
        let (_, url) = parse("http://user:p%40ss@host:3128").unwrap();
        assert_eq!(url.username(), "user");
        assert_eq!(url.password(), Some("p%40ss"));
        assert_eq!(url.port(), Some(3128));
    }

    #[test]
    fn rejects_invalid_proxy() {
        let err = |input: &str| parse(input).unwrap_err();
        assert_eq!(err(" "), "请填写代理地址。");
        assert_eq!(err("socks5://127.0.0.1:7891"), SOCKS);
        assert_eq!(err("SOCKS5H://127.0.0.1:7891"), SOCKS);
        assert!(err("https://127.0.0.1:7890").contains("只支持 HTTP 代理"));
        assert!(err("127.0.0.1").contains("端口"));
        assert!(err("http://127.0.0.1:").contains("端口"));
        assert!(err("http://[::1]").contains("端口"));
        assert!(err("http://user:pass@host").contains("端口"));
        assert!(err("http://127.0.0.1:0").contains("端口"));
        assert!(err("http://127.0.0.1:99999").ends_with("例如 http://127.0.0.1:7890。"));
        assert!(err("http://u:p@ss:w@rd@h:99999").ends_with("请使用百分号编码。"));
        assert!(err("http://:7890").contains("格式不正确"));
        assert!(err("http://127.0.0.1:7890/path").contains("只需填写主机和端口"));
        assert!(err("http://127.0.0.1:7890?a=1").contains("只需填写主机和端口"));
    }

    #[test]
    fn invalid_saved_setting_follows_system() {
        let setting = ProxySetting {
            mode: ProxyMode::Custom,
            url: "socks5://127.0.0.1:1080".into(),
        };
        assert!(matches!(Route::from_setting(&setting), Route::System));
    }

    #[test]
    fn loopback_bypasses_custom_proxy() {
        let route = Route::Via(parse("127.0.0.1:7890").unwrap().1);
        assert!(route.proxies(&Url::parse("https://huggingface.co/").unwrap()));
        assert!(!route.proxies(&Url::parse("http://localhost:8080/v1").unwrap()));
        assert!(!route.proxies(&Url::parse("http://[::1]:8080/").unwrap()));
        assert!(!Route::System.proxies(&Url::parse("https://huggingface.co/").unwrap()));
    }

    #[test]
    fn save_and_load() {
        let dir = crate::test_dir();
        let db = Db::open(dir.path()).unwrap();
        assert_eq!(load(&db).unwrap(), ProxySetting::default());
        let saved = save(
            &db,
            ProxySetting {
                mode: ProxyMode::Custom,
                url: " 127.0.0.1:7890/ ".into(),
            },
        )
        .unwrap();
        assert_eq!(saved.url, "http://127.0.0.1:7890");
        assert_eq!(load(&db).unwrap(), saved);
        // 不合法的地址不保存
        let bad = ProxySetting {
            mode: ProxyMode::Custom,
            url: "socks5://x:1".into(),
        };
        assert!(matches!(save(&db, bad), Err(SaveError::Invalid(_))));
        assert_eq!(load(&db).unwrap(), saved);
        // 切换模式时保留地址文本
        let none = save(
            &db,
            ProxySetting {
                mode: ProxyMode::None,
                url: saved.url.clone(),
            },
        )
        .unwrap();
        assert_eq!(none.url, "http://127.0.0.1:7890");
    }
}
