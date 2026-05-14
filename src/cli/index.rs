use std::sync::Arc;

use anyhow::{Result, bail};
use clap::Args;

use crate::chunk::tree_sitter::TreeSitterChunker;
use crate::config::Config;
use crate::embed::build_embedder;
use crate::indexer::cost::{self, ProviderRates};
use crate::indexer::{IndexRun, resolve_namespace};
use crate::store::build_store;
use crate::summarize::anthropic::build_summarizer;

#[derive(Args, Debug)]
pub struct IndexArgs {
    /// Ignore high-water mark, full reindex
    #[arg(long)]
    pub full: bool,

    /// Estimate cost without making API calls or writing to the store
    #[arg(long)]
    pub dry_run: bool,

    /// Bound parallel file processing (default: 4)
    #[arg(long, default_value = "4")]
    pub concurrency: usize,

    /// Override starting SHA for diff (manual recovery)
    #[arg(long)]
    pub from: Option<String>,

    /// Hard cost cap in USD; abort if estimated remaining cost exceeds this
    #[arg(long)]
    pub max_cost: Option<f64>,
}

pub async fn run(args: IndexArgs) -> Result<()> {
    if args.from.is_some() {
        bail!("--from is not yet implemented");
    }

    let config = Config::new()?;

    if args.dry_run {
        return run_dry_run(&config).await;
    }

    config.embed.validate()?;
    config.summarizer.validate()?;

    let namespace = resolve_namespace(&config)?;
    let chunker: Arc<dyn crate::chunk::Chunker> = Arc::new(TreeSitterChunker::new());
    let summarizer = build_summarizer(&config.summarizer)?;
    let embedder = build_embedder(&config.embed).await?;
    let store = build_store(&config.store, embedder.dimension())?;

    eprintln!(
        "Indexing into namespace '{}' with {}/{}...",
        namespace.as_str(),
        embedder.provider_name(),
        embedder.model_name()
    );

    let root = std::env::current_dir()?;
    let index_run = IndexRun::new(
        chunker,
        Arc::from(summarizer),
        Arc::from(embedder),
        Arc::from(store),
        namespace,
        args.concurrency,
    );
    let report = index_run.run(args.full, &root).await?;

    eprintln!(
        "\nDone in {:.1}s: {} processed, {} skipped (unchanged), {} failed, {} vectors upserted, {} deleted",
        report.elapsed.as_secs_f64(),
        report.files_processed,
        report.files_skipped,
        report.files_failed,
        report.vectors_upserted,
        report.vectors_deleted
    );
    if let Some(ref sha) = report.hwm_advanced_to {
        eprintln!("HWM advanced to {sha}");
    }

    Ok(())
}

async fn run_dry_run(config: &Config) -> Result<()> {
    let root = std::env::current_dir()?;
    let chunker = TreeSitterChunker::new();

    eprintln!("Scanning repository...");
    let report = cost::dry_run(&chunker, &root)?;
    let rates = ProviderRates::for_models(&config.summarizer.model, &config.embed.model);
    let report = report.with_cost(&rates);

    report.display(&config.summarizer.model, &config.embed.model);
    Ok(())
}
