use anyhow::Result;
use clap::Args;

use crate::config::Config;
use crate::embed::build_embedder;
use crate::indexer::resolve_namespace;
use crate::search::output;
use crate::search::{SearchParams, SearchRun};
use crate::store::build_store;

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

    /// Limit search to a subtree (path prefix)
    #[arg(long)]
    pub scope: Option<String>,

    /// Human-readable output instead of JSON
    #[arg(long)]
    pub pretty: bool,
}

pub async fn run(args: SearchArgs) -> Result<()> {
    let config = Config::new()?;
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
    };

    let search = SearchRun::new(embedder, store, namespace);
    let report = search.run(&params).await?;

    let rendered = if args.pretty {
        output::render_pretty(&report)
    } else {
        output::render_json(&report)?
    };
    print!("{rendered}");
    Ok(())
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
            scope: Some("src/finance/".into()),
            pretty: false,
        };
        let params = SearchParams {
            query: args.query.clone(),
            top_k: args.top_k,
            symbols_per_file: args.symbols_per_file,
            no_symbols: args.no_symbols,
            scope: args.scope.clone(),
        };
        assert_eq!(params.query, "find commission payments");
        assert_eq!(params.top_k, 10);
        assert_eq!(params.symbols_per_file, 5);
        assert!(params.no_symbols);
        assert_eq!(params.scope.as_deref(), Some("src/finance/"));
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
        set_env("WDPKR_NAMESPACE", "test-repo");
        set_env("VOYAGE_API_KEY", "fake-key");
        let args = SearchArgs {
            query: "test query".into(),
            top_k: 5,
            symbols_per_file: 3,
            no_symbols: false,
            scope: None,
            pretty: false,
        };
        let err = run(args).await.unwrap_err();
        assert!(
            err.to_string().contains("TURBOPUFFER_API_KEY"),
            "should fail on missing store credential; got: {err}"
        );
        clear_env();
    }
}
