//! 从 Chromium 系浏览器导入 localStorage(补 cookie 之外的登录态)。
//!
//! localStorage 存在 profile 的 `Local Storage/leveldb`(LevelDB,非 SQLite),
//! 按 origin 分区。数据项的 key 形如 `_<origin>\0<encoded-key>`,值与 key 的
//! 字符串都带一字节编码前缀(0=UTF-16LE, 1=Latin1)。rookie 不管这个,自己读。
//!
//! 范围:仅 Chromium 家族(Chrome/Brave/Edge/Chromium/Vivaldi/Opera)。
//! Firefox 的 localStorage 是另一套(per-origin sqlite + snappy),v1 不做——
//! Firefox 的 cookie 仍可用,只是 localStorage 不导。IndexedDB 一律不做。

use std::path::PathBuf;

use rusty_leveldb::{DB, LdbIterator, Options};

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("localStorage 导入暂不支持浏览器 '{0}'(仅 Chromium 家族)")]
    Unsupported(String),
    #[error("找不到 {browser} 的 Local Storage 目录:{path}")]
    NotFound { browser: String, path: String },
    #[error("读取 LevelDB 失败:{0}")]
    LevelDb(String),
    #[error("拷贝 Local Storage 失败:{0}")]
    Copy(String),
}

/// 取指定 origin(如 `https://github.com`)的 localStorage 键值对。
pub fn browser_local_storage(
    browser: &str,
    origin: &str,
) -> Result<Vec<(String, String)>, StorageError> {
    let src = chromium_leveldb_path(browser)?;
    if !src.is_dir() {
        return Err(StorageError::NotFound {
            browser: browser.to_owned(),
            path: src.display().to_string(),
        });
    }
    // Chrome 运行时对 LevelDB 持锁,拷一份出来读(WAL/log 一并拷,开时会重放)
    let tmp = copy_leveldb(&src)?;
    let result = read_origin(&tmp, origin);
    let _ = std::fs::remove_dir_all(&tmp); // 尽力清理临时副本
    result
}

fn read_origin(dir: &PathBuf, origin: &str) -> Result<Vec<(String, String)>, StorageError> {
    let opts = Options {
        paranoid_checks: false,
        ..Options::default()
    };
    let mut db = DB::open(dir, opts).map_err(|e| StorageError::LevelDb(e.to_string()))?;
    let mut iter = db.new_iter().map_err(|e| StorageError::LevelDb(e.to_string()))?;

    // 数据项 key 前缀:`_<origin>\0`
    let mut prefix = Vec::with_capacity(origin.len() + 2);
    prefix.push(b'_');
    prefix.extend_from_slice(origin.as_bytes());
    prefix.push(0);

    let mut out = Vec::new();
    while iter.advance() {
        let Some((k, v)) = iter.current() else { break };
        if !k.starts_with(&prefix) {
            continue;
        }
        let key = decode_string(&k[prefix.len()..]);
        let val = decode_string(&v);
        if let (Some(key), Some(val)) = (key, val) {
            out.push((key, val));
        }
    }
    Ok(out)
}

/// Chromium 的字符串编码:首字节 0=UTF-16LE,1=Latin1(每字节一个码点)。
fn decode_string(bytes: &[u8]) -> Option<String> {
    match bytes.split_first() {
        None => Some(String::new()),
        Some((0, rest)) => {
            if rest.len() % 2 != 0 {
                return None;
            }
            let units: Vec<u16> = rest
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            String::from_utf16(&units).ok()
        }
        Some((1, rest)) => Some(rest.iter().map(|&b| b as char).collect()),
        // 未知编码前缀:当作原始 UTF-8 兜底(不该发生)
        Some((_, _)) => String::from_utf8(bytes.to_vec()).ok(),
    }
}

/// 把 leveldb 目录浅拷贝到临时目录(leveldb 目录是扁平的,无子目录)。
fn copy_leveldb(src: &PathBuf) -> Result<PathBuf, StorageError> {
    let dst = std::env::temp_dir().join(format!("surl-ls-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dst);
    std::fs::create_dir_all(&dst).map_err(|e| StorageError::Copy(e.to_string()))?;
    let entries = std::fs::read_dir(src).map_err(|e| StorageError::Copy(e.to_string()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() {
            let name = entry.file_name();
            // LOCK 文件不拷:让副本自己新建,避免继承锁状态
            if name == "LOCK" {
                continue;
            }
            std::fs::copy(&path, dst.join(name)).map_err(|e| StorageError::Copy(e.to_string()))?;
        }
    }
    Ok(dst)
}

/// 该浏览器是否支持 localStorage 导入(仅 Chromium 家族)。
pub fn supports_local_storage(browser: &str) -> bool {
    matches!(
        browser.to_ascii_lowercase().as_str(),
        "chrome" | "chromium" | "brave" | "edge" | "vivaldi" | "opera"
    )
}

/// 各 Chromium 浏览器的 `Local Storage/leveldb` 路径(默认 profile)。
fn chromium_leveldb_path(browser: &str) -> Result<PathBuf, StorageError> {
    let root = chromium_user_data_dir(browser)?;
    Ok(root.join("Default").join("Local Storage").join("leveldb"))
}

fn chromium_user_data_dir(browser: &str) -> Result<PathBuf, StorageError> {
    let b = browser.to_ascii_lowercase();

    #[cfg(target_os = "macos")]
    let base = {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let app = home.map(|h| h.join("Library").join("Application Support"));
        match (b.as_str(), app) {
            (_, None) => None,
            ("chrome", Some(a)) => Some(a.join("Google").join("Chrome")),
            ("chromium", Some(a)) => Some(a.join("Chromium")),
            ("brave", Some(a)) => Some(a.join("BraveSoftware").join("Brave-Browser")),
            ("edge", Some(a)) => Some(a.join("Microsoft Edge")),
            ("vivaldi", Some(a)) => Some(a.join("Vivaldi")),
            ("opera", Some(a)) => Some(a.join("com.operasoftware.Opera")),
            _ => None,
        }
    };

    #[cfg(target_os = "linux")]
    let base = {
        let cfg = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
        match (b.as_str(), cfg) {
            (_, None) => None,
            ("chrome", Some(c)) => Some(c.join("google-chrome")),
            ("chromium", Some(c)) => Some(c.join("chromium")),
            ("brave", Some(c)) => Some(c.join("BraveSoftware").join("Brave-Browser")),
            ("edge", Some(c)) => Some(c.join("microsoft-edge")),
            ("vivaldi", Some(c)) => Some(c.join("vivaldi")),
            ("opera", Some(c)) => Some(c.join("opera")),
            _ => None,
        }
    };

    #[cfg(target_os = "windows")]
    let base = {
        let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
        match (b.as_str(), local) {
            (_, None) => None,
            ("chrome", Some(l)) => Some(l.join("Google").join("Chrome").join("User Data")),
            ("chromium", Some(l)) => Some(l.join("Chromium").join("User Data")),
            ("brave", Some(l)) => {
                Some(l.join("BraveSoftware").join("Brave-Browser").join("User Data"))
            }
            ("edge", Some(l)) => Some(l.join("Microsoft").join("Edge").join("User Data")),
            ("vivaldi", Some(l)) => Some(l.join("Vivaldi").join("User Data")),
            _ => None,
        }
    };

    base.ok_or_else(|| StorageError::Unsupported(browser.to_owned()))
}
