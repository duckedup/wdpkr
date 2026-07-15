use std::sync::Arc;

use anyhow::{Result, bail};
use clap::Args;

use owo_colors::{OwoColorize, Stream};

use crate::config::Config;
use crate::embed::build_embedder;
use crate::indexer::cost::{self, ProviderRates};
use crate::indexer::pipeline::EmbedMode;
use crate::indexer::{IndexRun, resolve_namespace};
use crate::store::build_store;
use crate::summarize::Summarizer;
use crate::summarize::build_summarizer;
use crate::tap::build_taps;

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

    /// Re-chunk all files and rebuild call-graph edges without re-summarizing
    /// or re-embedding. Zero API calls — uses existing vectors from the store.
    #[arg(long)]
    pub skip_summaries: bool,

    /// Embed code documentation (docstring + signature) instead of LLM
    /// summaries. Skips the summarizer entirely — zero Anthropic cost.
    /// Overrides the `embedder.embed_mode` config.
    #[arg(long)]
    pub docstring: bool,

    /// Run only this configured tap (default: all)
    #[arg(long)]
    pub tap: Option<String>,

    /// Target document(s) to index for a targeted tap, as a page ID or URL
    /// (repeatable). Used by the notion tap: `--tap notion --doc <id-or-url>`.
    /// Additive — only the named documents are (re)indexed. Ignored by taps
    /// that don't support targeting (files, linear).
    #[arg(long, action = clap::ArgAction::Append)]
    pub doc: Vec<String>,
}

pub async fn run(args: IndexArgs) -> Result<()> {
    if args.from.is_some() {
        bail!("--from is not yet implemented");
    }

    let config = Config::new()?;

    if args.dry_run {
        return run_dry_run(&config).await;
    }

    if args.skip_summaries {
        return run_skip_summaries(&config).await;
    }

    let mode = if args.docstring {
        EmbedMode::Docstring
    } else {
        EmbedMode::from_config(&config.embed.embed_mode)
    };

    config.store.validate()?;
    config.embed.validate()?;
    if mode == EmbedMode::Summary {
        config.summarizer.validate()?;
    }

    let namespace = resolve_namespace(&config)?;
    // Docstring mode skips the LLM entirely — no summarizer is built.
    let summarizer: Option<Arc<dyn Summarizer>> = match mode {
        EmbedMode::Summary => Some(Arc::from(build_summarizer(&config.summarizer)?)),
        EmbedMode::Docstring => None,
    };
    let embedder = build_embedder(&config.embed).await?;
    let store = build_store(&config.store, embedder.dimension())?;

    eprintln!(
        "Indexing into namespace '{}' with {}/{} ({} mode)...",
        namespace
            .as_str()
            .if_supports_color(Stream::Stderr, |s| s.cyan()),
        embedder.provider_name(),
        embedder.model_name(),
        mode.as_str(),
    );

    let root = std::env::current_dir()?;
    let taps = build_taps(&config.taps, root, args.tap.as_deref(), &args.doc)?;
    let index_run = IndexRun::new(
        taps,
        summarizer,
        Arc::from(embedder),
        Arc::from(store),
        namespace,
        args.concurrency,
        mode,
    );
    let report = index_run.run(args.full).await?;

    let elapsed = format!("{:.1}s", report.elapsed.as_secs_f64());
    eprintln!(
        "\nDone in {}: {} processed, {} skipped (unchanged), {} failed, {} vectors upserted, {} deleted",
        elapsed.if_supports_color(Stream::Stderr, |s| s.cyan()),
        report
            .files_processed
            .if_supports_color(Stream::Stderr, |s| s.green()),
        report
            .files_skipped
            .if_supports_color(Stream::Stderr, |s| s.yellow()),
        report
            .files_failed
            .if_supports_color(Stream::Stderr, |s| s.red()),
        report
            .vectors_upserted
            .if_supports_color(Stream::Stderr, |s| s.green()),
        report
            .vectors_deleted
            .if_supports_color(Stream::Stderr, |s| s.yellow()),
    );
    if let Some(ref sha) = report.hwm_advanced_to {
        eprintln!(
            "HWM advanced to {}",
            sha.if_supports_color(Stream::Stderr, |s| s.dimmed())
        );
    }

    Ok(())
}

