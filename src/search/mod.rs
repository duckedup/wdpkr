//! Search orchestration: embed query → store.search → group → tiered JSON.
//!
//! The search module does NOT depend on the CLI — it receives a
//! [`SearchParams`] struct and returns a [`SearchReport`]. The CLI layer
//! constructs `SearchParams` from clap's `SearchArgs` and serializes the
//! report to stdout via [`output::render_json`] or [`output::render_pretty`].

pub mod output;

use std::cmp::Ordering;
use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::config::DecayConfig;
use crate::embed::{Embedder, embedder_identity};
use crate::store::{ChunkKind, Namespace, SearchOptions, SearchResult, VectorStore};

// ── Input ─────────────────────────────────────────────────────────────────

pub struct SearchParams {
    pub query: String,
    pub top_k: usize,
    pub symbols_per_file: usize,
    pub no_symbols: bool,
    pub scope: Vec<String>,
    pub filters: Vec<String>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub symbols: Vec<SymbolResult>,
    /// Architectural decisions that govern this code file (L2 recall). Present
    /// only on code results whose path matches an active decision's `areas`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub governed_by: Option<Vec<GoverningDecision>>,
}

/// A decision referenced from a code result because its `areas` glob matches the
/// file. Compiled by the CLI from the decision registry and carried on
/// [`SearchRun`]; `search` attaches these to matching code files.
#[derive(Debug, Clone)]
pub struct DecisionMeta {
    pub uri: String,
    pub title: String,
    pub status: String,
    /// Whether this decision participates in active recall (not superseded).
    pub active: bool,
    /// Compiled globs of the code areas this decision governs.
    pub areas: globset::GlobSet,
    /// URIs of decisions this one overrides (scope-override precedence).
    pub overrides: Vec<String>,
}

/// A governing decision as surfaced in search output.
#[derive(Debug, Clone, Serialize)]
pub struct GoverningDecision {
    pub path: String,
    pub title: String,
    pub status: String,
}

/// Compile decision registry entries into search-layer [`DecisionMeta`] (area
/// globs + override URIs), keeping glob/registry parsing in one place. Used by
/// the CLI to feed [`SearchRun::with_decisions`].
pub fn compile_decisions(reg: &crate::decision::DecisionRegistry) -> Result<Vec<DecisionMeta>> {
    reg.decisions
        .iter()
        .map(|d| {
            let mut builder = globset::GlobSetBuilder::new();
            for area in &d.areas {
                builder.add(globset::Glob::new(area).with_context(|| {
                    format!("decision {} has invalid area glob '{area}'", d.id)
                })?);
            }
            let areas = builder.build().context("compiling decision area globs")?;
            Ok(DecisionMeta {
                uri: d.uri(),
                title: d.title.clone(),
                status: d.status.as_str().to_string(),
                active: d.status.is_active(),
                areas,
                overrides: d
                    .overrides
                    .iter()
                    .map(|id| crate::decision::decision_uri(*id))
                    .collect(),
            })
        })
        .collect()
}

#[derive(Debug, Clone, Serialize)]
pub struct SymbolResult {
    pub name: String,
    pub kind: String,
    pub lines: [u32; 2],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub score: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub calls: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub called_by: Option<Vec<String>>,
}

// ── Orchestrator ──────────────────────────────────────────────────────────

pub struct SearchRun {
    embedder: Box<dyn Embedder>,
    store: Box<dyn VectorStore>,
    /// (namespace, source_label, decay_config) per selected tap.
    namespaces: Vec<(Namespace, Option<String>, DecayConfig)>,
    /// Current unix seconds, used to age documents for decay. `0` disables the
    /// age effect (decay configs are also disabled by default).
    now: i64,
    /// Compiled decision registry (L2). Empty disables decision behavior:
    /// governed_by attach and superseded-result filtering.
    decisions: Vec<DecisionMeta>,
}

impl SearchRun {
    /// Single-namespace constructor (backward compat). Decay disabled.
    pub fn new(
        embedder: Box<dyn Embedder>,
        store: Box<dyn VectorStore>,
        namespace: Namespace,
    ) -> Self {
        Self {
            embedder,
            store,
            namespaces: vec![(namespace, None, DecayConfig::default())],
            now: 0,
            decisions: Vec::new(),
        }
    }

