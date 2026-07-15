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
fn readaware_app_renders_via_own_runtime() {
    // golden corpus 第一条,即项目验收标准:readaware.app(React + Vite 产物)
    // 在自研运行时里渲染出含 discord 链接的语义树。M0-M2 阶段这里是空壳
    // ——2026-07-15 裸 curl 验证部署误报的可视化;M3 起必须是满的。
    let out = surl_output(&["https://readaware.app"]);
    assert!(out.contains("discord.gg/whDrKXwHWU"), "{out}");
    assert!(out.contains("heading[1]"), "{out}");
    assert!(out.lines().count() > 10, "tree suspiciously small: {out}");

    // 对照组:--no-js 仍是诚实的空壳
    let raw = surl_output(&["https://readaware.app", "--no-js"]);
    assert_eq!(raw.lines().count(), 1, "{raw}");
}
