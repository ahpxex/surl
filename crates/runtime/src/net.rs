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

/// 拒绝一切请求的空实现:纯静态页面或测试用。
pub struct NoNetwork;

impl HttpClient for NoNetwork {
    fn fetch<'a>(&'a self, req: HttpRequest) -> Pin<Box<dyn Future<Output = HttpResult> + 'a>> {
        Box::pin(async move { Err(format!("network disabled (requested {})", req.url)) })
    }
}