    /// Multi-namespace constructor. Each entry is (namespace, source_label).
    /// The source label is `None` for the files tap (omitted from JSON)
    /// and `Some("linear")` etc. for external taps. Decay disabled.
    pub fn new_multi(
        embedder: Box<dyn Embedder>,
        store: Box<dyn VectorStore>,
        namespaces: Vec<(Namespace, Option<String>)>,
    ) -> Self {
        Self {
            embedder,
            store,
            namespaces: namespaces
                .into_iter()
                .map(|(ns, src)| (ns, src, DecayConfig::default()))
                .collect(),
            now: 0,
            decisions: Vec::new(),
        }
    }

    /// Multi-namespace constructor with per-tap decay. `now` is the current
    /// unix time used to age documents.
    pub fn new_multi_with_decay(
        embedder: Box<dyn Embedder>,
        store: Box<dyn VectorStore>,
        namespaces: Vec<(Namespace, Option<String>, DecayConfig)>,
        now: i64,
    ) -> Self {
        Self {
            embedder,
            store,
            namespaces,
            now,
            decisions: Vec::new(),
        }
    }

    /// Attach a compiled decision registry (L2 recall). When non-empty, search
    /// filters out superseded-decision results and attaches `governed_by` to
    /// code files whose path matches an active decision's areas.
    pub fn with_decisions(mut self, decisions: Vec<DecisionMeta>) -> Self {
        self.decisions = decisions;
        self
    }

    pub async fn run(&self, params: &SearchParams) -> Result<SearchReport> {
        // 1. Compile glob filters (fail fast before any API calls)
        let glob_set = if params.filters.is_empty() {
            None
        } else {
            let mut builder = globset::GlobSetBuilder::new();
            for pattern in &params.filters {
                builder.add(
                    globset::Glob::new(pattern)
                        .with_context(|| format!("invalid --filter glob: {pattern}"))?,
                );
            }
            Some(
                builder
                    .build()
                    .context("failed to compile --filter globs")?,
            )
        };

        // 2. Embed the query once
        let query_vector = self.embedder.embed_query(&params.query).await?;

        // 3. Search each namespace, collecting tagged results
        let over_fetch = params.top_k * (params.symbols_per_file + 1) * 3;
        let mut all_results: Vec<(SearchResult, Option<String>)> = Vec::new();
        let mut primary_namespace = String::new();
        let mut primary_indexed_at: Option<String> = None;
        let mut any_namespace_found = false;

        for (ns, source, decay) in &self.namespaces {
            if !self.store.namespace_exists(ns).await? {
                continue;
            }
            any_namespace_found = true;

            let meta = self.store.get_metadata(ns).await?;
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

            if primary_namespace.is_empty() {
                primary_namespace = ns.as_str().to_string();
                primary_indexed_at = meta.hwm_sha;
            }

            let ns_results = self
                .store
                .search(
                    ns,
                    &query_vector,
                    &SearchOptions {
                        top_k: over_fetch,
                        path_prefixes: params.scope.clone(),
                        ..Default::default()
                    },
                )
                .await?;

            for mut r in ns_results {
                r.score = apply_decay(r.score, r.last_used_at, self.now, decay);
                all_results.push((r, source.clone()));
            }
        }

        if !any_namespace_found {
            let ns_name = self
                .namespaces
                .first()
                .map(|(ns, _, _)| ns.as_str().to_string())
                .unwrap_or_default();
            bail!("index not found for namespace '{ns_name}'; run `wdpkr index` first");
        }

        // 4. Group into file → symbols tiered structure
        let results = group_results_multi(&all_results, params, glob_set.as_ref(), &self.decisions);

        Ok(SearchReport {
            query: params.query.clone(),
            namespace: primary_namespace,
            indexed_at: primary_indexed_at,
            results,
        })
    }
}

/// Apply a namespace's decay to a raw cosine score. A document with a known
/// `last_used_at` is aged by `now - last_used_at` and multiplied by the tap's
/// decay curve (floored). Disabled decay, or an unknown timestamp, leaves the
/// score unchanged.
fn apply_decay(score: f32, last_used_at: Option<i64>, now: i64, decay: &DecayConfig) -> f32 {
    match last_used_at {
        Some(ts) if decay.enabled => score * decay.multiplier(now - ts),
        _ => score,
    }
}

