//! Tap system for extensible data source indexing.
//!
//! The [`Tap`] trait defines the contract for any data source that wdpkr
//! can index. Built-in taps (files, future: linear, notion) implement
//! the trait directly in Rust; external taps communicate via a subprocess
//! JSON/stdio protocol through [`ProcessTap`](process::ProcessTap).

pub mod files;
pub mod process;

use std::collections::HashMap;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

// ── Trait ────────────────────────────────────────────────────────────────

#[async_trait]
pub trait Tap: Send + Sync {
    /// Unique name for this tap (e.g. "files", "linear").
    fn name(&self) -> &str;

    /// Fetch items to index. The tap reads its data source, applies
    /// change detection, and returns structured items for the shared
    /// summarize → embed → upsert pipeline.
    async fn fetch(&self, ctx: &FetchContext) -> Result<FetchResult>;
}

// ── Context & Result ─────────────────────────────────────────────────────

/// Input context passed to [`Tap::fetch`].
pub struct FetchContext {
    /// If true, ignore any stored cursor and fetch everything.
    pub full: bool,
    /// Opaque state from the previous run. For files: git SHA.
    /// For API-based taps: a timestamp or pagination token.
    pub cursor: Option<String>,
    /// Map of source_path → content_hash for skip detection.
    /// Taps can compare against this to avoid re-processing
    /// unchanged items.
    pub stored_hashes: HashMap<String, String>,
}

/// Output from [`Tap::fetch`].
#[derive(Debug)]
pub struct FetchResult {
    /// Items to process through the shared pipeline.
    pub items: Vec<SourceItem>,
    /// Source paths to delete from the store (e.g. deleted files,
    /// archived issues).
    pub deletions: Vec<String>,
    /// New cursor to persist for the next incremental run.
    pub cursor: Option<String>,
}

// ── Source types ──────────────────────────────────────────────────────────

/// A single document from a data source, ready for summarization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceItem {
    /// Unique identifier within this tap's domain. For files: the
    /// relative path (`src/main.rs`). For external sources: a URI
    /// (`linear://ENG-123`).
    pub source_path: String,
    /// Full text content to summarize at the document level.
    pub content: String,
    /// Hash of the content for change detection.
    pub content_hash: String,
    /// Programming language (if applicable).
    pub language: Option<String>,
    /// Sub-items: symbols for code, sections for documents, etc.
    #[serde(default)]
    pub children: Vec<SourceChunk>,
}

/// A sub-item within a [`SourceItem`]. For code: a function, struct, or
/// method. For documents: a section or heading.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceChunk {
    pub name: String,
    /// Normalized kind: "function", "struct", "section", etc.
    pub kind: String,
    /// Text content to summarize.
    pub content: String,
    /// Function/method signature (code only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
    /// Outbound references for call graph resolution (code only).
    #[serde(default)]
    pub references: Vec<String>,
}

// ── Registry ─────────────────────────────────────────────────────────────

/// Namespace suffix for a tap. Files tap uses no suffix (backward
/// compatible); all others get `--{name}` appended.
pub fn namespace_suffix(tap_name: &str) -> Option<String> {
    if tap_name == "files" {
        None
    } else {
        Some(format!("--{tap_name}"))
    }
}

/// Build a [] from a [`TapConfig`]. Built-in taps are
/// constructed directly; taps with a `command` field spawn a
/// subprocess via [`ProcessTap`](process::ProcessTap).
pub fn build_tap(
    cfg: &crate::config::TapConfig,
    root: std::path::PathBuf,
) -> Result<std::sync::Arc<dyn Tap>> {
    if let Some(ref command) = cfg.command {
        return Ok(std::sync::Arc::new(process::ProcessTap::new(
            cfg.name.clone(),
            command.clone(),
            cfg.args.clone(),
            cfg.settings.clone(),
        )));
    }
    match cfg.name.as_str() {
        "files" => Ok(std::sync::Arc::new(files::FilesTap::new(root))),
        other => anyhow::bail!(
            "unknown built-in tap '{other}'; \
             external taps must specify a 'command' field"
        ),
    }
}

