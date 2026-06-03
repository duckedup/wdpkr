use anyhow::{Result, bail};
use clap::Args;

use crate::config::{Config, TapConfig};
use crate::embed::build_embedder;
use crate::indexer::resolve_namespace;
use crate::search::output;
use crate::search::{SearchParams, SearchRun};
use crate::store::{Namespace, build_store};
use crate::tap::namespace_suffix;

#[derive(Args, Debug)]
pub struct SearchArgs {
    /// Natural-language query to search the codebase index
    pub query: String,

    /// Max file-level results
    #[arg(short = 'k', long, default_value_t = 5)]
    pub top_k: usize,

    /// Max symbols returned per file
    #[arg(long, default_value_t = 3)]
    pub symbols_per_file: usize,

    /// File-level results only, omit symbol nesting
    #[arg(long)]
    pub no_symbols: bool,

    /// Limit search to subtree(s); repeatable
    #[arg(long, action = clap::ArgAction::Append)]
    pub scope: Vec<String>,

    /// Glob pattern to filter result paths (repeatable, OR logic)
    #[arg(long, action = clap::ArgAction::Append)]
    pub filter: Vec<String>,

    /// Limit search to these tap sources (e.g. `files`, `linear`); repeatable.
    /// Default: search all configured taps.
    #[arg(long, action = clap::ArgAction::Append)]
    pub provider: Vec<String>,

    /// Compact output: paths + one-sentence summaries, no symbols
    #[arg(long)]
    pub terse: bool,

    /// Human-readable output instead of JSON
    #[arg(long)]
    pub pretty: bool,
}

pub async fn run(args: SearchArgs) -> Result<()> {
    let config = Config::new()?;
    config.store.validate()?;
    config.embed.validate()?;

    let namespace = resolve_namespace(&config)?;
    let embedder = build_embedder(&config.embed).await?;
    let store = build_store(&config.store, embedder.dimension())?;

    let params = SearchParams {
        query: args.query.clone(),
        top_k: args.top_k,
        symbols_per_file: args.symbols_per_file,
        no_symbols: args.no_symbols,
        scope: args.scope.clone(),
        filters: args.filter.clone(),
    };

    let selected_taps = select_taps(&config.taps, &args.provider)?;
    let namespaces: Vec<(Namespace, Option<String>)> = selected_taps
        .iter()
        .map(|p| {
            let ns = match namespace_suffix(&p.name) {
                None => namespace.clone(),
                Some(suffix) => Namespace::from(format!("{}{suffix}", namespace.as_str())),
            };
            let source = if p.name == "files" {
                None
            } else {
                Some(p.name.clone())
            };
            (ns, source)
        })
        .collect();

    let search = SearchRun::new_multi(embedder, store, namespaces);
    let report = search.run(&params).await?;

    let rendered = if args.pretty {
        output::render_pretty(&report, args.terse)
    } else {
        output::render_json(&report, args.terse)?
    };
    print!("{rendered}");
    Ok(())
}