/// Group flat search results into a tiered file → symbols structure.
///
/// Results carry an optional source label (tap name). For the files
/// tap this is `None` (omitted from JSON); for external taps it's
/// `Some("linear")` etc.
fn group_results_multi(
    results: &[(SearchResult, Option<String>)],
    params: &SearchParams,
    glob_set: Option<&globset::GlobSet>,
    decisions: &[DecisionMeta],
) -> Vec<FileResult> {
    let mut file_results: Vec<(&SearchResult, Option<&String>)> = Vec::new();
    let mut symbols_by_file: HashMap<&str, Vec<&SearchResult>> = HashMap::new();

    for (r, source) in results {
        match r.chunk_kind {
            ChunkKind::File => file_results.push((r, source.as_ref())),
            ChunkKind::Symbol => {
                symbols_by_file
                    .entry(r.file_path.as_str())
                    .or_default()
                    .push(r);
            }
        }
    }

    if let Some(gs) = glob_set {
        file_results.retain(|(r, _)| gs.is_match(&r.file_path));
    }

    // Drop superseded/deprecated decision results from active recall (they stay
    // in the store, walkable via links). Filter before truncation so they don't
    // occupy top_k slots.
    if decisions.iter().any(|d| !d.active) {
        let inactive: std::collections::HashSet<&str> = decisions
            .iter()
            .filter(|d| !d.active)
            .map(|d| d.uri.as_str())
            .collect();
        file_results.retain(|(r, _)| !inactive.contains(r.file_path.as_str()));
    }

    // Re-sort by score descending across all namespaces.
    file_results.sort_by(|(a, _), (b, _)| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));
    file_results.truncate(params.top_k);

    file_results
        .iter()
        .map(|(file, source)| {
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
                        summary: Some(s.summary.clone()),
                        score: s.score,
                        calls: s.calls.clone(),
                        called_by: s.called_by.clone(),
                    })
                    .collect()
            };

            // L2: attach governing decisions to code results (source == None).
            let governed_by = if source.is_none() {
                let g = governing_for(&file.file_path, decisions);
                (!g.is_empty()).then_some(g)
            } else {
                None
            };

            FileResult {
                path: file.file_path.clone(),
                score: file.score,
                summary: Some(file.summary.clone()),
                source: source.cloned(),
                symbols,
                governed_by,
            }
        })
        .collect()
}