/// Build all taps from config, optionally filtering to a single
/// tap by name.
pub fn build_taps(
    configs: &[crate::config::TapConfig],
    root: std::path::PathBuf,
    only: Option<&str>,
) -> Result<Vec<std::sync::Arc<dyn Tap>>> {
    let filtered: Vec<_> = match only {
        Some(name) => {
            let matching: Vec<_> = configs.iter().filter(|c| c.name == name).collect();
            if matching.is_empty() {
                anyhow::bail!(
                    "tap '{name}' is not configured; \
                     configured taps: {}",
                    configs
                        .iter()
                        .map(|c| c.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            matching
        }
        None => configs.iter().collect(),
    };
    filtered
        .into_iter()
        .map(|cfg| build_tap(cfg, root.clone()))
        .collect()
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::mock_tap::MockTap;

    fn _assert_send<T: Send>() {}
    fn _assert_sync<T: Sync>() {}

    #[test]
    fn trait_is_object_safe() {
        fn _takes_tap(_: &dyn Tap) {}
    }

    #[test]
    fn types_are_send_and_sync() {
        _assert_send::<FetchContext>();
        _assert_sync::<FetchContext>();
        _assert_send::<FetchResult>();
        _assert_sync::<FetchResult>();
        _assert_send::<SourceItem>();
        _assert_sync::<SourceItem>();
        _assert_send::<SourceChunk>();
        _assert_sync::<SourceChunk>();
    }

    #[test]
    fn namespace_suffix_files_is_none() {
        assert_eq!(namespace_suffix("files"), None);
    }

    #[test]
    fn namespace_suffix_other_taps() {
        assert_eq!(namespace_suffix("linear"), Some("--linear".into()));
        assert_eq!(namespace_suffix("notion"), Some("--notion".into()));
    }

    #[test]
    fn source_item_json_round_trip() {
        let item = SourceItem {
            source_path: "src/main.rs".into(),
            content: "fn main() {}".into(),
            content_hash: "abc123".into(),
            language: Some("rust".into()),
            children: vec![SourceChunk {
                name: "main".into(),
                kind: "function".into(),
                content: "fn main() {}".into(),
                signature: Some("fn main()".into()),
                start_line: Some(1),
                end_line: Some(1),
                references: vec!["println".into()],
            }],
        };
        let json = serde_json::to_string(&item).unwrap();
        let back: SourceItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back.source_path, "src/main.rs");
        assert_eq!(back.children.len(), 1);
        assert_eq!(back.children[0].name, "main");
        assert_eq!(back.children[0].references, vec!["println"]);
    }

    #[test]
    fn source_item_json_no_children() {
        let item = SourceItem {
            source_path: "linear://ENG-123".into(),
            content: "Fix the login bug".into(),
            content_hash: "def456".into(),
            language: None,
            children: vec![],
        };
        let json = serde_json::to_string(&item).unwrap();
        let back: SourceItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back.source_path, "linear://ENG-123");
        assert!(back.language.is_none());
        assert!(back.children.is_empty());
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn mock_tap_returns_configured_items() {
        let items = vec![SourceItem {
            source_path: "test.rs".into(),
            content: "fn test() {}".into(),
            content_hash: "hash1".into(),
            language: Some("rust".into()),
            children: vec![],
        }];
        let tap = MockTap::new("test", items);
        let ctx = FetchContext {
            full: true,
            cursor: None,
            stored_hashes: HashMap::new(),
        };
        let result = tap.fetch(&ctx).await.unwrap();
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].source_path, "test.rs");
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn mock_tap_skips_matching_hashes() {
        let items = vec![
            SourceItem {
                source_path: "a.rs".into(),
                content: "fn a() {}".into(),
                content_hash: "hash_a".into(),
                language: Some("rust".into()),
                children: vec![],
            },
            SourceItem {
                source_path: "b.rs".into(),
                content: "fn b() {}".into(),
                content_hash: "hash_b".into(),
                language: Some("rust".into()),
                children: vec![],
            },
        ];
        let tap = MockTap::new("test", items);
        let mut stored = HashMap::new();
        stored.insert("a.rs".into(), "hash_a".into());
        let ctx = FetchContext {
            full: false,
            cursor: None,
            stored_hashes: stored,
        };
        let result = tap.fetch(&ctx).await.unwrap();
        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].source_path, "b.rs");
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn mock_tap_full_ignores_hashes() {
        let items = vec![SourceItem {
            source_path: "a.rs".into(),
            content: "fn a() {}".into(),
            content_hash: "hash_a".into(),
            language: Some("rust".into()),
            children: vec![],
        }];
        let tap = MockTap::new("test", items);
        let mut stored = HashMap::new();
        stored.insert("a.rs".into(), "hash_a".into());
        let ctx = FetchContext {
            full: true,
            cursor: None,
            stored_hashes: stored,
        };
        let result = tap.fetch(&ctx).await.unwrap();
        assert_eq!(result.items.len(), 1, "full=true should skip hash check");
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn mock_tap_returns_deletions() {
        let tap = MockTap::with_deletions("test", vec![], vec!["deleted.rs".into()]);
        let ctx = FetchContext {
            full: false,
            cursor: None,
            stored_hashes: HashMap::new(),
        };
        let result = tap.fetch(&ctx).await.unwrap();
        assert!(result.items.is_empty());
        assert_eq!(result.deletions, vec!["deleted.rs"]);
    }

    #[test]
    fn mock_tap_name() {
        let tap = MockTap::new("my-source", vec![]);
        assert_eq!(tap.name(), "my-source");
    }

    // ── build_tap / build_taps tests ──────────────────────────

    use crate::config::TapConfig;

    fn files_config() -> TapConfig {
        TapConfig {
            name: "files".into(),
            command: None,
            args: vec![],
            settings: HashMap::new(),
        }
    }

    #[test]
    fn build_tap_files() {
        let cfg = files_config();
        let tap = build_tap(&cfg, std::path::PathBuf::from(".")).unwrap();
        assert_eq!(tap.name(), "files");
    }

    #[test]
    fn build_tap_unknown_errors() {
        let cfg = TapConfig {
            name: "nope".into(),
            command: None,
            args: vec![],
            settings: HashMap::new(),
        };
        let result = build_tap(&cfg, std::path::PathBuf::from("."));
        let err = result.err().expect("should error");
        assert!(
            err.to_string().contains("unknown built-in tap"),
            "got: {err}"
        );
    }

    #[test]
    fn build_tap_subprocess_creates_process_tap() {
        let cfg = TapConfig {
            name: "custom".into(),
            command: Some("/usr/bin/custom".into()),
            args: vec!["--flag".into()],
            settings: HashMap::new(),
        };
        let tap = build_tap(&cfg, std::path::PathBuf::from(".")).unwrap();
        assert_eq!(tap.name(), "custom");
    }

    #[test]
    fn build_taps_all() {
        let configs = vec![files_config()];
        let taps = build_taps(&configs, std::path::PathBuf::from("."), None).unwrap();
        assert_eq!(taps.len(), 1);
        assert_eq!(taps[0].name(), "files");
    }

    #[test]
    fn build_taps_filter_by_name() {
        let configs = vec![files_config()];
        let taps = build_taps(&configs, std::path::PathBuf::from("."), Some("files")).unwrap();
        assert_eq!(taps.len(), 1);
    }

    #[test]
    fn build_taps_filter_missing_errors() {
        let configs = vec![files_config()];
        let result = build_taps(&configs, std::path::PathBuf::from("."), Some("linear"));
        let err = result.err().expect("should error");
        assert!(err.to_string().contains("not configured"), "got: {err}");
    }
}
