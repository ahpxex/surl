//! WPT 切片 runner:把官方 web-platform-tests 的 HTML 测试文件跑在 surl
//! 自己的加载管线里(parse → load → settle),经 testharnessreport.js 的
//! completion callback 收回逐用例结果,与 expectations.json 对账。
//!
//! 回归语义:失败集合必须与 expectations **精确相等**——
//! - 新增失败 = 回归,测试挂;
//! - 原有失败变通过 = 棘轮该拧紧,测试也挂(防止 expectations 腐化)。
//!
//! 更新方式:`SURL_WPT_BLESS=1 cargo test -p surl-runtime --test wpt_runner`;
//! 单文件调试:`SURL_WPT_FILTER=cloneNode` + `SURL_WPT_VERBOSE=1`。
//!
//! 文件固定自 WPT master(commit 见 resources/WPT-COMMIT.txt),BSD-3 授权
//! (resources/WPT-LICENSE.md)。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use surl_dom::parse_html;
use surl_runtime::net::FsHttpClient;
use surl_runtime::{PageRuntime, SettleOptions};

const ORIGIN: &str = "https://wpt.test";

/// 跑过的子测试总数(统计输出用)。
static SUBTESTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// 显式跳过:纯性能压力测试,耗时贴着脚本墙钟预算,debug/release
/// 构建下结果不同——棘轮必须确定,这类测试没有结构语义,不跑。
const SKIP: &[&str] = &[
    "dom/nodes/NodeList-static-length-getter-tampered-1.html",
    "dom/nodes/NodeList-static-length-getter-tampered-2.html",
    "dom/nodes/NodeList-static-length-getter-tampered-3.html",
];

fn wpt_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/wpt")
}

/// 一个文件的失败清单:测试名 → 简短失败信息(只取首行,稳定可比对)。
type Failures = BTreeMap<String, String>;

async fn run_wpt_file(rel: &str) -> Failures {
    run_wpt_file_inner(rel)
        .await
        .into_iter()
        .map(|(name, msg)| (name, stabilize(&msg)))
        .collect()
}

async fn run_wpt_file_inner(rel: &str) -> Failures {
    let root = wpt_root();
    // 有些 WPT 测试刻意用非 UTF-8 编码,lossy 读入(编码正确性不在本切片范围)
    let bytes = std::fs::read(root.join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"));
    let html = String::from_utf8_lossy(&bytes).into_owned();
    let base = url::Url::parse(&format!("{ORIGIN}/{rel}")).unwrap();
    let mut rt = PageRuntime::with_base(parse_html(&html), Some(base)).expect("runtime boots");
    // WPT 单文件都是小测试:2s 预算足够,防住个别病理文件拖垮整个套件
    rt.set_script_wall_budget(std::time::Duration::from_secs(2));
    let net = FsHttpClient {
        root,
        origin: ORIGIN.into(),
    };
    let mut failures = Failures::new();
    let report = match rt.load(&net, SettleOptions::default()).await {
        Ok(report) => report,
        Err(e) => {
            failures.insert("__load_error__".into(), first_line(&e.to_string()).to_owned());
            return failures;
        }
    };
    for e in report.scripts.errors.iter().chain(&report.modules.errors) {
        failures.insert(
            "__script_error__".into(),
            first_line(e).to_owned(),
        );
    }

    let raw = match rt.eval_string("JSON.stringify(globalThis.__wpt_results ?? null)") {
        Ok(raw) => raw,
        Err(e) => {
            failures.insert("__harness__".into(), format!("results unreadable: {}", first_line(&e.to_string())));
            return failures;
        }
    };
    if std::env::var("SURL_WPT_VERBOSE").is_ok() {
        eprintln!("=== {rel} ===");
        for e in report.scripts.errors.iter().chain(&report.modules.errors) {
            eprintln!("script error: {e}");
        }
        eprintln!("{raw}");
    }
    let Ok(results) = serde_json::from_str::<serde_json::Value>(&raw) else {
        failures.insert("__harness__".into(), format!("results not JSON: {}", first_line(&raw)));
        return failures;
    };
    if results.is_null() {
        failures.insert(
            "__harness__".into(),
            "harness never completed (timeout or crash before completion)".into(),
        );
        return failures;
    }
    let results_arr = results.as_array().cloned().unwrap_or_default();
    SUBTESTS.fetch_add(results_arr.len(), std::sync::atomic::Ordering::Relaxed);
    for t in &results_arr {
        let status = t["status"].as_i64().unwrap_or(-1);
        if status != 0 {
            let name = t["name"].as_str().unwrap_or("<unnamed>").to_owned();
            let code = match status {
                1 => "FAIL",
                2 => "TIMEOUT",
                3 => "NOTRUN",
                4 => "PRECONDITION_FAILED",
                _ => "UNKNOWN",
            };
            let message = first_line(t["message"].as_str().unwrap_or(""));
            failures.insert(name, format!("{code}: {message}"));
        }
    }
    let harness_status = rt
        .eval_string("String(globalThis.__wpt_harness_status ?? 'null')")
        .unwrap_or_default();
    if harness_status != "0" && harness_status != "null" {
        let msg = rt
            .eval_string("String(globalThis.__wpt_harness_message ?? '')")
            .unwrap_or_default();
        failures.insert("__harness__".into(), format!("status {harness_status}: {}", first_line(&msg)));
    }
    failures
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("").trim()
}

/// 失败消息里的非确定性 token 归一化:堆地址(Object(0x...))与测试自生成
/// 的 uuid/uid 每次运行都不同,不抹掉的话棘轮的「精确相等」无法成立。
fn stabilize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // 0x 后跟一串十六进制 → 0x…
        if bytes[i] == b'0' && i + 1 < bytes.len() && bytes[i + 1] == b'x' {
            let mut j = i + 2;
            while j < bytes.len() && bytes[j].is_ascii_hexdigit() {
                j += 1;
            }
            if j > i + 2 {
                out.push_str("0x…");
                i = j;
                continue;
            }
        }
        // 8-4-4-4-12 的 uuid → <uuid>
        if bytes[i].is_ascii_hexdigit() {
            let rest = &s[i..];
            if is_uuid_prefix(rest) {
                out.push_str("<uuid>");
                i += 36;
                continue;
            }
        }
        // uid=数字 → uid=…
        if s[i..].starts_with("uid=") {
            let mut j = i + 4;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 4 {
                out.push_str("uid=…");
                i = j;
                continue;
            }
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn is_uuid_prefix(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 36 {
        return false;
    }
    for (idx, &c) in b[..36].iter().enumerate() {
        match idx {
            8 | 13 | 18 | 23 => {
                if c != b'-' {
                    return false;
                }
            }
            _ => {
                if !c.is_ascii_hexdigit() {
                    return false;
                }
            }
        }
    }
    true
}

/// 递归发现测试:扩切片 = 往 wpt/ 扔目录,这里不维护清单。
/// 一个测试 = 引用 testharness.js 的 .html/.htm 文件;support/、resources/、
/// common/ 下的辅助页与 -manual/-ref 不算。
fn discover_tests() -> Vec<String> {
    let root = wpt_root();
    let filter = std::env::var("SURL_WPT_FILTER").unwrap_or_default();
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if !matches!(name.as_str(), "support" | "resources" | "common") {
                    stack.push(path);
                }
                continue;
            }
            if !(name.ends_with(".html") || name.ends_with(".htm")) {
                continue;
            }
            // WPT 命名约定:-manual/-ref 是文件名后缀,不能子串匹配
            // (no-referrer.sub.html 里也有 "-ref")
            let stem = name
                .trim_end_matches(".html")
                .trim_end_matches(".htm")
                .trim_end_matches(".sub");
            if stem.ends_with("-manual") || stem.ends_with("-ref") {
                continue;
            }
            let rel = path
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if SKIP.contains(&rel.as_str()) {
                continue;
            }
            if !filter.is_empty() && !rel.contains(&filter) {
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            if String::from_utf8_lossy(&bytes).contains("testharness.js") {
                out.push(rel);
            }
        }
    }
    out.sort();
    out
}

