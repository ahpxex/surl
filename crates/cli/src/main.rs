use clap::Parser;

/// curl gives you bytes. Browsers give you pixels. spurl gives you structure.
#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// 要抓取的 URL
    url: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let args = Args::parse();
    let result = spurl_core::fetch::fetch(&args.url).await?;

    eprintln!("→ {}", result.final_url);
    eprintln!("  status: {}", result.status);
    if let Some(ct) = &result.content_type {
        eprintln!("  content-type: {ct}");
    }
    eprintln!("  body: {} bytes", result.body.len());

    Ok(())
}
