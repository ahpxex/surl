//! 从本机浏览器导入登录态(cookies),喂进 surl 的请求管线。
//!
//! 原理:浏览器把 cookies 存在 profile 里的 SQLite/二进制库,Chromium 系
//! 的值经 OS 密钥(macOS Keychain / Windows DPAPI / Linux keyring)加密。
//! 解密的脏活交给 `rookie` crate(与 yt-dlp 的 --cookies-from-browser 同源);
//! 我们只负责:选浏览器、按目标域过滤、组成 reqwest 的 cookie jar。
//!
//! 边界:导出的是明文 bearer token,和密码同等敏感。这是读你自己机器上
//! 你自己的会话——合法,但 dump 出来的东西别随手落盘。

use std::sync::Arc;

use reqwest::cookie::Jar;
use url::Url;

pub use rookie::enums::Cookie;

/// 所有出网请求共用的 cookie 容器句柄。上层不必点名 reqwest。
pub type CookieJar = Arc<Jar>;

#[derive(Debug, thiserror::Error)]
pub enum CookiesError {
    #[error("unknown browser '{0}' (支持: chrome, firefox, safari, brave, edge, arc, chromium, vivaldi, opera, 或 any)")]
    UnknownBrowser(String),
    #[error("cookie 提取失败({browser}): {reason}")]
    Extract { browser: String, reason: String },
}

/// 从指定浏览器取 cookies;`domain` 非空时只取该域(含子域)。
/// macOS Chrome 首次会弹一次钥匙串授权(rookie 读 Safe Storage 密钥)。
pub fn browser_cookies(browser: &str, domain: Option<&str>) -> Result<Vec<Cookie>, CookiesError> {
    let domains = domain.map(|d| vec![d.to_owned()]);
    type Extractor = fn(Option<Vec<String>>) -> rookie::Result<Vec<Cookie>>;
    let extract: Extractor = match browser.to_ascii_lowercase().as_str() {
        "chrome" => rookie::chrome,
        "firefox" => rookie::firefox,
        "safari" => rookie::safari,
        "brave" => rookie::brave,
        "edge" => rookie::edge,
        "arc" => rookie::arc,
        "chromium" => rookie::chromium,
        "vivaldi" => rookie::vivaldi,
        "opera" => rookie::opera,
        "" | "any" | "auto" => rookie::load,
        other => return Err(CookiesError::UnknownBrowser(other.to_owned())),
    };
    extract(domains).map_err(|e| CookiesError::Extract {
        browser: browser.to_owned(),
        reason: e.to_string(),
    })
}

/// 为一个目标 URL 构建 cookie jar(只含该站相关、未过期的 cookies)。
/// 返回 (jar, 装入条数)。
pub fn jar_for_url(browser: &str, url: &Url) -> Result<(Arc<Jar>, usize), CookiesError> {
    let cookies = browser_cookies(browser, url.host_str())?;
    let jar = Jar::default();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let mut loaded = 0;
    for c in &cookies {
        // 已过期的不装(rookie 会连过期项一起返回)
        if c.expires.is_some_and(|e| e > 0 && e < now) {
            continue;
        }
        if let Some(origin) = cookie_origin_url(c) {
            jar.add_cookie_str(&format_set_cookie(c), &origin);
            loaded += 1;
        }
    }
    Ok((Arc::new(jar), loaded))
}

/// cookie 所属 origin:reqwest 的 jar 按它做域/路径匹配。
fn cookie_origin_url(c: &Cookie) -> Option<Url> {
    let host = c.domain.trim_start_matches('.');
    if host.is_empty() {
        return None;
    }
    let scheme = if c.secure { "https" } else { "http" };
    let path = if c.path.is_empty() { "/" } else { &c.path };
    Url::parse(&format!("{scheme}://{host}{path}")).ok()
}

/// 组一条 Set-Cookie:显式带 Domain/Path,让 jar 按原样落库。
/// 不带 Expires——surl 是一次性执行,当次会话内全部视为有效。
fn format_set_cookie(c: &Cookie) -> String {
    let mut s = format!("{}={}; Domain={}; Path={}", c.name, c.value, c.domain, c.path);
    if c.secure {
        s.push_str("; Secure");
    }
    if c.http_only {
        s.push_str("; HttpOnly");
    }
    s
}

/// Netscape cookie 文件格式(curl / yt-dlp 通用),供 `surl cookies` 导出与排障。
pub fn to_netscape(cookies: &[Cookie]) -> String {
    let mut out = String::from("# Netscape HTTP Cookie File\n");
    for c in cookies {
        let include_subdomains = if c.domain.starts_with('.') { "TRUE" } else { "FALSE" };
        let secure = if c.secure { "TRUE" } else { "FALSE" };
        let expiry = c.expires.unwrap_or(0);
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            c.domain, include_subdomains, c.path, secure, expiry, c.name, c.value
        ));
    }
    out
}