/// Active decisions whose `areas` match `path`, with scope-override applied: a
/// decision overridden by another matching decision is dropped.
fn governing_for(path: &str, decisions: &[DecisionMeta]) -> Vec<GoverningDecision> {
    let matched: Vec<&DecisionMeta> = decisions
        .iter()
        .filter(|d| d.active && d.areas.is_match(path))
        .collect();
    let overridden: std::collections::HashSet<&str> = matched
        .iter()
        .flat_map(|d| d.overrides.iter().map(String::as_str))
        .collect();
    matched
        .iter()
        .filter(|d| !overridden.contains(d.uri.as_str()))
        .map(|d| GoverningDecision {
            path: d.uri.clone(),
            title: d.title.clone(),
            status: d.status.clone(),
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
                content_hash: None,
                calls: None,
                called_by: None,
                last_used_at: None,
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
                content_hash: None,
                calls: None,
                called_by: None,
                last_used_at: None,
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
                content_hash: None,
                calls: None,
                called_by: None,
                last_used_at: None,
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
                content_hash: None,
                calls: None,
                called_by: None,
                last_used_at: None,
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
                content_hash: None,
                calls: None,
                called_by: None,
                last_used_at: None,
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
                content_hash: None,
                calls: None,
                called_by: None,
                last_used_at: None,
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
                content_hash: None,
                calls: None,
                called_by: None,
                last_used_at: None,
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

    // ── decay ─────────────────────────────────────────────────────────

    #[test]
    fn apply_decay_disabled_is_identity() {
        let d = DecayConfig::default(); // disabled
        assert_eq!(apply_decay(0.8, Some(0), 10_000_000, &d), 0.8);
    }

    #[test]
    fn apply_decay_unknown_timestamp_is_identity() {
        let d = DecayConfig {
            enabled: true,
            half_life_days: 90.0,
            floor: 0.4,
        };
        assert_eq!(apply_decay(0.8, None, 10_000_000, &d), 0.8);
    }

    #[test]
    fn apply_decay_ages_score_with_floor() {
        let d = DecayConfig {
            enabled: true,
            half_life_days: 90.0,
            floor: 0.4,
        };
        let now = 100 * 86_400;
        // Fresh (age 0) → unchanged.
        assert!((apply_decay(0.8, Some(now), now, &d) - 0.8).abs() < 1e-4);
        // One half-life old → ~half.
        let one_hl = now - 90 * 86_400;
        assert!((apply_decay(0.8, Some(one_hl), now, &d) - 0.4).abs() < 1e-3);
        // Ancient → floored (0.8 * 0.4).
        let ancient = now - 10_000 * 86_400;
        assert!((apply_decay(0.8, Some(ancient), now, &d) - 0.32).abs() < 1e-4);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn search_decay_reranks_stale_below_fresh() {
        // Two docs with identical vectors; decay must sink the stale one.
        let store = MockVectorStore::new();
        let ns = Namespace::from("test--notion");
        store.create_namespace(&ns, 3).await.unwrap();
        let now = 1_000 * 86_400;
        let mk = |id: &str, path: &str, ts: i64| VectorDocument {
            id: id.into(),
            vector: vec![1.0, 0.0, 0.0],
            summary: format!("doc {id}"),
            file_path: path.into(),
            chunk_kind: ChunkKind::File,
            symbol_name: None,
            symbol_kind: None,
            start_line: None,
            end_line: None,
            language: None,
            content_hash: None,
            calls: None,
            called_by: None,
            last_used_at: Some(ts),
        };
        store
            .upsert(
                &ns,
                &[
                    mk("fresh", "notion://fresh", now),
                    mk("stale", "notion://stale", now - 3_650 * 86_400),
                ],
            )
            .await
            .unwrap();

        let mut embedder = MockEmbedder::new(3);
        embedder.set_override("q", vec![1.0, 0.0, 0.0]);

        let decay = DecayConfig {
            enabled: true,
            half_life_days: 90.0,
            floor: 0.4,
        };
        let search = SearchRun::new_multi_with_decay(
            Box::new(embedder),
            Box::new(store),
            vec![(ns, Some("notion".into()), decay)],
            now,
        );
        let report = search
            .run(&SearchParams {
                query: "q".into(),
                top_k: 5,
                symbols_per_file: 3,
                no_symbols: true,
                scope: vec![],
                filters: vec![],
            })
            .await
            .unwrap();

        assert_eq!(report.results.len(), 2);
        assert_eq!(report.results[0].path, "notion://fresh");
        assert_eq!(report.results[1].path, "notion://stale");
        assert!(
            report.results[0].score > report.results[1].score,
            "fresh ({}) should outrank stale ({})",
            report.results[0].score,
            report.results[1].score
        );
    }

    #[cfg_attr(miri, ignore)]
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
                scope: vec![],
                filters: vec![],
            })
            .await
            .unwrap();

        assert_eq!(report.query, "release commission payments");
        assert_eq!(report.namespace, "test");
        assert!(!report.results.is_empty());
        // Commission file should rank first (closest vector)
        assert_eq!(report.results[0].path, "src/finance/commission.rs");
    }

    #[cfg_attr(miri, ignore)]
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
                scope: vec![],
                filters: vec![],
            })
            .await
            .unwrap();

        let commission = &report.results[0];
        assert!(!commission.symbols.is_empty());
        // release_payment should rank above correct_amount (closer vector)
        assert_eq!(commission.symbols[0].name, "release_payment");
    }

    #[cfg_attr(miri, ignore)]
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
                scope: vec![],
                filters: vec![],
            })
            .await
            .unwrap();

        for file in &report.results {
            assert!(file.symbols.is_empty());
        }
    }

    #[cfg_attr(miri, ignore)]
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
                scope: vec![],
                filters: vec![],
            })
            .await
            .unwrap();

        assert_eq!(report.results.len(), 1);
        assert_eq!(report.results[0].path, "src/finance/commission.rs");
    }

    #[cfg_attr(miri, ignore)]
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
                scope: vec![],
                filters: vec![],
            })
            .await
            .unwrap();

        let commission = &report.results[0];
        assert_eq!(commission.symbols.len(), 1);
        assert_eq!(commission.symbols[0].name, "release_payment");
    }

    #[cfg_attr(miri, ignore)]
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
                scope: vec!["src/finance/".into()],
                filters: vec![],
            })
            .await
            .unwrap();

        assert_eq!(report.results.len(), 1);
        assert!(report.results[0].path.starts_with("src/finance/"));
    }

    #[cfg_attr(miri, ignore)]
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
                scope: vec![],
                filters: vec![],
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("index not found"));
    }

    #[cfg_attr(miri, ignore)]
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
                scope: vec![],
                filters: vec![],
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("embedder mismatch"));
    }

    #[cfg_attr(miri, ignore)]
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
                scope: vec![],
                filters: vec![],
            })
            .await
            .unwrap();

        assert_eq!(report.indexed_at.as_deref(), Some("abc123def456"));
    }

    #[cfg_attr(miri, ignore)]
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
                scope: vec![],
                filters: vec![],
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

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn filter_glob_prunes_results() {
        let store = seeded_store().await;
        let embedder = query_embedder();
        let search = SearchRun::new(Box::new(embedder), Box::new(store), Namespace::from("test"));

        let report = search
            .run(&SearchParams {
                query: "release commission payments".into(),
                top_k: 5,
                symbols_per_file: 3,
                no_symbols: false,
                scope: vec![],
                filters: vec!["**/commission.*".into()],
            })
            .await
            .unwrap();

        assert_eq!(report.results.len(), 1);
        assert_eq!(report.results[0].path, "src/finance/commission.rs");
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn filter_glob_multiple_patterns_or() {
        let store = seeded_store().await;
        let embedder = query_embedder();
        let search = SearchRun::new(Box::new(embedder), Box::new(store), Namespace::from("test"));

        let report = search
            .run(&SearchParams {
                query: "release commission payments".into(),
                top_k: 5,
                symbols_per_file: 3,
                no_symbols: false,
                scope: vec![],
                filters: vec!["**/commission.*".into(), "**/login.*".into()],
            })
            .await
            .unwrap();

        assert_eq!(report.results.len(), 2);
        let paths: Vec<&str> = report.results.iter().map(|r| r.path.as_str()).collect();
        assert!(paths.contains(&"src/finance/commission.rs"));
        assert!(paths.contains(&"src/auth/login.rs"));
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn filter_glob_no_match_returns_empty() {
        let store = seeded_store().await;
        let embedder = query_embedder();
        let search = SearchRun::new(Box::new(embedder), Box::new(store), Namespace::from("test"));

        let report = search
            .run(&SearchParams {
                query: "release commission payments".into(),
                top_k: 5,
                symbols_per_file: 3,
                no_symbols: false,
                scope: vec![],
                filters: vec!["*.go".into()],
            })
            .await
            .unwrap();

        assert!(report.results.is_empty());
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn filter_invalid_glob_returns_error() {
        let store = seeded_store().await;
        let embedder = query_embedder();
        let search = SearchRun::new(Box::new(embedder), Box::new(store), Namespace::from("test"));

        let err = search
            .run(&SearchParams {
                query: "anything".into(),
                top_k: 5,
                symbols_per_file: 3,
                no_symbols: false,
                scope: vec![],
                filters: vec!["[invalid".into()],
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("--filter"));
    }

    // ── Multi-namespace search tests ────────────────────────────────

    async fn seeded_multi_store() -> MockVectorStore {
        let store = seeded_store().await;
        store
            .create_namespace(&Namespace::from("test--linear"), 3)
            .await
            .unwrap();
        let linear_docs = vec![VectorDocument {
            id: "f-linear-eng1".into(),
            vector: vec![0.95, 0.05, 0.0],
            summary: "Fix commission calculation bug".into(),
            file_path: "linear://ENG-1".into(),
            chunk_kind: ChunkKind::File,
            symbol_name: None,
            symbol_kind: None,
            start_line: None,
            end_line: None,
            language: None,
            content_hash: None,
            calls: None,
            called_by: None,
            last_used_at: None,
        }];
        store
            .upsert(&Namespace::from("test--linear"), &linear_docs)
            .await
            .unwrap();
        store
    }

    // ── decision recall (L1/L2) ─────────────────────────────────────

    fn globset(patterns: &[&str]) -> globset::GlobSet {
        let mut b = globset::GlobSetBuilder::new();
        for p in patterns {
            b.add(globset::Glob::new(p).unwrap());
        }
        b.build().unwrap()
    }

    fn decision_meta(uri: &str, areas: &[&str], active: bool, overrides: &[&str]) -> DecisionMeta {
        DecisionMeta {
            uri: uri.into(),
            title: format!("decision {uri}"),
            status: if active { "accepted" } else { "superseded" }.into(),
            active,
            areas: globset(areas),
            overrides: overrides.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn governed_by_attaches_to_in_area_code() {
        let store = seeded_store().await;
        let embedder = query_embedder();
        let d = decision_meta("decision://0001", &["src/finance/**"], true, &[]);
        let search = SearchRun::new(Box::new(embedder), Box::new(store), Namespace::from("test"))
            .with_decisions(vec![d]);

        let report = search
            .run(&SearchParams {
                query: "release commission payments".into(),
                top_k: 5,
                symbols_per_file: 3,
                no_symbols: true,
                scope: vec![],
                filters: vec![],
            })
            .await
            .unwrap();

        let commission = report
            .results
            .iter()
            .find(|r| r.path == "src/finance/commission.rs")
            .expect("commission file present");
        let gov = commission
            .governed_by
            .as_ref()
            .expect("governed_by attached");
        assert_eq!(gov.len(), 1);
        assert_eq!(gov[0].path, "decision://0001");

        // A file outside the area gets no governed_by.
        if let Some(auth) = report
            .results
            .iter()
            .find(|r| r.path == "src/auth/login.rs")
        {
            assert!(auth.governed_by.is_none());
        }
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn scope_override_drops_overridden_decision() {
        let store = seeded_store().await;
        let embedder = query_embedder();
        // Broad decision 0001 governs src/**; narrow 0002 governs src/finance/**
        // and overrides 0001. Only 0002 should attach to the commission file.
        let broad = decision_meta("decision://0001", &["src/**"], true, &[]);
        let narrow = decision_meta(
            "decision://0002",
            &["src/finance/**"],
            true,
            &["decision://0001"],
        );
        let search = SearchRun::new(Box::new(embedder), Box::new(store), Namespace::from("test"))
            .with_decisions(vec![broad, narrow]);

        let report = search
            .run(&SearchParams {
                query: "release commission payments".into(),
                top_k: 5,
                symbols_per_file: 0,
                no_symbols: true,
                scope: vec![],
                filters: vec![],
            })
            .await
            .unwrap();

        let commission = report
            .results
            .iter()
            .find(|r| r.path == "src/finance/commission.rs")
            .unwrap();
        let gov = commission.governed_by.as_ref().unwrap();
        assert_eq!(gov.len(), 1, "override should drop the broad decision");
        assert_eq!(gov[0].path, "decision://0002");
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn superseded_decision_result_excluded() {
        // A decision doc in the namespace whose registry status is superseded
        // must not appear in active results.
        let store = MockVectorStore::new();
        let ns = Namespace::from("test");
        store.create_namespace(&ns, 3).await.unwrap();
        let mk = |id: &str, path: &str| VectorDocument {
            id: id.into(),
            vector: vec![1.0, 0.0, 0.0],
            summary: format!("doc {id}"),
            file_path: path.into(),
            chunk_kind: ChunkKind::File,
            symbol_name: None,
            symbol_kind: None,
            start_line: None,
            end_line: None,
            language: None,
            content_hash: None,
            calls: None,
            called_by: None,
            last_used_at: None,
        };
        store
            .upsert(
                &ns,
                &[mk("d1", "decision://0001"), mk("d2", "decision://0002")],
            )
            .await
            .unwrap();

        let mut embedder = MockEmbedder::new(3);
        embedder.set_override("q", vec![1.0, 0.0, 0.0]);

        let active = decision_meta("decision://0001", &[], true, &[]);
        let superseded = decision_meta("decision://0002", &[], false, &[]);
        let search = SearchRun::new(Box::new(embedder), Box::new(store), ns)
            .with_decisions(vec![active, superseded]);

        let report = search
            .run(&SearchParams {
                query: "q".into(),
                top_k: 5,
                symbols_per_file: 0,
                no_symbols: true,
                scope: vec![],
                filters: vec![],
            })
            .await
            .unwrap();

        let paths: Vec<&str> = report.results.iter().map(|r| r.path.as_str()).collect();
        assert!(
            paths.contains(&"decision://0001"),
            "active decision present"
        );
        assert!(
            !paths.contains(&"decision://0002"),
            "superseded decision must be excluded"
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn multi_namespace_merges_by_score() {
        let store = seeded_multi_store().await;
        let embedder = query_embedder();
        let search = SearchRun::new_multi(
            Box::new(embedder),
            Box::new(store),
            vec![
                (Namespace::from("test"), None),
                (Namespace::from("test--linear"), Some("linear".into())),
            ],
        );

        let report = search
            .run(&SearchParams {
                query: "release commission payments".into(),
                top_k: 5,
                symbols_per_file: 3,
                no_symbols: false,
                scope: vec![],
                filters: vec![],
            })
            .await
            .unwrap();

        assert!(report.results.len() >= 2);
        let linear_result = report.results.iter().find(|r| r.path == "linear://ENG-1");
        assert!(linear_result.is_some(), "should include linear results");
        assert_eq!(
            linear_result.unwrap().source.as_deref(),
            Some("linear"),
            "linear results should have source label"
        );

        let files_result = report
            .results
            .iter()
            .find(|r| r.path == "src/finance/commission.rs");
        assert!(files_result.is_some());
        assert!(
            files_result.unwrap().source.is_none(),
            "files results should have no source label"
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn multi_namespace_missing_namespace_skipped() {
        let store = seeded_store().await;
        let embedder = query_embedder();
        let search = SearchRun::new_multi(
            Box::new(embedder),
            Box::new(store),
            vec![
                (Namespace::from("test"), None),
                (Namespace::from("test--never-indexed"), Some("never".into())),
            ],
        );

        let report = search
            .run(&SearchParams {
                query: "release commission payments".into(),
                top_k: 5,
                symbols_per_file: 3,
                no_symbols: false,
                scope: vec![],
                filters: vec![],
            })
            .await
            .unwrap();

        assert!(!report.results.is_empty());
        assert!(
            report
                .results
                .iter()
                .all(|r| r.source.as_deref() != Some("never")),
            "never-indexed namespace should be skipped"
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn source_field_omitted_in_json_when_none() {
        let store = seeded_store().await;
        let embedder = query_embedder();
        let search = SearchRun::new(Box::new(embedder), Box::new(store), Namespace::from("test"));

        let report = search
            .run(&SearchParams {
                query: "release commission payments".into(),
                top_k: 1,
                symbols_per_file: 0,
                no_symbols: true,
                scope: vec![],
                filters: vec![],
            })
            .await
            .unwrap();

        let json = serde_json::to_value(&report).unwrap();
        assert!(
            json["results"][0].get("source").is_none(),
            "source field should be omitted when None"
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn source_field_present_in_json_when_set() {
        let store = seeded_multi_store().await;
        let embedder = query_embedder();
        let search = SearchRun::new_multi(
            Box::new(embedder),
            Box::new(store),
            vec![
                (Namespace::from("test"), None),
                (Namespace::from("test--linear"), Some("linear".into())),
            ],
        );

        let report = search
            .run(&SearchParams {
                query: "release commission payments".into(),
                top_k: 5,
                symbols_per_file: 0,
                no_symbols: true,
                scope: vec![],
                filters: vec![],
            })
            .await
            .unwrap();

        let json = serde_json::to_value(&report).unwrap();
        let linear = json["results"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["path"] == "linear://ENG-1");
        assert!(linear.is_some());
        assert_eq!(linear.unwrap()["source"], "linear");
    }
}
