use anyhow::Result;
use clap::Args;
use owo_colors::{OwoColorize, Stream};

use crate::config::Config;
use crate::embed::build_embedder;
use crate::indexer::{now_unix_secs, resolve_namespace};
use crate::store::{Namespace, build_store};
use crate::tap::namespace_suffix;

#[derive(Args, Debug)]
pub struct ReinforceArgs {
    /// Document ID(s) to reinforce, e.g. `notion://<page-id>` (repeatable).
    /// Bumps each document's `last_used_at` so per-tap decay treats it as fresh
    /// again — no re-embedding. The tap is inferred from the URI scheme; a bare
    /// path targets the files namespace.
    #[arg(required = true)]
    pub ids: Vec<String>,
}

pub async fn run(args: ReinforceArgs) -> Result<()> {
    let config = Config::new()?;
    config.store.validate()?;
    let base = resolve_namespace(&config)?;
    let embedder = build_embedder(&config.embed).await?;
    let store = build_store(&config.store, embedder.dimension())?;
    let now = now_unix_secs();

    let mut total = 0usize;
    let mut missing = 0usize;
    for id in &args.ids {
        let ns = reinforce_namespace(&base, id);
        let n = store.touch_by_file(&ns, id, now).await?;
        if n == 0 {
            missing += 1;
            eprintln!(
                "  {} {} not found in namespace '{}'",
                "—".if_supports_color(Stream::Stderr, |s| s.dimmed()),
                id.if_supports_color(Stream::Stderr, |s| s.yellow()),
                ns.as_str(),
            );
        } else {
            total += n;
            eprintln!(
                "  {} {} ({} vectors)",
                "reinforced".if_supports_color(Stream::Stderr, |s| s.green()),
                id.if_supports_color(Stream::Stderr, |s| s.cyan()),
                n,
            );
        }
    }

    eprintln!(
        "Reinforced {} vectors across {} document(s){}",
        total.if_supports_color(Stream::Stderr, |s| s.green()),
        args.ids.len() - missing,
        if missing > 0 {
            format!(" ({missing} not found)")
        } else {
            String::new()
        },
    );
    Ok(())
}

/// Namespace a document id lives in, inferred from its URI scheme
/// (`notion://…` → the notion tap namespace, `linear://…` → linear, etc.).
/// A bare path (no scheme) targets the base (files) namespace.
fn reinforce_namespace(base: &Namespace, id: &str) -> Namespace {
    match id.split_once("://") {
        Some((scheme, _)) => match namespace_suffix(scheme) {
            None => base.clone(),
            Some(suffix) => Namespace::from(format!("{}{suffix}", base.as_str())),
        },
        None => base.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notion_id_maps_to_notion_namespace() {
        let base = Namespace::from("repo");
        let ns = reinforce_namespace(&base, "notion://399cb3ca-dc95-80a1-a77f-d58e1248ded6");
        assert_eq!(ns.as_str(), "repo--notion");
    }

    #[test]
    fn linear_id_maps_to_linear_namespace() {
        let base = Namespace::from("repo");
        let ns = reinforce_namespace(&base, "linear://ENG-123");
        assert_eq!(ns.as_str(), "repo--linear");
    }

    #[test]
    fn bare_path_maps_to_base_namespace() {
        let base = Namespace::from("repo");
        let ns = reinforce_namespace(&base, "src/main.rs");
        assert_eq!(ns.as_str(), "repo");
    }
}
