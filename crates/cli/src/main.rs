use std::io::Read;

use anyhow::Context;
use clap::Parser;

/// curl gives you bytes. Browsers give you pixels. surl gives you structure.
#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// URL(http/https)、本地 HTML 文件路径,或 `-` 从 stdin 读
    input: String,

    /// 语义大纲(默认输出)
    #[arg(long, group = "mode")]
    tree: bool,

    /// JS 执行后的序列化 HTML
    #[arg(long, group = "mode")]
    dom: bool,

    /// 完整语义 IR(JSON)
    #[arg(long, group = "mode")]
    json: bool,

    /// 不执行页面 JS(只看服务器返回的原始结构)
    #[arg(long)]
    no_js: bool,
}

// PageRuntime 是单线程世界(Rc + JS 引擎),整个程序跑 current_thread;
// 网络并发靠 async,不靠线程。
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();
    let (html, base) = load(&args.input).await?;

    let mut doc = surl_dom::parse_html(&html);
    if !args.no_js {
        let rt = surl_runtime::PageRuntime::with_base(doc, base.clone())?;
        let net = surl_core::net::ReqwestClient::new()?;
        let report = rt
            .load(&net, surl_runtime::SettleOptions::default())
            .await?;
        tracing::debug!(
            scripts = report.scripts.executed,
            script_errors = report.scripts.errors.len(),
            modules_prefetched = report.modules.prefetched,
            modules_evaluated = report.modules.evaluated,
            timers = report.settle.timers_fired,
            fetches = report.settle.fetches,
            virtual_ms = report.settle.virtual_elapsed_ms,
            "page load settled"
        );
        for e in report
            .scripts
            .errors
            .iter()
            .chain(&report.modules.prefetch_errors)
            .chain(&report.modules.errors)
        {
            tracing::warn!(target: "surl_js", "{e}");
        }
        for miss in &report.modules.runtime_misses {
            tracing::warn!(target: "surl_js", "dynamic import miss: {miss}");
        }
        doc = rt.take_document();
    }

    if args.dom {
        // 精确字节输出,不加尾换行:保证 --dom 的输出可以无损地再喂回来
        print!("{}", doc.to_html());
        return Ok(());
    }

    let snapshot = surl_core::semantic::extract(&doc, base.as_ref());
    if args.json {
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
    } else {
        print!("{}", snapshot.to_tree_string());
    }
    Ok(())
}

/// 取输入:返回 HTML 文本与用于解析相对链接的 base URL。
async fn load(input: &str) -> anyhow::Result<(String, Option<url::Url>)> {
    if input == "-" {
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .context("read stdin")?;
        return Ok((buf, None));
    }
    if input.starts_with("http://") || input.starts_with("https://") {
        let result = surl_core::fetch::fetch(input).await?;
        if !(200..300).contains(&result.status) {
            // 项目起源就是 200 掩盖了空壳;非 2xx 更要明着说(可能是反爬挑战页)
            eprintln!(
                "surl: warning: HTTP {} from {} — output may be an error/challenge page",
                result.status, result.final_url
            );
        }
        let html = String::from_utf8_lossy(&result.body).into_owned();
        return Ok((html, Some(result.final_url)));
    }
    let path = std::path::Path::new(input);
    if path.exists() {
        let html = std::fs::read_to_string(path).with_context(|| format!("read {input}"))?;
        return Ok((html, None));
    }
    anyhow::bail!("`{input}` is neither a URL, an existing file, nor `-`");
}
