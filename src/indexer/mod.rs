//! Indexer: walks the repo, processes each file through the
//! chunk → summarize → embed → upsert pipeline, and advances the HWM.

pub mod cost;
pub mod git;
pub mod pipeline;
pub mod walk;

use std::path::Path;

use anyhow::{Result, bail};

use crate::chunk::Chunker;
use crate::embed::{Embedder, embedder_identity};
use crate::store::{Namespace, NamespaceMetadata, VectorStore};
use crate::summarize::Summarizer;

pub struct IndexRun {
    chunker: Box<dyn Chunker>,
    summarizer: Box<dyn Summarizer>,
    embedder: Box<dyn Embedder>,
    store: Box<dyn VectorStore>,
    namespace: Namespace,
}

pub struct IndexReport {
    pub files_processed: usize,
    pub files_failed: usize,
    pub vectors_upserted: usize,
    pub vectors_deleted: usize,
    pub hwm_advanced_to: Option<String>,
}

impl IndexRun {
    pub fn new(
        chunker: Box<dyn Chunker>,
        summarizer: Box<dyn Summarizer>,
        embedder: Box<dyn Embedder>,
        store: Box<dyn VectorStore>,
        namespace: Namespace,
    ) -> Self {
        Self {
            chunker,
            summarizer,
            embedder,
            store,
            namespace,
        }
    }

    /// Run the indexer against the given repo root.
    ///
    /// `full = true` ignores the HWM and walks all files. `full = false`
    /// diffs from the stored HWM to HEAD and processes only changed files.
    pub async fn run(&self, full: bool, root: &Path) -> Result<IndexReport> {
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

        let mut processed = 0;
        let mut failed = 0;
        let mut upserted = 0;

        for rel_path in &to_process {
            let abs_path = root.join(rel_path);
            let content = match std::fs::read_to_string(&abs_path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("warning: skipping {rel_path}: {e}");
                    failed += 1;
                    continue;
                }
            };

            match pipeline::process_file(
                rel_path,
                &content,
                self.chunker.as_ref(),
                self.summarizer.as_ref(),
                self.embedder.as_ref(),
            )
            .await
            {
                Ok(result) => {
                    let stats = self
                        .store
                        .upsert(&self.namespace, &result.documents)
                        .await?;
                    upserted += stats.upserted;
                    processed += 1;
                }
                Err(e) => {
                    eprintln!("warning: failed to process {rel_path}: {e}");
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
            vectors_upserted: upserted,
            vectors_deleted,
            hwm_advanced_to: Some(head),
        })
    }
}

pub fn resolve_namespace(config: &crate::config::Config) -> Result<Namespace> {
    let ns = &config.indexer.namespace;
    if ns.is_empty() {
        let remote = git::remote_url(&std::env::current_dir()?)?;
        Ok(Namespace::from(git::derive_namespace(&remote)))
    } else {
        Ok(Namespace::from(ns.clone()))
    }
}
