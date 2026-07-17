use thiserror::Error;

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("invalid url: {0}")]
    InvalidUrl(#[from] url::ParseError),
    #[error("request failed: {0}")]
    Request(#[from] reqwest::Error),
}

/// 一次抓取的原始结果:重定向后的最终 URL、状态码、媒体类型与原始字节。
#[derive(Debug)]
pub struct FetchResult {
    pub final_url: url::Url,
    pub status: u16,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

/// 抓取单个 URL,跟随重定向,自动解压。
pub async fn fetch(input: &str) -> Result<FetchResult, FetchError> {
    fetch_with_cookies(input, None).await
}

/// 同 [`fetch`],但可带导入的登录态(jar)——落地页的初始 GET 也认 cookie,
/// 否则受保护页面第一跳就是登录重定向,后面全白搭。
pub async fn fetch_with_cookies(
    input: &str,
    jar: Option<crate::cookies::CookieJar>,
) -> Result<FetchResult, FetchError> {
    let url = url::Url::parse(input)?;
    let builder = crate::net::client_builder();
    let client = match jar {
        Some(jar) => builder.cookie_provider(jar).build()?,
        None => builder.build()?,
    };
    let resp = client.get(url).send().await?;

    let final_url = resp.url().clone();
    let status = resp.status().as_u16();
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let body = resp.bytes().await?.to_vec();

    Ok(FetchResult { final_url, status, content_type, body })
}
