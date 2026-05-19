//! Indexer: walks the repo, processes each file through the
//! chunk → summarize → embed → upsert pipeline, and advances the HWM.

pub mod cost;
pub mod git;
pub mod pipeline;
pub mod walk;

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Result, bail};
use owo_colors::{OwoColorize, Stream};
use tokio::sync::Semaphore;

use crate::chunk::Chunker;
use crate::embed::{Embedder, embedder_identity};
use crate::store::{ChunkKind, Namespace, NamespaceMetadata, VectorDocument, VectorStore};
use crate::summarize::Summarizer;

pub struct IndexRun {
    chunker: Arc<dyn Chunker>,
    summarizer: Arc<dyn Summarizer>,
    embedder: Arc<dyn Embedder>,
    store: Arc<dyn VectorStore>,
    namespace: Namespace,
    concurrency: usize,
}

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
        chunker: Arc<dyn Chunker>,
        summarizer: Arc<dyn Summarizer>,
        embedder: Arc<dyn Embedder>,
        store: Arc<dyn VectorStore>,
        namespace: Namespace,
        concurrency: usize,
    ) -> Self {
        Self {
            chunker,
            summarizer,
            embedder,
            store,
            namespace,
            concurrency: concurrency.max(1),
        }
    }

    /// Run the indexer against the given repo root.
    ///
    /// `full = true` ignores the HWM and walks all files. `full = false`
    /// diffs from the stored HWM to HEAD and processes only changed files.
    pub async fn run(&self, full: bool, root: &Path) -> Result<IndexReport> {
        let start = Instant::now();
        let head = git::current_sha(root)?;

        if !self.store.namespace_exists(&self.namespace).await? {
            self.store
                .create_namespace(&self.namespace, self.embedder.dimension())
                .await?;
        }

        let meta = self.store.get_metadata(&self.namespace).await?;

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

        let (to_process, to_delete) = match (&meta.hwm_sha, full) {
            (_, true) | (None, _) => {
                let files = walk::walk_files(root)?;
                let rel_paths: Vec<String> = files
                    .iter()
                    .filter_map(|p| {
                        p.strip_prefix(root)
                            .ok()
                            .map(|r| r.to_string_lossy().to_string())
                    })
                    .collect();
                (rel_paths, vec![])
            }
            (Some(hwm), false) => {
                let diff = git::diff_files(root, hwm, &head)?;
                (diff.changed, diff.deleted)
            }
        };

        let mut vectors_deleted = 0;
        for file_path in &to_delete {
            self.store
                .delete_by_file(&self.namespace, file_path)
                .await?;
            vectors_deleted += 1;
        }

        // Fetch stored content hashes for skip detection
        let stored_hashes = if full {
            self.store
                .get_content_hashes(&self.namespace)
                .await
                .unwrap_or_default()
        } else {
            std::collections::HashMap::new()
        };

        let total = to_process.len();
        eprintln!(
            "  {} files to process (concurrency: {})",
            total.if_supports_color(Stream::Stderr, |s| s.cyan()),
            self.concurrency
                .if_supports_color(Stream::Stderr, |s| s.cyan()),
        );

        let semaphore = Arc::new(Semaphore::new(self.concurrency));
        let mut join_set = tokio::task::JoinSet::new();

        for (i, rel_path) in to_process.into_iter().enumerate() {
            let permit = semaphore.clone().acquire_owned().await?;
            let abs_path = root.join(&rel_path);
            let chunker = self.chunker.clone();
            let summarizer = self.summarizer.clone();
            let embedder = self.embedder.clone();
            let store = self.store.clone();
            let namespace = self.namespace.clone();
            let stored_hashes = stored_hashes.clone();

            join_set.spawn(async move {
                let task = FileTask {
                    index: i,
                    total,
                    rel_path: &rel_path,
                    abs_path: &abs_path,
                    stored_hashes: &stored_hashes,
                    chunker: chunker.as_ref(),
                    summarizer: summarizer.as_ref(),
                    embedder: embedder.as_ref(),
                    store: store.as_ref(),
                    namespace: &namespace,
                };
                let result = process_one_file(&task).await;
                drop(permit);
                result
            });
        }

        let mut processed = 0usize;
        let mut failed = 0usize;
        let mut skipped = 0usize;
        let mut all_documents: Vec<VectorDocument> = Vec::new();

        while let Some(join_result) = join_set.join_next().await {
            let result = join_result?;
            match result.outcome {
                FileOutcome::Processed { documents } => {
                    processed += 1;
                    all_documents.extend(documents);
                }
                FileOutcome::Skipped => {
                    skipped += 1;
                }
                FileOutcome::Failed => {
                    failed += 1;
                }
            }
        }

        resolve_call_edges(&mut all_documents);

        let mut upserted = 0usize;
        for chunk in all_documents.chunks(200) {
            match self.store.upsert(&self.namespace, chunk).await {
                Ok(stats) => upserted += stats.upserted,
                Err(e) => {
                    eprintln!(
                        "  {} {}",
                        "batch upsert error:".if_supports_color(Stream::Stderr, |s| s.red()),
                        e,
                    );
                }
            }
        }

        let new_meta = NamespaceMetadata {
            hwm_sha: Some(head.clone()),
            embedder: Some(embedder_identity(self.embedder.as_ref())),
            ..Default::default()
        };
        self.store.set_metadata(&self.namespace, &new_meta).await?;

        Ok(IndexReport {
            files_processed: processed,
            files_failed: failed,
            files_skipped: skipped,
            vectors_upserted: upserted,
            vectors_deleted,
            hwm_advanced_to: Some(head),
            elapsed: start.elapsed(),
        })
    }
}

