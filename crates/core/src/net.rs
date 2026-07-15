//! surl-runtime 的 HttpClient trait 在真实网络上的实现(reqwest)。
//! 页面主文档、外链 script、页面内 fetch 都走这一个客户端(共享连接池/cookie)。

use std::future::Future;
use std::pin::Pin;

use surl_runtime::net::{HttpClient, HttpRequest, HttpResponse, HttpResult};

pub struct ReqwestClient {
    client: reqwest::Client,
}

impl ReqwestClient {
    pub fn new() -> Result<Self, reqwest::Error> {
        Ok(ReqwestClient {
            client: reqwest::Client::builder()
                .user_agent(concat!("surl/", env!("CARGO_PKG_VERSION")))
                .build()?,
        })
    }
}

impl HttpClient for ReqwestClient {
    fn fetch<'a>(&'a self, req: HttpRequest) -> Pin<Box<dyn Future<Output = HttpResult> + 'a>> {
        Box::pin(async move {
            let method = reqwest::Method::from_bytes(req.method.as_bytes())
                .map_err(|e| format!("bad method {}: {e}", req.method))?;
            let mut builder = self.client.request(method, &req.url);
            for (k, v) in req.headers {
                builder = builder.header(k, v);
            }
            if let Some(body) = req.body {
                builder = builder.body(body);
            }
            let resp = builder.send().await.map_err(|e| e.to_string())?;

            let status = resp.status();
            let final_url = resp.url().to_string();
            let headers = resp
                .headers()
                .iter()
                .filter_map(|(k, v)| Some((k.as_str().to_owned(), v.to_str().ok()?.to_owned())))
                .collect();
            let body = resp.bytes().await.map_err(|e| e.to_string())?.to_vec();

            Ok(HttpResponse {
                status: status.as_u16(),
                status_text: status.canonical_reason().unwrap_or("").to_owned(),
                url: final_url,
                headers,
                body,
            })
        })
    }
}