#[tokio::test]
async fn wpt_slice_matches_expectations() {
    let expectations_path = wpt_root().join("expectations.json");
    let expected: BTreeMap<String, Failures> = std::fs::read_to_string(&expectations_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    let mut actual: BTreeMap<String, Failures> = BTreeMap::new();
    let mut total_cases = 0usize;
    for rel in discover_tests() {
        let failures = run_wpt_file(&rel).await;
        let ran = 1; // 至少 harness 本身
        total_cases += ran;
        if !failures.is_empty() {
            actual.insert(rel, failures);
        }
    }
    assert!(total_cases > 0, "no WPT files discovered");

    let subtests = SUBTESTS.load(std::sync::atomic::Ordering::Relaxed);
    let failed: usize = actual.values().map(BTreeMap::len).sum();
    eprintln!(
        "WPT slice: {total_cases} files, {subtests} subtests, {failed} known-fail entries, \
         {} files fully green",
        total_cases - actual.len()
    );

    if std::env::var("SURL_WPT_BLESS").is_ok() {
        std::fs::write(
            &expectations_path,
            serde_json::to_string_pretty(&actual).unwrap() + "\n",
        )
        .unwrap();
        eprintln!("expectations blessed: {} files with failures", actual.len());
        return;
    }

    if actual != expected {
        let mut diff = String::new();
        for (file, fails) in &actual {
            let empty = Failures::new();
            let exp = expected.get(file).unwrap_or(&empty);
            for (name, msg) in fails {
                match exp.get(name) {
                    None => diff.push_str(&format!("REGRESSION {file} :: {name} :: {msg}\n")),
                    Some(old) if old != msg => diff.push_str(&format!(
                        "MESSAGE-DRIFT {file} :: {name} :: {old} -> {msg}\n"
                    )),
                    Some(_) => {}
                }
            }
        }
        for (file, exp) in &expected {
            let empty = Failures::new();
            let act = actual.get(file).unwrap_or(&empty);
            for name in exp.keys() {
                if !act.contains_key(name) {
                    diff.push_str(&format!("NOW-PASSING {file} :: {name} (re-bless to ratchet)\n"));
                }
            }
        }
        panic!(
            "WPT results drifted from expectations:\n{diff}\n\
             If intentional: SURL_WPT_BLESS=1 cargo test -p surl-runtime --test wpt_runner"
        );
    }
}
