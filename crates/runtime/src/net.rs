//! 网络抽象:运行时只认这个 trait,不认 reqwest。
//!
//! surl-core 提供 reqwest 实现;测试提供内存 mock,让事件循环 / settledness
//! 的集成测试完全离线、完全确定。

use std::future::Future;
use std::pin::Pin;

#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub url: String,
    pub method: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub status_text: String,
    /// 重定向后的最终 URL
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub type HttpResult = Result<HttpResponse, String>;

/// 单线程友好(future 不要求 Send):JS 世界本身就是 !Send 的。
pub trait HttpClient {
    fn fetch<'a>(&'a self, req: HttpRequest) -> Pin<Box<dyn Future<Output = HttpResult> + 'a>>;
}

/// 把本地目录当成一个网站来 serve:`<origin>/<path>` → `<root>/<path>`。
/// 离线 corpus 回归与 WPT 切片的地基——测试永远不碰真实网络。
/// 同 origin 下缺文件回 404;跨 origin 一律拒绝(fetch 会 reject)。
pub struct FsHttpClient {
    pub root: std::path::PathBuf,
    /// 形如 "https://wpt.test",不带尾斜杠
    pub origin: String,
}

impl HttpClient for FsHttpClient {
    fn fetch<'a>(&'a self, req: HttpRequest) -> Pin<Box<dyn Future<Output = HttpResult> + 'a>> {
        Box::pin(async move {
            let Some(rest) = req.url.strip_prefix(&self.origin) else {
                return Err(format!("offline: foreign origin {}", req.url));
            };
            let path = rest.split(['?', '#']).next().unwrap_or("");
            let rel = path.trim_start_matches('/').to_owned();
            if rel.split('/').any(|seg| seg == "..") {
                return Err("offline: path traversal rejected".into());
            }
            match std::fs::read(self.root.join(&rel)) {
                Ok(body) => Ok(HttpResponse {
                    status: 200,
                    status_text: "OK".into(),
                    url: req.url,
                    headers: vec![("content-type".into(), guess_mime(&rel).into())],
                    body,
                }),
                Err(_) => Ok(HttpResponse {
                    status: 404,
                    status_text: "Not Found".into(),
                    url: req.url,
                    headers: Vec::new(),
                    body: Vec::new(),
                }),
            }
        })
    }
}

fn guess_mime(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html",
        Some("js" | "mjs") => "text/javascript",
        Some("json") => "application/json",
        Some("css") => "text/css",
        _ => "application/octet-stream",
    }
}

/// 拒绝一切请求的空实现:纯静态页面或测试用。
pub struct NoNetwork;

impl HttpClient for NoNetwork {
    fn fetch<'a>(&'a self, req: HttpRequest) -> Pin<Box<dyn Future<Output = HttpResult> + 'a>> {
        Box::pin(async move { Err(format!("network disabled (requested {})", req.url)) })
    }
}
