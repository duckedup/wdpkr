//! Search orchestration: embed query → store.search → group → tiered JSON.
//!
//! The search module does NOT depend on the CLI — it receives a
//! [`SearchParams`] struct and returns a [`SearchReport`]. The CLI layer
//! constructs `SearchParams` from clap's `SearchArgs` and serializes the
//! report to stdout via [`output::render_json`] or [`output::render_pretty`].

pub mod output;

use std::cmp::Ordering;
use std::collections::HashMap;

use anyhow::{Result, bail};
use serde::Serialize;

use crate::embed::{Embedder, embedder_identity};
use crate::store::{ChunkKind, Namespace, SearchOptions, SearchResult, VectorStore};

// ── Input ─────────────────────────────────────────────────────────────────

pub struct SearchParams {
    pub query: String,
    pub top_k: usize,
    pub symbols_per_file: usize,
    pub no_symbols: bool,
    pub scope: Option<String>,
}

// ── Output (serializable to the SPEC's JSON shape) ────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct SearchReport {
    pub query: String,
    pub namespace: String,
    pub indexed_at: Option<String>,
    pub results: Vec<FileResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileResult {
    pub path: String,
    pub score: f32,
    pub summary: String,
    pub symbols: Vec<SymbolResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SymbolResult {
    pub name: String,
    pub kind: String,
    pub lines: [u32; 2],
    pub summary: String,
    pub score: f32,
}

// ── Orchestrator ──────────────────────────────────────────────────────────

pub struct SearchRun {
    embedder: Box<dyn Embedder>,
    store: Box<dyn VectorStore>,
    namespace: Namespace,
}

impl SearchRun {
    pub fn new(
        embedder: Box<dyn Embedder>,
        store: Box<dyn VectorStore>,
        namespace: Namespace,
    ) -> Self {
        Self {
            embedder,
            store,
            namespace,
        }
    }

    pub async fn run(&self, params: &SearchParams) -> Result<SearchReport> {
        // 1. Verify namespace exists
        if !self.store.namespace_exists(&self.namespace).await? {
            bail!(
                "index not found for namespace '{}'; run `wdpkr index` first",
                self.namespace.as_str()
            );
        }

        // 2. Read metadata for HWM + embedder mismatch check
        let meta = self.store.get_metadata(&self.namespace).await?;
        if let Some(ref stored_embedder) = meta.embedder {
            let current = embedder_identity(self.embedder.as_ref());
            if stored_embedder != &current {
                bail!(
                    "embedder mismatch: index was built with {stored_embedder}, \
                     but search is configured for {current}; \
                     run `wdpkr index --full` to reindex or change your embedder config"
                );
            }
        }

        // 3. Embed the query
        let query_vector = self.embedder.embed(&params.query).await?;

        // 4. Over-fetch from the store so we have both file-level and
        //    symbol-level results to group. Factor of 3 is the starting
        //    heuristic per SPEC; tunable via eval.
        let over_fetch = params.top_k * (params.symbols_per_file + 1) * 3;
        let all_results = self
            .store
            .search(
                &self.namespace,
                &query_vector,
                &SearchOptions {
                    top_k: over_fetch,
                    path_prefix: params.scope.clone(),
                    ..Default::default()
                },
            )
            .await?;

        // 5. Group into file → symbols tiered structure
        let results = group_results(&all_results, params);

        Ok(SearchReport {
            query: params.query.clone(),
            namespace: self.namespace.as_str().to_string(),
            indexed_at: meta.hwm_sha,
            results,
        })
    }
}

/// Group flat search results into a tiered file → symbols structure.
///
/// 1. Separate into file-level and symbol-level results.
/// 2. Take the top `top_k` files (already score-sorted from the store).
/// 3. For each file, attach the top `symbols_per_file` symbol results
///    that share the same `file_path`.
fn group_results(results: &[SearchResult], params: &SearchParams) -> Vec<FileResult> {
    let mut file_results: Vec<&SearchResult> = Vec::new();
    let mut symbols_by_file: HashMap<&str, Vec<&SearchResult>> = HashMap::new();

    for r in results {
        match r.chunk_kind {
            ChunkKind::File => file_results.push(r),
            ChunkKind::Symbol => {
                symbols_by_file
                    .entry(r.file_path.as_str())
                    .or_default()
                    .push(r);
            }
        }
    }

    // Files arrive sorted by score from the store; truncate to top_k.
    file_results.truncate(params.top_k);

    file_results
        .iter()
        .map(|file| {
            let symbols = if params.no_symbols {
                vec![]
            } else {
                let mut syms: Vec<&SearchResult> = symbols_by_file
                    .get(file.file_path.as_str())
                    .cloned()
                    .unwrap_or_default();
                syms.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));
                syms.truncate(params.symbols_per_file);
                syms.into_iter()
                    .map(|s| SymbolResult {
                        name: s.symbol_name.clone().unwrap_or_default(),
                        kind: s.symbol_kind.clone().unwrap_or_default(),
                        lines: [s.start_line.unwrap_or(0), s.end_line.unwrap_or(0)],
                        summary: s.summary.clone(),
                        score: s.score,
                    })
                    .collect()
            };

            FileResult {
                path: file.file_path.clone(),
                score: file.score,
                summary: file.summary.clone(),
                symbols,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{ChunkKind, Namespace, NamespaceMetadata, VectorDocument};
    use crate::testing::mock_embed::MockEmbedder;
    use crate::testing::mock_store::MockVectorStore;

    /// Set up a mock store with a realistic codebase: three files, each
    /// with two symbols. Vectors are 3D for easy reasoning about similarity.
    async fn seeded_store() -> MockVectorStore {
        let store = MockVectorStore::new();
        store
            .create_namespace(&Namespace::from("test"), 3)
            .await
            .unwrap();

        let docs = vec![
            // Commission module — close to [1,0,0]
            VectorDocument {
                id: "f-commission".into(),
                vector: vec![1.0, 0.0, 0.0],
                summary: "Commission payment release service".into(),
                file_path: "src/finance/commission.rs".into(),
                chunk_kind: ChunkKind::File,
                symbol_name: None,
                symbol_kind: None,
                start_line: None,
                end_line: None,
                language: Some("rust".into()),
            },
            VectorDocument {
                id: "s-release".into(),
                vector: vec![0.9, 0.1, 0.0],
                summary: "Releases commission for a payee".into(),
                file_path: "src/finance/commission.rs".into(),
                chunk_kind: ChunkKind::Symbol,
                symbol_name: Some("release_payment".into()),
                symbol_kind: Some("function".into()),
                start_line: Some(42),
                end_line: Some(78),
                language: Some("rust".into()),
            },
            VectorDocument {
                id: "s-correct".into(),
                vector: vec![0.8, 0.2, 0.0],
                summary: "Corrects commission amount before release".into(),
                file_path: "src/finance/commission.rs".into(),
                chunk_kind: ChunkKind::Symbol,
                symbol_name: Some("correct_amount".into()),
                symbol_kind: Some("function".into()),
                start_line: Some(80),
                end_line: Some(95),
                language: Some("rust".into()),
            },
            // Auth module — close to [0,1,0]
            VectorDocument {
                id: "f-auth".into(),
                vector: vec![0.0, 1.0, 0.0],
                summary: "Authentication and session management".into(),
                file_path: "src/auth/login.rs".into(),
                chunk_kind: ChunkKind::File,
                symbol_name: None,
                symbol_kind: None,
                start_line: None,
                end_line: None,
                language: Some("rust".into()),
            },
            VectorDocument {
                id: "s-authenticate".into(),
                vector: vec![0.1, 0.9, 0.0],
                summary: "Authenticates a user".into(),
                file_path: "src/auth/login.rs".into(),
                chunk_kind: ChunkKind::Symbol,
                symbol_name: Some("authenticate".into()),
                symbol_kind: Some("function".into()),
                start_line: Some(10),
                end_line: Some(30),
                language: Some("rust".into()),
            },
            // API module — close to [0,0,1]
            VectorDocument {
                id: "f-api".into(),
                vector: vec![0.0, 0.0, 1.0],
                summary: "HTTP request handler".into(),
                file_path: "src/api/handler.rs".into(),
                chunk_kind: ChunkKind::File,
                symbol_name: None,
                symbol_kind: None,
                start_line: None,
                end_line: None,
                language: Some("rust".into()),
            },
            VectorDocument {
                id: "s-handle".into(),
                vector: vec![0.1, 0.0, 0.9],
                summary: "Handles incoming HTTP request".into(),
                file_path: "src/api/handler.rs".into(),
                chunk_kind: ChunkKind::Symbol,
                symbol_name: Some("handle_request".into()),
                symbol_kind: Some("function".into()),
                start_line: Some(5),
                end_line: Some(20),
                language: Some("rust".into()),
            },
        ];
        store.upsert(&Namespace::from("test"), &docs).await.unwrap();
        store
    }

    fn query_embedder() -> MockEmbedder {
        let mut e = MockEmbedder::new(3);
        // "commission query" embeds close to the commission file
        e.set_override("release commission payments", vec![0.95, 0.05, 0.0]);
        // "auth query" embeds close to the auth file
        e.set_override("user authentication", vec![0.05, 0.95, 0.0]);
        e
    }

    #[tokio::test]
    async fn search_returns_ranked_results() {
        let store = seeded_store().await;
        let embedder = query_embedder();
        let search = SearchRun::new(Box::new(embedder), Box::new(store), Namespace::from("test"));

        let report = search
            .run(&SearchParams {
                query: "release commission payments".into(),
                top_k: 5,
                symbols_per_file: 3,
                no_symbols: false,
                scope: None,
            })
            .await
            .unwrap();

        assert_eq!(report.query, "release commission payments");
        assert_eq!(report.namespace, "test");
        assert!(!report.results.is_empty());
        // Commission file should rank first (closest vector)
        assert_eq!(report.results[0].path, "src/finance/commission.rs");
    }

    #[tokio::test]
    async fn symbols_nested_under_files() {
        let store = seeded_store().await;
        let embedder = query_embedder();
        let search = SearchRun::new(Box::new(embedder), Box::new(store), Namespace::from("test"));

        let report = search
            .run(&SearchParams {
                query: "release commission payments".into(),
                top_k: 5,
                symbols_per_file: 3,
                no_symbols: false,
                scope: None,
            })
            .await
            .unwrap();

        let commission = &report.results[0];
        assert!(!commission.symbols.is_empty());
        // release_payment should rank above correct_amount (closer vector)
        assert_eq!(commission.symbols[0].name, "release_payment");
    }

    #[tokio::test]
    async fn no_symbols_flag_omits_symbols() {
        let store = seeded_store().await;
        let embedder = query_embedder();
        let search = SearchRun::new(Box::new(embedder), Box::new(store), Namespace::from("test"));

        let report = search
            .run(&SearchParams {
                query: "release commission payments".into(),
                top_k: 5,
                symbols_per_file: 3,
                no_symbols: true,
                scope: None,
            })
            .await
            .unwrap();

        for file in &report.results {
            assert!(file.symbols.is_empty());
        }
    }

    #[tokio::test]
    async fn top_k_limits_file_count() {
        let store = seeded_store().await;
        let embedder = query_embedder();
        let search = SearchRun::new(Box::new(embedder), Box::new(store), Namespace::from("test"));

        let report = search
            .run(&SearchParams {
                query: "release commission payments".into(),
                top_k: 1,
                symbols_per_file: 3,
                no_symbols: false,
                scope: None,
            })
            .await
            .unwrap();

        assert_eq!(report.results.len(), 1);
        assert_eq!(report.results[0].path, "src/finance/commission.rs");
    }

    #[tokio::test]
    async fn symbols_per_file_limits_symbol_count() {
        let store = seeded_store().await;
        let embedder = query_embedder();
        let search = SearchRun::new(Box::new(embedder), Box::new(store), Namespace::from("test"));

        let report = search
            .run(&SearchParams {
                query: "release commission payments".into(),
                top_k: 5,
                symbols_per_file: 1,
                no_symbols: false,
                scope: None,
            })
            .await
            .unwrap();

        let commission = &report.results[0];
        assert_eq!(commission.symbols.len(), 1);
        assert_eq!(commission.symbols[0].name, "release_payment");
    }

    #[tokio::test]
    async fn scope_filters_by_path_prefix() {
        let store = seeded_store().await;
        let embedder = query_embedder();
        let search = SearchRun::new(Box::new(embedder), Box::new(store), Namespace::from("test"));

        let report = search
            .run(&SearchParams {
                query: "release commission payments".into(),
                top_k: 5,
                symbols_per_file: 3,
                no_symbols: false,
                scope: Some("src/finance/".into()),
            })
            .await
            .unwrap();

        assert_eq!(report.results.len(), 1);
        assert!(report.results[0].path.starts_with("src/finance/"));
    }

    #[tokio::test]
    async fn missing_namespace_errors() {
        let store = MockVectorStore::new();
        let embedder = MockEmbedder::new(3);
        let search = SearchRun::new(
            Box::new(embedder),
            Box::new(store),
            Namespace::from("nonexistent"),
        );

        let err = search
            .run(&SearchParams {
                query: "anything".into(),
                top_k: 5,
                symbols_per_file: 3,
                no_symbols: false,
                scope: None,
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("index not found"));
    }

    #[tokio::test]
    async fn embedder_mismatch_errors() {
        let store = seeded_store().await;
        // Set metadata with a different embedder identity
        store
            .set_metadata(
                &Namespace::from("test"),
                &NamespaceMetadata {
                    embedder: Some("openai/text-embedding-3-large".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let embedder = MockEmbedder::new(3);
        let search = SearchRun::new(Box::new(embedder), Box::new(store), Namespace::from("test"));

        let err = search
            .run(&SearchParams {
                query: "anything".into(),
                top_k: 5,
                symbols_per_file: 3,
                no_symbols: false,
                scope: None,
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("embedder mismatch"));
    }

    #[tokio::test]
    async fn indexed_at_reflects_hwm() {
        let store = seeded_store().await;
        store
            .set_metadata(
                &Namespace::from("test"),
                &NamespaceMetadata {
                    hwm_sha: Some("abc123def456".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let embedder = query_embedder();
        let search = SearchRun::new(Box::new(embedder), Box::new(store), Namespace::from("test"));

        let report = search
            .run(&SearchParams {
                query: "release commission payments".into(),
                top_k: 5,
                symbols_per_file: 3,
                no_symbols: false,
                scope: None,
            })
            .await
            .unwrap();

        assert_eq!(report.indexed_at.as_deref(), Some("abc123def456"));
    }

    #[tokio::test]
    async fn report_serializes_to_spec_json_shape() {
        let store = seeded_store().await;
        let embedder = query_embedder();
        let search = SearchRun::new(Box::new(embedder), Box::new(store), Namespace::from("test"));

        let report = search
            .run(&SearchParams {
                query: "release commission payments".into(),
                top_k: 2,
                symbols_per_file: 1,
                no_symbols: false,
                scope: None,
            })
            .await
            .unwrap();

        let json = serde_json::to_value(&report).unwrap();
        // Top-level fields per SPEC
        assert!(json.get("query").is_some());
        assert!(json.get("namespace").is_some());
        assert!(json.get("indexed_at").is_some());
        assert!(json.get("results").is_some());
        // Nested structure
        let first = &json["results"][0];
        assert!(first.get("path").is_some());
        assert!(first.get("score").is_some());
        assert!(first.get("summary").is_some());
        assert!(first.get("symbols").is_some());
    }
}
