//! Golden 回归:fixtures/*.html 的 --tree 输出必须逐字节等于同名 .tree 文件。
//!
//! 更新方式:确认新输出正确后
//! `cargo run -q -- crates/cli/tests/fixtures/<name>.html > .../<name>.tree`

use std::path::{Path, PathBuf};
use std::process::Command;

fn surl() -> Command {
    Command::new(env!("CARGO_BIN_EXE_surl"))
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn run_ok(args: &[&str], stdin: Option<&str>) -> String {
    use std::io::Write;
    use std::process::Stdio;
    let mut cmd = surl();
    cmd.args(args).stdout(Stdio::piped());
    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    }
    let mut child = cmd.spawn().expect("spawn surl");
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    let out = child.wait_with_output().expect("wait surl");
    assert!(out.status.success(), "surl {args:?} failed: {out:?}");
    String::from_utf8(out.stdout).expect("stdout utf-8")
}

#[test]
fn golden_trees_match() {
    let mut checked = 0;
    for entry in std::fs::read_dir(fixtures_dir()).unwrap() {
        let html = entry.unwrap().path();
        if html.extension().is_none_or(|e| e != "html") {
            continue;
        }
        let golden = html.with_extension("tree");
        assert!(
            golden.exists(),
            "missing golden file for {}",
            html.display()
        );
        let expected = std::fs::read_to_string(&golden).unwrap();
        let actual = run_ok(&[html.to_str().unwrap()], None);
        assert_eq!(
            actual,
            expected,
            "tree drift for {} — if intentional, regenerate the golden file",
            html.display()
        );
        checked += 1;
    }
    assert!(checked >= 2, "expected at least 2 golden fixtures");
}

#[test]
fn json_mode_is_valid_and_matches_tree_semantics() {
    let fixture = fixtures_dir().join("landing.html");
    let out = run_ok(&[fixture.to_str().unwrap(), "--json"], None);
    let v: serde_json::Value = serde_json::from_str(&out).expect("--json emits valid JSON");
    assert_eq!(v["root"]["role"], "document");
    assert_eq!(v["title"], "Acme — Ship faster");
    assert_eq!(v["root"]["uid"], 0);
    // landmark 顺序:banner / main / complementary / contentinfo
    let roles: Vec<&str> = v["root"]["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["banner", "main", "complementary", "contentinfo"]);
}

#[test]
fn dom_mode_is_idempotent() {
    // --dom 的输出再喂回去,输出必须不动(序列化稳定性)
    let fixture = fixtures_dir().join("landing.html");
    let once = run_ok(&[fixture.to_str().unwrap(), "--dom"], None);
    let twice = run_ok(&["-", "--dom"], Some(&once));
    assert_eq!(once, twice);
}

#[test]
fn stdin_input_works() {
    let out = run_ok(&["-"], Some("<h1>from stdin</h1>"));
    assert_eq!(out, "document\n  heading[1] \"from stdin\"\n");
}

#[test]
fn spa_shell_is_honestly_empty() {
    // 项目起源:SPA 空壳在 M0 阶段只有标题,没有一行内容。
    // M3 之后这里将长出真实结构(届时更新此断言)。
    let fixture = fixtures_dir().join("readaware-shell.html");
    let out = run_ok(&[fixture.to_str().unwrap()], None);
    assert_eq!(out, "document \"ReadAware — Reading that remembers\"\n");
}

#[test]
fn rejects_nonexistent_input() {
    let out = surl()
        .arg("/definitely/not/a/file.html")
        .output()
        .expect("spawn surl");
    assert!(!out.status.success());
}