struct FileResult {
    outcome: FileOutcome,
}

enum FileOutcome {
    Processed { documents: Vec<VectorDocument> },
    Skipped,
    Failed,
}

struct FileTask<'a> {
    index: usize,
    total: usize,
    rel_path: &'a str,
    abs_path: &'a std::path::Path,
    stored_hashes: &'a std::collections::HashMap<String, String>,
    chunker: &'a dyn Chunker,
    summarizer: &'a dyn Summarizer,
    embedder: &'a dyn Embedder,
    store: &'a dyn VectorStore,
    namespace: &'a Namespace,
}

async fn process_one_file(task: &FileTask<'_>) -> FileResult {
    let index = task.index;
    let total = task.total;
    let rel_path = task.rel_path;
    let abs_path = task.abs_path;
    let stored_hashes = task.stored_hashes;
    let chunker = task.chunker;
    let summarizer = task.summarizer;
    let embedder = task.embedder;
    let store = task.store;
    let namespace = task.namespace;
    let content = match std::fs::read_to_string(abs_path) {
        Ok(c) => c,
        Err(e) => {
            let idx = format!("{:>4}/{}", index + 1, total);
            eprintln!(
                "  [{}] {rel_path} {} {}",
                idx.if_supports_color(Stream::Stderr, |s| s.cyan()),
                "—".if_supports_color(Stream::Stderr, |s| s.dimmed()),
                format!("error: {e}").if_supports_color(Stream::Stderr, |s| s.red()),
            );
            return FileResult {
                outcome: FileOutcome::Failed,
            };
        }
    };

    // Content-hash skip: if stored hash matches, skip expensive pipeline
    let content_hash = blake3::hash(content.as_bytes()).to_hex()[..16].to_string();
    if stored_hashes
        .get(rel_path)
        .is_some_and(|s| *s == content_hash)
    {
        return FileResult {
            outcome: FileOutcome::Skipped,
        };
    }

    let idx = format!("{:>4}/{}", index + 1, total);

    match pipeline::process_file(rel_path, &content, chunker, summarizer, embedder).await {
        Ok(result) => {
            let t = &result.timing;
            let syms = format!("({} symbols)", result.symbol_count);
            let timing = format!(
                "summarize: {:.1}s, embed: {:.1}s",
                t.summarize.as_secs_f64(),
                t.embed.as_secs_f64(),
            );
            eprintln!(
                "  [{}] {rel_path} {} {} {}",
                idx.if_supports_color(Stream::Stderr, |s| s.cyan()),
                syms.if_supports_color(Stream::Stderr, |s| s.green()),
                "—".if_supports_color(Stream::Stderr, |s| s.dimmed()),
                timing.if_supports_color(Stream::Stderr, |s| s.yellow()),
            );
            if let Err(e) = store.delete_by_file(namespace, rel_path).await {
                eprintln!(
                    "  [{}] {rel_path} {} {}",
                    idx.if_supports_color(Stream::Stderr, |s| s.cyan()),
                    "—".if_supports_color(Stream::Stderr, |s| s.dimmed()),
                    format!("pre-upsert delete error: {e}")
                        .if_supports_color(Stream::Stderr, |s| s.red()),
                );
                return FileResult {
                    outcome: FileOutcome::Failed,
                };
            }
            FileResult {
                outcome: FileOutcome::Processed {
                    documents: result.documents,
                },
            }
        }
        Err(e) => {
            eprintln!(
                "  [{}] {rel_path} {} {}",
                idx.if_supports_color(Stream::Stderr, |s| s.cyan()),
                "—".if_supports_color(Stream::Stderr, |s| s.dimmed()),
                format!("error: {e}").if_supports_color(Stream::Stderr, |s| s.red()),
            );
            FileResult {
                outcome: FileOutcome::Failed,
            }
        }
    }
}

