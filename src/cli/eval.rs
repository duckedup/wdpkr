use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Args;

use crate::config::Config;
use crate::embed::build_embedder;
use crate::eval::EvalSuite;
use crate::eval::output;
use crate::eval::runner::{EvalRunner, FsSourceReader};
use crate::indexer::resolve_namespace;
use crate::search::SearchRun;
use crate::store::build_store;

#[derive(Args, Debug)]
pub struct EvalArgs {
    /// Path to the eval suite JSON file
    #[arg(default_value = "eval/cases/wdpkr.json")]
    pub suite: String,

    /// Output full JSON results instead of summary table
    #[arg(long)]
    pub json: bool,

    /// Filter to cases matching a tag
    #[arg(long)]
    pub tag: Option<String>,
}

pub async fn run(args: EvalArgs) -> Result<()> {
    let suite_path = PathBuf::from(&args.suite);
    let raw = std::fs::read_to_string(&suite_path)
        .with_context(|| format!("reading suite file: {}", suite_path.display()))?;
    let mut suite: EvalSuite = serde_json::from_str(&raw).context("parsing eval suite JSON")?;

    if let Some(ref tag) = args.tag {
        suite.cases.retain(|c| c.tags.contains(tag));
    }

    if suite.cases.is_empty() {
        eprintln!("no eval cases to run");
        return Ok(());
    }

    let config = Config::new()?;
    config.embed.validate()?;

    let namespace = resolve_namespace(&config)?;
    let embedder = build_embedder(&config.embed).await?;
    let store = build_store(&config.store, embedder.dimension())?;

    let search = SearchRun::new(embedder, store, namespace);
    let root = std::env::current_dir().context("resolving repo root")?;
    let reader = FsSourceReader::new(root);

    let runner = EvalRunner::new(search, Box::new(reader));

    eprintln!("running {} cases…", suite.cases.len());
    let result = runner.run_suite(&suite).await?;

    if args.json {
        print!("{}", output::render_json(&result));
    } else {
        print!("{}", output::render_table(&result));
    }

    Ok(())
}
