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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let args = Args::parse();
    let (html, base) = load(&args.input).await?;

    let doc = surl_dom::parse_html(&html);

    if args.dom {
        print!("{}", doc.to_html());
        println!();
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
            tracing::warn!(status = result.status, "non-2xx response");
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
