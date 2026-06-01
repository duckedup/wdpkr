//! Indexer: runs taps through the summarize → embed → upsert pipeline.

pub mod cost;
pub mod docstring;
pub mod git;
pub mod pipeline;
pub mod walk;

use std::sync::Arc;
use std::time::Instant;

use anyhow::{Result, bail};
use owo_colors::{OwoColorize, Stream};
use tokio::sync::Semaphore;

use crate::embed::{Embedder, embedder_identity};
use crate::indexer::pipeline::EmbedMode;
use crate::store::{ChunkKind, Namespace, NamespaceMetadata, VectorDocument, VectorStore};
use crate::summarize::Summarizer;
use crate::tap::{FetchContext, Tap, namespace_suffix};

/// Metadata key under which the embed mode is recorded per namespace.
const EMBED_MODE_META_KEY: &str = "embed_mode";

pub struct IndexRun {
    taps: Vec<Arc<dyn Tap>>,
    /// `None` in docstring mode — no summarizer is built or called.
    summarizer: Option<Arc<dyn Summarizer>>,
    embedder: Arc<dyn Embedder>,
    store: Arc<dyn VectorStore>,
    base_namespace: Namespace,
    concurrency: usize,
    mode: EmbedMode,
}

#[derive(Debug)]
pub struct IndexReport {
    pub files_processed: usize,
    pub files_failed: usize,
    pub files_skipped: usize,
    pub vectors_upserted: usize,
    pub vectors_deleted: usize,
    pub hwm_advanced_to: Option<String>,
    pub elapsed: std::time::Duration,
}

impl IndexRun {
    pub fn new(
        taps: Vec<Arc<dyn Tap>>,
        summarizer: Option<Arc<dyn Summarizer>>,
        embedder: Arc<dyn Embedder>,
        store: Arc<dyn VectorStore>,
        base_namespace: Namespace,
        concurrency: usize,
        mode: EmbedMode,
    ) -> Self {
        Self {
            taps,
            summarizer,
            embedder,
            store,
            base_namespace,
            concurrency: concurrency.max(1),
            mode,
        }
    }

    fn tap_namespace(&self, tap_name: &str) -> Namespace {
        match namespace_suffix(tap_name) {
            None => self.base_namespace.clone(),
            Some(suffix) => Namespace::from(format!("{}{suffix}", self.base_namespace.as_str())),
        }
    }

