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

fn wpt_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/wpt")
}

/// 一个文件的失败清单:测试名 → 简短失败信息(只取首行,稳定可比对)。
type Failures = BTreeMap<String, String>;

async fn run_wpt_file(rel: &str) -> Failures {
    let root = wpt_root();
    let html = std::fs::read_to_string(root.join(rel))
        .unwrap_or_else(|e| panic!("read {rel}: {e}"));
    let base = url::Url::parse(&format!("{ORIGIN}/{rel}")).unwrap();
    let rt = PageRuntime::with_base(parse_html(&html), Some(base)).expect("runtime boots");
    let net = FsHttpClient {
        root,
        origin: ORIGIN.into(),
    };
    let report = rt
        .load(&net, SettleOptions::default())
        .await
        .expect("load succeeds");

    let mut failures = Failures::new();
    for e in report.scripts.errors.iter().chain(&report.modules.errors) {
        failures.insert(
            "__script_error__".into(),
            first_line(e).to_owned(),
        );
    }

    let raw = rt
        .eval_string("JSON.stringify(globalThis.__wpt_results ?? null)")
        .expect("read results");
    if std::env::var("SURL_WPT_VERBOSE").is_ok() {
        eprintln!("=== {rel} ===");
        for e in report.scripts.errors.iter().chain(&report.modules.errors) {
            eprintln!("script error: {e}");
        }
        eprintln!("{raw}");
    }
    let results: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
    if results.is_null() {
        failures.insert(
            "__harness__".into(),
            "harness never completed (timeout or crash before completion)".into(),
        );
        return failures;
    }
    for t in results.as_array().expect("results array") {
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

fn discover_tests() -> Vec<String> {
    let root = wpt_root();
    let mut out = Vec::new();
    for dir in ["dom/nodes", "dom/events"] {
        let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
            continue;
        };
        let filter = std::env::var("SURL_WPT_FILTER").unwrap_or_default();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".html") && (filter.is_empty() || name.contains(&filter)) {
                out.push(format!("{dir}/{name}"));
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
                if !exp.contains_key(name) {
                    diff.push_str(&format!("REGRESSION {file} :: {name} :: {msg}\n"));
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
