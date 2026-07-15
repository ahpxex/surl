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
    let url = url::Url::parse(input)?;
    let client = reqwest::Client::builder()
        .user_agent(concat!("spurl/", env!("CARGO_PKG_VERSION")))
        .build()?;
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
