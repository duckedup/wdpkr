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
use tokio::sync::Semaphore;

use crate::chunk::Chunker;
use crate::embed::{Embedder, embedder_identity};
use crate::store::{Namespace, NamespaceMetadata, VectorStore};
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
            "  {total} files to process (concurrency: {})",
            self.concurrency
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
        let mut upserted = 0usize;

        while let Some(join_result) = join_set.join_next().await {
            let result = join_result?;
            match result.outcome {
                FileOutcome::Processed { vectors } => {
                    processed += 1;
                    upserted += vectors;
                }
                FileOutcome::Skipped => {
                    skipped += 1;
                }
                FileOutcome::Failed => {
                    failed += 1;
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
    Processed { vectors: usize },
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
            eprintln!("  [{:>4}/{}] {rel_path} — error: {e}", index + 1, total);
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

    match pipeline::process_file(rel_path, &content, chunker, summarizer, embedder).await {
        Ok(result) => {
            let t = &result.timing;
            eprintln!(
                "  [{:>4}/{}] {rel_path} ({} symbols) — summarize: {:.1}s, embed: {:.1}s",
                index + 1,
                total,
                result.symbol_count,
                t.summarize.as_secs_f64(),
                t.embed.as_secs_f64(),
            );
            // Delete existing vectors for this file before upserting so that
            // symbols removed since the last index don't linger as stale results.
            if let Err(e) = store.delete_by_file(namespace, rel_path).await {
                eprintln!(
                    "  [{:>4}/{}] {rel_path} — pre-upsert delete error: {e}",
                    index + 1,
                    total
                );
                return FileResult {
                    outcome: FileOutcome::Failed,
                };
            }
            match store.upsert(namespace, &result.documents).await {
                Ok(stats) => FileResult {
                    outcome: FileOutcome::Processed {
                        vectors: stats.upserted,
                    },
                },
                Err(e) => {
                    eprintln!(
                        "  [{:>4}/{}] {rel_path} — upsert error: {e}",
                        index + 1,
                        total
                    );
                    FileResult {
                        outcome: FileOutcome::Failed,
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("  [{:>4}/{}] {rel_path} — error: {e}", index + 1, total);
            FileResult {
                outcome: FileOutcome::Failed,
            }
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