fn resolve_call_edges(documents: &mut [VectorDocument]) {
    use std::collections::HashMap;

    // Phase 1: build owned symbol table and called_by map
    let mut symbol_table: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for doc in documents.iter() {
        if doc.chunk_kind == ChunkKind::Symbol
            && let Some(ref name) = doc.symbol_name
        {
            symbol_table
                .entry(name.clone())
                .or_default()
                .push((doc.file_path.clone(), doc.id.clone()));
        }
    }

    let mut called_by_map: HashMap<String, Vec<String>> = HashMap::new();
    for doc in documents.iter() {
        if doc.chunk_kind != ChunkKind::Symbol {
            continue;
        }
        if let Some(ref calls) = doc.calls {
            let caller_name = doc.symbol_name.as_deref().unwrap_or("?");
            let caller_ref = format!("{}:{}", doc.file_path, caller_name);
            for call_name in calls {
                if let Some(targets) = symbol_table.get(call_name) {
                    for (_, target_id) in targets {
                        called_by_map
                            .entry(target_id.clone())
                            .or_default()
                            .push(caller_ref.clone());
                    }
                }
            }
        }
    }

    // Phase 2: apply resolved edges
    for doc in documents.iter_mut() {
        if doc.chunk_kind != ChunkKind::Symbol {
            continue;
        }

        if let Some(ref raw_calls) = doc.calls {
            let resolved: Vec<String> = raw_calls
                .iter()
                .flat_map(|name| {
                    symbol_table
                        .get(name)
                        .into_iter()
                        .flatten()
                        .map(move |(file, _)| format!("{file}:{name}"))
                })
                .collect();
            doc.calls = Some(resolved);
        }

        doc.called_by = Some(called_by_map.remove(&doc.id).unwrap_or_default());
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
        let mut docs = vec![VectorDocument {
            id: "f1".into(),
            vector: vec![0.0; 3],
            summary: String::new(),
            file_path: "src/a.rs".into(),
            chunk_kind: ChunkKind::File,
            symbol_name: None,
            symbol_kind: None,
            start_line: None,
            end_line: None,
            language: Some("rust".into()),
            content_hash: None,
            calls: None,
            called_by: None,
        }];

        resolve_call_edges(&mut docs);

        assert!(docs[0].calls.is_none());
        assert!(docs[0].called_by.is_none());
    }
}