/// Filter configured taps to those named in `--provider`. An empty `providers`
/// selects all taps. Errors on an unknown provider name, listing what's
/// configured.
fn select_taps<'a>(taps: &'a [TapConfig], providers: &[String]) -> Result<Vec<&'a TapConfig>> {
    if providers.is_empty() {
        return Ok(taps.iter().collect());
    }
    let mut selected = Vec::new();
    for name in providers {
        match taps.iter().find(|t| &t.name == name) {
            Some(t) => selected.push(t),
            None => bail!(
                "unknown --provider '{name}'; configured taps: {}",
                taps.iter()
                    .map(|t| t.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_helpers::{remove_envs, set_env};
    use serial_test::serial;

    fn clear_env() {
        remove_envs(&[
            "WDPKR_STORE_PROVIDER",
            "WDPKR_EMBED_PROVIDER",
            "WDPKR_EMBED_MODEL",
            "WDPKR_NAMESPACE",
            "WDPKR_CONCURRENCY",
            "WDPKR_MAX_COST",
            "WDPKR_HWM_SUCCESS_THRESHOLD",
            "WDPKR_DEFAULT_BRANCH",
            "WDPKR_EMBED_BATCH_SIZE",
            "WDPKR_SUMMARIZER_PROVIDER",
            "WDPKR_SUMMARIZER_MODEL",
            "TURBOPUFFER_API_KEY",
            "VOYAGE_API_KEY",
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "OLLAMA_HOST",
            "XDG_CONFIG_HOME",
        ]);
    }

    #[test]
    fn args_to_params_mapping() {
        let args = SearchArgs {
            query: "find commission payments".into(),
            top_k: 10,
            symbols_per_file: 5,
            no_symbols: true,
            scope: vec!["src/finance/".into()],
            filter: vec![],
            provider: vec![],
            terse: false,
            pretty: false,
        };
        let params = SearchParams {
            query: args.query.clone(),
            top_k: args.top_k,
            symbols_per_file: args.symbols_per_file,
            no_symbols: args.no_symbols,
            scope: args.scope.clone(),
            filters: vec![],
        };
        assert_eq!(params.query, "find commission payments");
        assert_eq!(params.top_k, 10);
        assert_eq!(params.symbols_per_file, 5);
        assert!(params.no_symbols);
        assert_eq!(params.scope, vec!["src/finance/".to_string()]);
    }

    #[test]
    #[serial]
    fn resolve_namespace_from_config() {
        clear_env();
        set_env("WDPKR_NAMESPACE", "my-repo");
        let config = Config::from_file(None);
        let ns = resolve_namespace(&config).unwrap();
        assert_eq!(ns.as_str(), "my-repo");
        clear_env();
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    #[serial]
    fn resolve_namespace_derives_from_git_when_empty() {
        clear_env();
        let config = Config::from_file(None);
        let ns = resolve_namespace(&config).unwrap();
        assert!(
            ns.as_str().contains("wdpkr"),
            "should derive namespace from git remote; got: {}",
            ns.as_str()
        );
        clear_env();
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    #[serial]
    async fn run_fails_without_store_credentials() {
        clear_env();
        // Point config resolution at an empty dir so Config::new() can't pick
        // up a real ~/.config/wdpkr/config.yaml (which would supply a store
        // credential and let the run reach the embedder).
        let cfg_home = std::env::temp_dir().join(format!(
            "wdpkr-search-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&cfg_home).unwrap();
        set_env("XDG_CONFIG_HOME", cfg_home.to_str().unwrap());
        set_env("WDPKR_NAMESPACE", "test-repo");
        set_env("VOYAGE_API_KEY", "fake-key");
        let args = SearchArgs {
            query: "test query".into(),
            top_k: 5,
            symbols_per_file: 3,
            no_symbols: false,
            scope: vec![],
            filter: vec![],
            provider: vec![],
            terse: false,
            pretty: false,
        };
        let err = run(args).await.unwrap_err();
        assert!(
            err.to_string().contains("TURBOPUFFER_API_KEY"),
            "should fail on missing store credential; got: {err}"
        );
        clear_env();
        std::fs::remove_dir_all(&cfg_home).ok();
    }

    // ── select_taps (provider filter) ─────────────────────────────────

    fn taps(names: &[&str]) -> Vec<TapConfig> {
        names
            .iter()
            .map(|n| TapConfig {
                name: (*n).into(),
                command: None,
                args: vec![],
                settings: std::collections::HashMap::new(),
            })
            .collect()
    }

    #[test]
    fn select_taps_empty_provider_selects_all() {
        let configured = taps(&["files", "linear"]);
        let selected = select_taps(&configured, &[]).unwrap();
        assert_eq!(selected.len(), 2);
    }

    #[test]
    fn select_taps_filters_to_named() {
        let configured = taps(&["files", "linear"]);
        let selected = select_taps(&configured, &["linear".into()]).unwrap();
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].name, "linear");
    }

    #[test]
    fn select_taps_unknown_provider_errors() {
        let configured = taps(&["files", "linear"]);
        let err = select_taps(&configured, &["notion".into()]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown --provider 'notion'"), "got: {msg}");
        assert!(
            msg.contains("files") && msg.contains("linear"),
            "got: {msg}"
        );
    }
}