    pub async fn run(&self, full: bool) -> Result<IndexReport> {
        let start = Instant::now();
        let mut total_processed = 0usize;
        let mut total_failed = 0usize;
        let mut total_upserted = 0usize;
        let mut total_deleted = 0usize;
        let mut last_cursor: Option<String> = None;

        for tap in &self.taps {
            let ns = self.tap_namespace(tap.name());

            if !self.store.namespace_exists(&ns).await? {
                self.store
                    .create_namespace(&ns, self.embedder.dimension())
                    .await?;
            }

            let meta = self.store.get_metadata(&ns).await?;

            if let Some(ref stored) = meta.embedder
                && !full
            {
                let current = embedder_identity(self.embedder.as_ref());
                if stored != &current {
                    bail!(
                        "embedder mismatch: index was built with {stored}, \
                         but indexer is configured for {current}; \
                         run with --full to reindex"
                    );
                }
            }

            if let Some(stored_mode) = meta.extra.get(EMBED_MODE_META_KEY)
                && !full
                && stored_mode != self.mode.as_str()
            {
                bail!(
                    "embed mode mismatch: index was built in '{stored_mode}' mode, \
                     but indexer is configured for '{}'; \
                     run with --full to reindex",
                    self.mode.as_str()
                );
            }

            let stored_hashes = self.store.get_content_hashes(&ns).await.unwrap_or_default();
            let ctx = FetchContext {
                full,
                cursor: meta.hwm_sha.clone(),
                stored_hashes,
            };

            let fetch_result = tap.fetch(&ctx).await?;
            let cursor = fetch_result.cursor;

            for path in &fetch_result.deletions {
                self.store.delete_by_file(&ns, path).await?;
                total_deleted += 1;
            }

            let items = fetch_result.items;
            let total = items.len();
            eprintln!(
                "  [{}] {} items to process (concurrency: {})",
                tap.name()
                    .if_supports_color(Stream::Stderr, |s| s.magenta()),
                total.if_supports_color(Stream::Stderr, |s| s.cyan()),
                self.concurrency
                    .if_supports_color(Stream::Stderr, |s| s.cyan()),
            );

            let semaphore = Arc::new(Semaphore::new(self.concurrency));
            let mut join_set = tokio::task::JoinSet::new();

            for (i, item) in items.into_iter().enumerate() {
                let permit = semaphore.clone().acquire_owned().await?;
                let summarizer = self.summarizer.clone();
                let embedder = self.embedder.clone();
                let store = self.store.clone();
                let ns = ns.clone();
                let mode = self.mode;

                join_set.spawn(async move {
                    let source_path = item.source_path.clone();
                    let idx = format!("{:>4}/{}", i + 1, total);

                    let outcome = match pipeline::process_item(
                        &item,
                        summarizer.as_deref(),
                        embedder.as_ref(),
                        mode,
                    )
                    .await
                    {
                        Ok(result) => {
                            let t = &result.timing;
                            let syms = format!("({} symbols)", result.symbol_count);
                            let timing = format!(
                                "summarize: {:.1}s, embed: {:.1}s",
                                t.summarize.as_secs_f64(),
                                t.embed.as_secs_f64(),
                            );
                            eprintln!(
                                "  [{}] {} {} {} {}",
                                idx.if_supports_color(Stream::Stderr, |s| s.cyan()),
                                source_path,
                                syms.if_supports_color(Stream::Stderr, |s| s.green()),
                                "—".if_supports_color(Stream::Stderr, |s| s.dimmed()),
                                timing.if_supports_color(Stream::Stderr, |s| s.yellow()),
                            );
                            if let Err(e) = store.delete_by_file(&ns, &source_path).await {
                                eprintln!(
                                    "  [{}] {} {} {}",
                                    idx.if_supports_color(Stream::Stderr, |s| s.cyan()),
                                    source_path,
                                    "—".if_supports_color(Stream::Stderr, |s| s.dimmed()),
                                    format!("pre-upsert delete error: {e}")
                                        .if_supports_color(Stream::Stderr, |s| s.red()),
                                );
                                ItemOutcome::Failed
                            } else {
                                ItemOutcome::Processed {
                                    documents: result.documents,
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!(
                                "  [{}] {} {} {}",
                                idx.if_supports_color(Stream::Stderr, |s| s.cyan()),
                                source_path,
                                "—".if_supports_color(Stream::Stderr, |s| s.dimmed()),
                                format!("error: {e}")
                                    .if_supports_color(Stream::Stderr, |s| s.red()),
                            );
                            ItemOutcome::Failed
                        }
                    };
                    drop(permit);
                    outcome
                });
            }

            let mut tap_documents: Vec<VectorDocument> = Vec::new();
            while let Some(join_result) = join_set.join_next().await {
                match join_result? {
                    ItemOutcome::Processed { documents } => {
                        total_processed += 1;
                        tap_documents.extend(documents);
                    }
                    ItemOutcome::Failed => {
                        total_failed += 1;
                    }
                }
            }

            resolve_call_edges(&mut tap_documents);

            let stats = self.store.upsert(&ns, &tap_documents).await?;
            total_upserted += stats.upserted;

            let mut extra = std::collections::HashMap::new();
            extra.insert(
                EMBED_MODE_META_KEY.to_string(),
                self.mode.as_str().to_string(),
            );
            let new_meta = NamespaceMetadata {
                hwm_sha: cursor.clone(),
                embedder: Some(embedder_identity(self.embedder.as_ref())),
                extra,
            };
            self.store.set_metadata(&ns, &new_meta).await?;

            last_cursor = cursor;
        }

        Ok(IndexReport {
            files_processed: total_processed,
            files_failed: total_failed,
            files_skipped: 0,
            vectors_upserted: total_upserted,
            vectors_deleted: total_deleted,
            hwm_advanced_to: last_cursor,
            elapsed: start.elapsed(),
        })
    }
}

enum ItemOutcome {
    Processed { documents: Vec<VectorDocument> },
    Failed,
}

pub fn resolve_call_edges(documents: &mut [VectorDocument]) {
    use std::collections::HashMap;

    let mut symbol_table: HashMap<&str, Vec<(usize, &str)>> = HashMap::new();
    for (i, doc) in documents.iter().enumerate() {
        if doc.chunk_kind == ChunkKind::Symbol
            && let Some(ref name) = doc.symbol_name
        {
            symbol_table
                .entry(name.as_str())
                .or_default()
                .push((i, doc.file_path.as_str()));
        }
    }

    let mut called_by_map: HashMap<usize, Vec<String>> = HashMap::new();
    for (i, doc) in documents.iter().enumerate() {
        if doc.chunk_kind != ChunkKind::Symbol {
            continue;
        }
        if let Some(ref calls) = doc.calls {
            let caller_name = doc.symbol_name.as_deref().unwrap_or("?");
            let caller_ref = format!("{}:{}", doc.file_path, caller_name);
            for call_name in calls {
                if let Some(targets) = symbol_table.get(call_name.as_str()) {
                    for &(target_idx, _) in targets {
                        if target_idx != i {
                            called_by_map
                                .entry(target_idx)
                                .or_default()
                                .push(caller_ref.clone());
                        }
                    }
                }
            }
        }
    }

    let resolved_calls: Vec<(usize, Vec<String>)> = documents
        .iter()
        .enumerate()
        .filter(|(_, doc)| doc.chunk_kind == ChunkKind::Symbol && doc.calls.is_some())
        .map(|(i, doc)| {
            let resolved: Vec<String> = doc
                .calls
                .as_ref()
                .unwrap()
                .iter()
                .flat_map(|name| {
                    symbol_table
                        .get(name.as_str())
                        .into_iter()
                        .flatten()
                        .map(move |&(_, file)| format!("{file}:{name}"))
                })
                .collect();
            (i, resolved)
        })
        .collect();

    drop(symbol_table);

    for (i, resolved) in resolved_calls {
        documents[i].calls = Some(resolved);
    }
    for (i, doc) in documents.iter_mut().enumerate() {
        if doc.chunk_kind == ChunkKind::Symbol {
            doc.called_by = Some(called_by_map.remove(&i).unwrap_or_default());
        }
    }
}

pub fn resolve_namespace(config: &crate::config::Config) -> Result<Namespace> {
    let ns = &config.indexer.namespace;
    if ns.is_empty() {
        let remote = git::remote_url(&std::env::current_dir()?, &config.indexer.git_remote)?;
        Ok(Namespace::from(git::derive_namespace(&remote)))
    } else {
        Ok(Namespace::from(ns.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tap::SourceItem;
    use crate::testing::mock_embed::MockEmbedder;
    use crate::testing::mock_store::MockVectorStore;
    use crate::testing::mock_summarize::MockSummarizer;
    use crate::testing::mock_tap::MockTap;

    fn sym_doc(id: &str, file: &str, name: &str, calls: Vec<&str>) -> VectorDocument {
        VectorDocument {
            id: id.into(),
            vector: vec![0.0; 3],
            summary: String::new(),
            file_path: file.into(),
            chunk_kind: ChunkKind::Symbol,
            symbol_name: Some(name.into()),
            symbol_kind: Some("function".into()),
            start_line: Some(1),
            end_line: Some(10),
            language: Some("rust".into()),
            content_hash: None,
            calls: Some(calls.into_iter().map(String::from).collect()),
            called_by: None,
        }
    }

    fn sample_items() -> Vec<SourceItem> {
        vec![
            SourceItem {
                source_path: "src/main.rs".into(),
                content: "pub fn hello() {}".into(),
                content_hash: "hash1".into(),
                language: Some("rust".into()),
                module_doc: None,
                children: vec![],
            },
            SourceItem {
                source_path: "src/lib.rs".into(),
                content: "pub fn lib_fn() {}".into(),
                content_hash: "hash2".into(),
                language: Some("rust".into()),
                module_doc: None,
                children: vec![],
            },
        ]
    }

    // ── resolve_call_edges tests ─────────────────────────────────────

    #[test]
    fn resolve_populates_calls_and_called_by() {
        let mut docs = vec![
            sym_doc("s1", "src/a.rs", "orchestrate", vec!["validate", "process"]),
            sym_doc("s2", "src/b.rs", "validate", vec![]),
            sym_doc("s3", "src/b.rs", "process", vec!["validate"]),
        ];

        resolve_call_edges(&mut docs);

        let orchestrate = &docs[0];
        let calls = orchestrate.calls.as_ref().unwrap();
        assert!(
            calls.contains(&"src/b.rs:validate".to_string()),
            "calls: {calls:?}"
        );
        assert!(
            calls.contains(&"src/b.rs:process".to_string()),
            "calls: {calls:?}"
        );

        let validate = &docs[1];
        let called_by = validate.called_by.as_ref().unwrap();
        assert!(
            called_by.contains(&"src/a.rs:orchestrate".to_string()),
            "called_by: {called_by:?}"
        );
        assert!(
            called_by.contains(&"src/b.rs:process".to_string()),
            "called_by: {called_by:?}"
        );

        let process = &docs[2];
        let called_by = process.called_by.as_ref().unwrap();
        assert!(
            called_by.contains(&"src/a.rs:orchestrate".to_string()),
            "called_by: {called_by:?}"
        );
    }

    #[test]
    fn resolve_drops_unresolved_calls() {
        let mut docs = vec![sym_doc(
            "s1",
            "src/a.rs",
            "main",
            vec!["println", "nonexistent"],
        )];

        resolve_call_edges(&mut docs);

        let calls = docs[0].calls.as_ref().unwrap();
        assert!(calls.is_empty(), "unresolved should be dropped: {calls:?}");
    }

    #[test]
    fn resolve_sets_empty_called_by_for_leaf_symbols() {
        let mut docs = vec![sym_doc("s1", "src/a.rs", "leaf", vec![])];

        resolve_call_edges(&mut docs);

        assert_eq!(docs[0].called_by, Some(vec![]));
    }

    #[test]
    fn resolve_skips_file_level_documents() {
        let mut docs = vec![crate::testing::sample_document("src/a.rs", ChunkKind::File)];

        resolve_call_edges(&mut docs);

        assert!(docs[0].calls.is_none());
        assert!(docs[0].called_by.is_none());
    }

    // ── tap_namespace tests ───────────────────────────────────────

    #[test]
    fn tap_namespace_files_uses_base() {
        let run = IndexRun::new(
            vec![],
            Some(Arc::new(MockSummarizer::new()) as Arc<dyn Summarizer>),
            Arc::new(MockEmbedder::new(8)),
            Arc::new(MockVectorStore::new()),
            Namespace::from("my-repo"),
            1,
            EmbedMode::Summary,
        );
        assert_eq!(run.tap_namespace("files").as_str(), "my-repo");
    }

    #[test]
    fn tap_namespace_other_appends_suffix() {
        let run = IndexRun::new(
            vec![],
            Some(Arc::new(MockSummarizer::new()) as Arc<dyn Summarizer>),
            Arc::new(MockEmbedder::new(8)),
            Arc::new(MockVectorStore::new()),
            Namespace::from("my-repo"),
            1,
            EmbedMode::Summary,
        );
        assert_eq!(run.tap_namespace("linear").as_str(), "my-repo--linear");
    }

    // ── IndexRun integration tests ───────────────────────────────────

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn index_run_processes_items_from_tap() {
        let store = Arc::new(MockVectorStore::new());
        let tap: Arc<dyn Tap> = Arc::new(MockTap::new("files", sample_items()));
        let run = IndexRun::new(
            vec![tap],
            Some(Arc::new(MockSummarizer::new()) as Arc<dyn Summarizer>),
            Arc::new(MockEmbedder::new(8)),
            store.clone(),
            Namespace::from("test"),
            1,
            EmbedMode::Summary,
        );
        let report = run.run(true).await.unwrap();

        assert_eq!(report.files_processed, 2);
        assert_eq!(report.files_failed, 0);
        assert!(report.vectors_upserted > 0);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn index_run_handles_deletions() {
        let store = Arc::new(MockVectorStore::new());
        store
            .create_namespace(&Namespace::from("test"), 8)
            .await
            .unwrap();

        let doc = VectorDocument {
            id: "to-delete".into(),
            vector: vec![0.1; 8],
            summary: "old doc".into(),
            file_path: "deleted.rs".into(),
            chunk_kind: ChunkKind::File,
            symbol_name: None,
            symbol_kind: None,
            start_line: None,
            end_line: None,
            language: Some("rust".into()),
            content_hash: None,
            calls: None,
            called_by: None,
        };
        store
            .upsert(&Namespace::from("test"), &[doc])
            .await
            .unwrap();

        let tap: Arc<dyn Tap> = Arc::new(MockTap::with_deletions(
            "files",
            vec![],
            vec!["deleted.rs".into()],
        ));
        let run = IndexRun::new(
            vec![tap],
            Some(Arc::new(MockSummarizer::new()) as Arc<dyn Summarizer>),
            Arc::new(MockEmbedder::new(8)),
            store.clone(),
            Namespace::from("test"),
            1,
            EmbedMode::Summary,
        );
        let report = run.run(true).await.unwrap();

        assert_eq!(report.vectors_deleted, 1);
        let docs = store
            .list_documents(&Namespace::from("test"))
            .await
            .unwrap();
        assert!(
            docs.iter().all(|d| d.file_path != "deleted.rs"),
            "deleted.rs should be removed"
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn index_run_multiple_taps_separate_namespaces() {
        let store = Arc::new(MockVectorStore::new());
        let files_tap: Arc<dyn Tap> = Arc::new(MockTap::new(
            "files",
            vec![SourceItem {
                source_path: "a.rs".into(),
                content: "fn a() {}".into(),
                content_hash: "h1".into(),
                language: Some("rust".into()),
                module_doc: None,
                children: vec![],
            }],
        ));
        let linear_tap: Arc<dyn Tap> = Arc::new(MockTap::new(
            "linear",
            vec![SourceItem {
                source_path: "linear://ENG-1".into(),
                content: "Fix bug".into(),
                content_hash: "h2".into(),
                language: None,
                module_doc: None,
                children: vec![],
            }],
        ));
        let run = IndexRun::new(
            vec![files_tap, linear_tap],
            Some(Arc::new(MockSummarizer::new()) as Arc<dyn Summarizer>),
            Arc::new(MockEmbedder::new(8)),
            store.clone(),
            Namespace::from("repo"),
            1,
            EmbedMode::Summary,
        );
        let report = run.run(true).await.unwrap();

        assert!(
            store
                .namespace_exists(&Namespace::from("repo"))
                .await
                .unwrap()
        );
        assert!(
            store
                .namespace_exists(&Namespace::from("repo--linear"))
                .await
                .unwrap()
        );
        assert_eq!(report.files_processed, 2);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn index_run_persists_cursor_in_metadata() {
        let store = Arc::new(MockVectorStore::new());
        let tap: Arc<dyn Tap> = Arc::new(MockTap::with_cursor(
            "files",
            sample_items(),
            "abc123def".into(),
        ));
        let run = IndexRun::new(
            vec![tap],
            Some(Arc::new(MockSummarizer::new()) as Arc<dyn Summarizer>),
            Arc::new(MockEmbedder::new(8)),
            store.clone(),
            Namespace::from("test"),
            1,
            EmbedMode::Summary,
        );
        let report = run.run(true).await.unwrap();

        assert_eq!(report.hwm_advanced_to.as_deref(), Some("abc123def"));

        let meta = store.get_metadata(&Namespace::from("test")).await.unwrap();
        assert_eq!(meta.hwm_sha.as_deref(), Some("abc123def"));
        assert_eq!(meta.embedder.as_deref(), Some("mock/mock-embed-v1"));
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn index_run_embedder_mismatch_errors_on_incremental() {
        let store = Arc::new(MockVectorStore::new());
        store
            .create_namespace(&Namespace::from("test"), 8)
            .await
            .unwrap();
        store
            .set_metadata(
                &Namespace::from("test"),
                &NamespaceMetadata {
                    hwm_sha: Some("old-sha".into()),
                    embedder: Some("voyage/voyage-code-3".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let tap: Arc<dyn Tap> = Arc::new(MockTap::new("files", vec![]));
        let run = IndexRun::new(
            vec![tap],
            Some(Arc::new(MockSummarizer::new()) as Arc<dyn Summarizer>),
            Arc::new(MockEmbedder::new(8)),
            store,
            Namespace::from("test"),
            1,
            EmbedMode::Summary,
        );
        let err = run.run(false).await.unwrap_err();

        assert!(
            err.to_string().contains("embedder mismatch"),
            "expected embedder mismatch error, got: {err}"
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn index_run_embedder_mismatch_allowed_on_full() {
        let store = Arc::new(MockVectorStore::new());
        store
            .create_namespace(&Namespace::from("test"), 8)
            .await
            .unwrap();
        store
            .set_metadata(
                &Namespace::from("test"),
                &NamespaceMetadata {
                    hwm_sha: Some("old-sha".into()),
                    embedder: Some("voyage/voyage-code-3".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let tap: Arc<dyn Tap> = Arc::new(MockTap::new("files", sample_items()));
        let run = IndexRun::new(
            vec![tap],
            Some(Arc::new(MockSummarizer::new()) as Arc<dyn Summarizer>),
            Arc::new(MockEmbedder::new(8)),
            store,
            Namespace::from("test"),
            1,
            EmbedMode::Summary,
        );
        let report = run.run(true).await.unwrap();

        assert_eq!(report.files_processed, 2);
    }

    fn meta_with_mode(mode: &str) -> NamespaceMetadata {
        let mut extra = std::collections::HashMap::new();
        extra.insert(EMBED_MODE_META_KEY.to_string(), mode.to_string());
        NamespaceMetadata {
            hwm_sha: Some("old-sha".into()),
            embedder: Some("mock/mock-embed-v1".into()),
            extra,
        }
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn index_run_mode_mismatch_errors_on_incremental() {
        let store = Arc::new(MockVectorStore::new());
        store
            .create_namespace(&Namespace::from("test"), 8)
            .await
            .unwrap();
        store
            .set_metadata(&Namespace::from("test"), &meta_with_mode("summary"))
            .await
            .unwrap();

        let tap: Arc<dyn Tap> = Arc::new(MockTap::new("files", vec![]));
        let run = IndexRun::new(
            vec![tap],
            None,
            Arc::new(MockEmbedder::new(8)),
            store,
            Namespace::from("test"),
            1,
            EmbedMode::Docstring,
        );
        let err = run.run(false).await.unwrap_err();
        assert!(
            err.to_string().contains("embed mode mismatch"),
            "expected embed mode mismatch, got: {err}"
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn index_run_mode_mismatch_allowed_on_full() {
        let store = Arc::new(MockVectorStore::new());
        store
            .create_namespace(&Namespace::from("test"), 8)
            .await
            .unwrap();
        store
            .set_metadata(&Namespace::from("test"), &meta_with_mode("summary"))
            .await
            .unwrap();

        let tap: Arc<dyn Tap> = Arc::new(MockTap::new("files", sample_items()));
        let run = IndexRun::new(
            vec![tap],
            None,
            Arc::new(MockEmbedder::new(8)),
            store,
            Namespace::from("test"),
            1,
            EmbedMode::Docstring,
        );
        let report = run.run(true).await.unwrap();
        assert_eq!(report.files_processed, 2);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn index_run_persists_mode_in_metadata() {
        let store = Arc::new(MockVectorStore::new());
        let tap: Arc<dyn Tap> = Arc::new(MockTap::new("files", sample_items()));
        let run = IndexRun::new(
            vec![tap],
            None,
            Arc::new(MockEmbedder::new(8)),
            store.clone(),
            Namespace::from("test"),
            1,
            EmbedMode::Docstring,
        );
        run.run(true).await.unwrap();

        let meta = store.get_metadata(&Namespace::from("test")).await.unwrap();
        assert_eq!(
            meta.extra.get(EMBED_MODE_META_KEY).map(String::as_str),
            Some("docstring")
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn index_run_empty_tap_no_errors() {
        let store = Arc::new(MockVectorStore::new());
        let tap: Arc<dyn Tap> = Arc::new(MockTap::new("files", vec![]));
        let run = IndexRun::new(
            vec![tap],
            Some(Arc::new(MockSummarizer::new()) as Arc<dyn Summarizer>),
            Arc::new(MockEmbedder::new(8)),
            store,
            Namespace::from("test"),
            1,
            EmbedMode::Summary,
        );
        let report = run.run(true).await.unwrap();

        assert_eq!(report.files_processed, 0);
        assert_eq!(report.files_failed, 0);
        assert_eq!(report.vectors_upserted, 0);
    }
}
