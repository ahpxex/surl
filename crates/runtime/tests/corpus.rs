//! 离线 corpus 回归:真实网站的产物冻结在 tests/corpus/ 里,经 FsHttpClient
//! 走完整加载管线。boss fight 由此变成永久的、不碰网络的回归测试。
//!
//! 第一条用例即项目验收标准:readaware.app(React 19 + Vite 产物,快照于
//! 2026-07-15)必须渲染出含 discord.gg/whDrKXwHWU 的语义树。

use std::path::Path;

use surl_dom::parse_html;
use surl_runtime::net::FsHttpClient;
use surl_runtime::{PageRuntime, SettleOptions};

fn corpus_net(site: &str) -> FsHttpClient {
    FsHttpClient {
        root: Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus").join(site),
        origin: format!("https://{site}"),
    }
}

#[tokio::test]
async fn readaware_renders_offline() {
    let net = corpus_net("readaware.app");
    let html = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/readaware.app/index.html"),
    )
    .expect("frozen index.html");

    let base = url::Url::parse("https://readaware.app/").unwrap();
    let rt = PageRuntime::with_base(parse_html(&html), Some(base.clone())).unwrap();
    let report = rt.load(&net, SettleOptions::default()).await.unwrap();

    assert!(
        report.modules.errors.is_empty(),
        "module errors: {:?}",
        report.modules.errors
    );
    assert!(report.modules.prefetched >= 2, "{:?}", report.modules);

    let tree = {
        let doc = rt.document();
        surl_core::semantic::extract(&doc, Some(&base)).to_tree_string()
    };

    // 项目验收标准:2026-07-15 的 curl 误报,在这里永久回归
    assert!(tree.contains("discord.gg/whDrKXwHWU"), "{tree}");
    // 结构抽查:地标、标题、下载链接
    assert!(tree.contains("banner\n"), "{tree}");
    assert!(tree.contains("heading[1] \"Reading that remembers\""), "{tree}");
    assert!(tree.contains("link \"GitHub\" -> https://github.com/ahpxex/read-aware"), "{tree}");
    assert!(
        tree.lines().count() > 40,
        "tree suspiciously small ({} lines):\n{tree}",
        tree.lines().count()
    );

    // 确定性:同一份输入再跑一遍,输出逐字节一致(虚拟时钟 + 固定种子 crypto)
    let rt2 = PageRuntime::with_base(parse_html(&html), Some(base.clone())).unwrap();
    rt2.load(&net, SettleOptions::default()).await.unwrap();
    let tree2 = {
        let doc2 = rt2.document();
        surl_core::semantic::extract(&doc2, Some(&base)).to_tree_string()
    };
    assert_eq!(tree, tree2, "rendering must be deterministic");
}
