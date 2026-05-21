use anyhow::Result;
use clap::Args;
use owo_colors::{OwoColorize, Stream};

use crate::config::Config;
use crate::embed::build_embedder;
use crate::indexer::resolve_namespace;
use crate::store::build_store;

#[derive(Args, Debug)]
pub struct DeleteArgs {
    /// Glob pattern matching file paths to remove from the index
    pub pattern: String,
}

pub async fn run(args: DeleteArgs) -> Result<()> {
    let config = Config::new()?;
    let namespace = resolve_namespace(&config)?;
    let embedder = build_embedder(&config.embed).await?;
    let store = build_store(&config.store, embedder.dimension())?;

    let deleted = store.delete_by_glob(&namespace, &args.pattern).await?;

    eprintln!(
        "Deleted {} vectors matching '{}'",
        deleted.if_supports_color(Stream::Stderr, |s| s.green()),
        args.pattern.if_supports_color(Stream::Stderr, |s| s.cyan()),
    );

    Ok(())
}
