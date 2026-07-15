use anyhow::{Result, bail};
use clap::Args;
use owo_colors::{OwoColorize, Stream};

use crate::config::{Config, TapConfig};
use crate::embed::build_embedder;
use crate::indexer::resolve_namespace;
use crate::store::{Namespace, build_store};
use crate::tap::namespace_suffix;

#[derive(Args, Debug)]
pub struct DeleteArgs {
    /// Glob pattern matching file paths to remove from the index.
    /// For non-files taps the paths are the tap URIs, e.g. `notion://<id>*`.
    pub pattern: String,

    /// Delete from this tap's namespace instead of the base (files)
    /// namespace, e.g. `--tap notion`. Default: the files namespace.
    #[arg(long)]
    pub tap: Option<String>,
}

pub async fn run(args: DeleteArgs) -> Result<()> {
    let config = Config::new()?;
    config.store.validate()?;
    let namespace = resolve_delete_namespace(&config, args.tap.as_deref())?;
    let embedder = build_embedder(&config.embed).await?;
    let store = build_store(&config.store, embedder.dimension())?;

    let deleted = store.delete_by_glob(&namespace, &args.pattern).await?;

    eprintln!(
        "Deleted {} vectors matching '{}' from namespace '{}'",
        deleted.if_supports_color(Stream::Stderr, |s| s.green()),
        args.pattern.if_supports_color(Stream::Stderr, |s| s.cyan()),
        namespace
            .as_str()
            .if_supports_color(Stream::Stderr, |s| s.cyan()),
    );

    Ok(())
}

/// Resolve which namespace `delete` targets. Without `--tap` (or with
/// `--tap files`) this is the base namespace; for another configured tap it's
/// the base namespace plus the tap's `--{name}` suffix. Errors if the named
/// tap is not configured, mirroring `search`.
fn resolve_delete_namespace(config: &Config, tap: Option<&str>) -> Result<Namespace> {
    let base = resolve_namespace(config)?;
    let Some(name) = tap else {
        return Ok(base);
    };
    if !config.taps.iter().any(|t: &TapConfig| t.name == name) {
        bail!(
            "unknown --tap '{name}'; configured taps: {}",
            config
                .taps
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(match namespace_suffix(name) {
        None => base,
        Some(suffix) => Namespace::from(format!("{}{suffix}", base.as_str())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config_with_taps(names: &[&str]) -> Config {
        let mut config = Config::from_file(None);
        config.indexer.namespace = "repo".into();
        config.taps = names
            .iter()
            .map(|n| TapConfig {
                name: (*n).into(),
                command: None,
                args: vec![],
                settings: HashMap::new(),
            })
            .collect();
        config
    }

    #[test]
    fn no_tap_uses_base_namespace() {
        let config = config_with_taps(&["files", "notion"]);
        let ns = resolve_delete_namespace(&config, None).unwrap();
        assert_eq!(ns.as_str(), "repo");
    }

    #[test]
    fn files_tap_uses_base_namespace() {
        let config = config_with_taps(&["files", "notion"]);
        let ns = resolve_delete_namespace(&config, Some("files")).unwrap();
        assert_eq!(ns.as_str(), "repo");
    }

    #[test]
    fn other_tap_appends_suffix() {
        let config = config_with_taps(&["files", "notion"]);
        let ns = resolve_delete_namespace(&config, Some("notion")).unwrap();
        assert_eq!(ns.as_str(), "repo--notion");
    }

    #[test]
    fn unknown_tap_errors() {
        let config = config_with_taps(&["files"]);
        let err = resolve_delete_namespace(&config, Some("notion")).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("unknown --tap 'notion'"), "got: {msg}");
        assert!(msg.contains("files"), "got: {msg}");
    }
}
