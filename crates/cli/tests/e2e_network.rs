//! 端到端(真实网络)。默认 ignored,跑法:`cargo test -- --ignored`
//! 或 `cargo test --test e2e_network -- --ignored`。

use std::process::Command;

fn surl_output(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_surl"))
        .args(args)
        .output()
        .expect("spawn surl");
    assert!(out.status.success(), "surl {args:?} failed: {out:?}");
    String::from_utf8(out.stdout).expect("stdout utf-8")
}

#[test]
#[ignore = "hits the real network"]
fn example_com_static_page() {
    let out = surl_output(&["https://example.com"]);
    assert!(out.starts_with("document \"Example Domain\""), "{out}");
    assert!(out.contains("heading[1] \"Example Domain\""), "{out}");
    // 相对 href 应被解析为绝对地址
    assert!(out.contains("link \"") && out.contains(" -> https://"), "{out}");
}

#[test]
#[ignore = "hits the real network"]
fn readaware_app_is_empty_until_m3() {
    // golden corpus 第一条:M0 阶段 SPA 空壳没有任何结构 —— 这正是
    // 2026-07-15 裸 curl 误报的可视化。M3 hydration 打通后,这条测试
    // 将改为要求树里出现 discord.gg/whDrKXwHWU。
    let out = surl_output(&["https://readaware.app"]);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 1, "SPA shell should have no children yet: {out}");
    assert!(lines[0].starts_with("document \"ReadAware"), "{out}");
}