async fn run_skip_summaries(config: &Config) -> Result<()> {
    use crate::chunk::tree_sitter::TreeSitterChunker;
    use crate::chunk::{Chunker, detect_language};
    use crate::indexer::{resolve_call_edges, walk};
    use crate::store::ChunkKind;
    use std::collections::HashMap;
    use std::time::Instant;

    let start = Instant::now();
    let root = std::env::current_dir()?;
    let namespace = resolve_namespace(config)?;
    let embedder = build_embedder(&config.embed).await?;
    let store = build_store(&config.store, embedder.dimension())?;

    eprintln!(
        "Rebuilding call graph for namespace '{}'...",
        namespace
            .as_str()
            .if_supports_color(Stream::Stderr, |s| s.cyan()),
    );

    if !store.namespace_exists(&namespace).await? {
        bail!(
            "namespace '{}' does not exist; run `wdpkr index` first",
            namespace.as_str()
        );
    }

    eprintln!("  Fetching existing documents from store...");
    let mut documents = store.list_documents(&namespace).await?;
    eprintln!(
        "  {} existing documents",
        documents
            .len()
            .if_supports_color(Stream::Stderr, |s| s.cyan()),
    );

    eprintln!("  Chunking all files for call references...");
    let chunker = TreeSitterChunker::new();
    let files = walk::walk_files(&root)?;
    let mut ref_table: HashMap<(String, String), Vec<String>> = HashMap::new();

    for path in &files {
        let rel_path = path
            .strip_prefix(&root)
            .map(|r| r.to_string_lossy().to_string())
            .unwrap_or_default();
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let language = detect_language(&rel_path).unwrap_or("unknown");
        if let Ok(chunks) = chunker.chunk(&rel_path, &content, language) {
            for sym in chunks.symbols {
                ref_table.insert((rel_path.clone(), sym.name.clone()), sym.references);
            }
        }
    }

    let mut updated = 0usize;
    for doc in &mut documents {
        if doc.chunk_kind != ChunkKind::Symbol {
            continue;
        }
        if let Some(ref name) = doc.symbol_name {
            let key = (doc.file_path.clone(), name.clone());
            if let Some(refs) = ref_table.get(&key) {
                doc.calls = Some(refs.clone());
                updated += 1;
            }
        }
    }

    eprintln!(
        "  {} symbols updated with call references",
        updated.if_supports_color(Stream::Stderr, |s| s.green()),
    );

    resolve_call_edges(&mut documents);

    store.upsert(&namespace, &documents).await?;

    let elapsed = format!("{:.1}s", start.elapsed().as_secs_f64());
    eprintln!(
        "\nDone in {}: {} documents re-upserted with call graph data",
        elapsed.if_supports_color(Stream::Stderr, |s| s.cyan()),
        documents
            .len()
            .if_supports_color(Stream::Stderr, |s| s.green()),
    );

    Ok(())
}

async fn run_dry_run(config: &Config) -> Result<()> {
    use crate::chunk::tree_sitter::TreeSitterChunker;
    use crate::tap::{FetchContext, build_tap};
    use std::collections::HashMap;

    let root = std::env::current_dir()?;
    let chunker = TreeSitterChunker::new();

    eprintln!("Scanning repository...");
    let mut report = cost::dry_run(&chunker, &root)?;

    // Fold in the Linear tap if it's configured. Fetching issue text is a free
    // metadata read (no LLM/embed calls); if the API key is missing we skip the
    // Linear estimate so --dry-run still works for code alone.
    if let Some(tap_cfg) = config.taps.iter().find(|t| t.name == "linear") {
        match build_tap(tap_cfg, root.clone(), &[]) {
            Ok(tap) => {
                eprintln!("Fetching Linear issues for estimate...");
                let ctx = FetchContext {
                    full: true,
                    cursor: None,
                    stored_hashes: HashMap::new(),
                };
                let result = tap.fetch(&ctx).await?;
                report.merge_linear(cost::estimate_linear(&result.items));
            }
            Err(e) => eprintln!("Skipping Linear cost estimate: {e}"),
        }
    }

    let rates = ProviderRates::for_models(&config.summarizer.model, &config.embed.model);
    let report = report.with_cost(&rates);

    report.display(&config.summarizer.model, &config.embed.model);
    Ok(())
}
