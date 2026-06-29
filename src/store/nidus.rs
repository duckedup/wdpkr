//! Local nidus vector store adapter.
//!
//! A file-backed [`VectorStore`] so wdpkr's storage leg can run with no hosted
//! third party — and, unlike the former DuckDB backend, with **no FFI**: nidus
//! ([`nidus`](https://crates.io/crates/nidus)) is a small, pure-Rust embeddable
//! vector store, so wdpkr builds and links without a C/C++ toolchain. This is
//! what lets the internal product vendor wdpkr cleanly.
//!
//! One nidus **directory** holds every namespace; the wdpkr namespace maps to a
//! nidus **collection** (isolated id space within the shared embedding space).
//!
//! Search is **exact brute-force cosine** (nidus's only mode). nidus already
//! returns cosine similarity as `Hit::score`, matching the Turbopuffer adapter's
//! `score = 1 - distance`, so `min_score` and the output layer are unchanged
//! across backends.
//!
//! ## Attribute mapping
//!
//! A [`VectorDocument`]'s fields become a nidus record's typed `attrs`:
//! strings as [`Value::Str`], line numbers as [`Value::Int`], `calls`/`called_by`
//! as [`Value::List`]. Optional fields are **omitted** from `attrs` when `None`,
//! which preserves the not-indexed (`None`) vs empty (`Some(vec![])`) distinction
//! for the call-graph lists: an absent key reads back as `None`, a `List([])` as
//! `Some(vec![])`.
//!
//! ## Dimension constraint
//!
//! A nidus store is tied to one embedding dimension (fixed at open via
//! [`nidus::Config`]). Reopening a directory with a different dimension is a hard
//! error — one install uses one embedder. Use a separate `store.nidus.path` or
//! reindex to change embedders.
//!
//! ## Concurrency
//!
//! nidus is synchronous and writes need `&mut`, so the store wraps a single
//! `Nidus` in `Arc<Mutex<_>>` and runs every operation inside `spawn_blocking`,
//! holding the mutex only inside the closure (never across `.await`).

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use nidus::{Filter, Hit, Nidus, Predicate, Record, Scope, SearchOpts, Value};

use super::{
    ChunkKind, Namespace, NamespaceMetadata, SearchOptions, SearchResult, StoreProvider,
    UpsertStats, VectorDocument, VectorStore,
};
use crate::config::StoreConfig;

// Reserved metadata keys inside a collection's nidus meta map.
const META_HWM_SHA: &str = "hwm_sha";
const META_EMBEDDER: &str = "embedder";
const META_EXTRA: &str = "extra";

// ── Provider ─────────────────────────────────────────────────────────────

pub struct NidusProvider;

impl StoreProvider for NidusProvider {
    fn name(&self) -> &str {
        "nidus"
    }

    fn validate(&self, config: &StoreConfig) -> Result<()> {
        if config.nidus.path.trim().is_empty() {
            bail!("store.nidus.path is required when store.provider=nidus");
        }
        Ok(())
    }

    fn build(&self, config: &StoreConfig, dimension: usize) -> Result<Box<dyn VectorStore>> {
        Ok(Box::new(NidusStore::open(&config.nidus.path, dimension)?))
    }
}

// ── Store ────────────────────────────────────────────────────────────────

pub struct NidusStore {
    db: Arc<Mutex<Nidus>>,
    dimension: usize,
}

impl NidusStore {
    /// Open (or create) a nidus store rooted at the directory `path`.
    pub fn open(path: &str, dimension: usize) -> Result<Self> {
        std::fs::create_dir_all(path)
            .with_context(|| format!("creating nidus store dir {path}"))?;
        let db = Nidus::open_dir(Path::new(path), dimension).with_context(|| {
            format!(
                "opening nidus store at {path} (dimension {dimension}); if this directory was \
                 created with a different embedding dimension, use a separate store.nidus.path \
                 or reindex with --full"
            )
        })?;
        Ok(Self::from_db(db, dimension))
    }

    /// In-memory store — used by tests.
    pub fn open_in_memory(dimension: usize) -> Result<Self> {
        let db = Nidus::open_in_memory(dimension).context("opening in-memory nidus store")?;
        Ok(Self::from_db(db, dimension))
    }

    fn from_db(db: Nidus, dimension: usize) -> Self {
        Self {
            db: Arc::new(Mutex::new(db)),
            dimension,
        }
    }

    /// Run a blocking nidus closure off the async runtime, holding the store
    /// mutex only inside the closure (never across `.await`).
    async fn with_db<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut Nidus) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let db = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = db.lock().map_err(|_| anyhow!("nidus mutex poisoned"))?;
            f(&mut guard)
        })
        .await
        .context("nidus blocking task failed")?
    }
}

#[async_trait]
impl VectorStore for NidusStore {
    async fn create_namespace(&self, ns: &Namespace, _dimension: usize) -> Result<()> {
        // Dimension is fixed store-wide at open; here we just register the
        // collection. Idempotent: creating an existing collection is a no-op.
        let ns = ns.as_str().to_string();
        self.with_db(move |db| {
            if !db.has_collection(&ns) {
                db.create_collection(&ns)
                    .with_context(|| format!("creating nidus collection {ns}"))?;
                db.flush()?;
            }
            Ok(())
        })
        .await
    }

    async fn delete_namespace(&self, ns: &Namespace) -> Result<()> {
        let ns = ns.as_str().to_string();
        self.with_db(move |db| {
            if db.has_collection(&ns) {
                db.drop_collection(&ns)
                    .with_context(|| format!("dropping nidus collection {ns}"))?;
                db.flush()?;
            }
            Ok(())
        })
        .await
    }

    async fn namespace_exists(&self, ns: &Namespace) -> Result<bool> {
        let ns = ns.as_str().to_string();
        self.with_db(move |db| Ok(db.has_collection(&ns))).await
    }

    async fn get_metadata(&self, ns: &Namespace) -> Result<NamespaceMetadata> {
        let ns = ns.as_str().to_string();
        self.with_db(move |db| {
            if !db.has_collection(&ns) {
                return Ok(NamespaceMetadata::default());
            }
            Ok(meta_from_map(db.get_meta(&ns)))
        })
        .await
    }

    async fn set_metadata(&self, ns: &Namespace, meta: &NamespaceMetadata) -> Result<()> {
        let ns = ns.as_str().to_string();
        let map = meta_to_map(meta);
        self.with_db(move |db| {
            // set_meta requires the collection to exist.
            if !db.has_collection(&ns) {
                db.create_collection(&ns)
                    .with_context(|| format!("creating nidus collection {ns}"))?;
            }
            db.set_meta(&ns, map)
                .with_context(|| format!("setting nidus metadata for {ns}"))?;
            db.flush()?;
            Ok(())
        })
        .await
    }

    async fn upsert(&self, ns: &Namespace, docs: &[VectorDocument]) -> Result<UpsertStats> {
        let ns = ns.as_str().to_string();
        let dimension = self.dimension;
        let records: Vec<Record> = docs
            .iter()
            .map(|doc| {
                if doc.vector.len() != dimension {
                    bail!(
                        "document '{}' has vector dimension {}, expected {dimension}",
                        doc.id,
                        doc.vector.len()
                    );
                }
                Ok(to_record(doc))
            })
            .collect::<Result<_>>()?;
        self.with_db(move |db| {
            if !db.has_collection(&ns) {
                db.create_collection(&ns)
                    .with_context(|| format!("creating nidus collection {ns}"))?;
            }
            let upserted = db
                .upsert(&ns, &records)
                .with_context(|| format!("upserting into nidus collection {ns}"))?;
            db.flush()?;
            Ok(UpsertStats {
                upserted,
                skipped: 0,
            })
        })
        .await
    }

    async fn delete_by_ids(&self, ns: &Namespace, ids: &[&str]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let ns = ns.as_str().to_string();
        let ids: Vec<String> = ids.iter().map(|s| s.to_string()).collect();
        self.with_db(move |db| {
            let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
            db.delete(&ns, &id_refs)
                .with_context(|| format!("deleting ids from nidus collection {ns}"))?;
            db.flush()?;
            Ok(())
        })
        .await
    }

    async fn delete_by_file(&self, ns: &Namespace, file_path: &str) -> Result<()> {
        let ns = ns.as_str().to_string();
        let file_path = file_path.to_string();
        self.with_db(move |db| {
            let filter = Filter(vec![Predicate::Eq(
                "file_path".into(),
                Value::Str(file_path),
            )]);
            db.delete_where(&ns, &filter)
                .with_context(|| format!("deleting by file from nidus collection {ns}"))?;
            db.flush()?;
            Ok(())
        })
        .await
    }

    async fn delete_by_glob(&self, ns: &Namespace, pattern: &str) -> Result<usize> {
        let ns = ns.as_str().to_string();
        let pattern = pattern.to_string();
        self.with_db(move |db| {
            let filter = Filter(vec![Predicate::Glob("file_path".into(), pattern)]);
            let deleted = db
                .delete_where(&ns, &filter)
                .with_context(|| format!("deleting by glob from nidus collection {ns}"))?;
            db.flush()?;
            Ok(deleted)
        })
        .await
    }

    async fn get_content_hashes(&self, ns: &Namespace) -> Result<HashMap<String, String>> {
        let ns = ns.as_str().to_string();
        self.with_db(move |db| {
            let mut map = HashMap::new();
            for r in db.get_all(&ns) {
                let is_file = matches!(attr_str(&r.attrs, "chunk_kind").as_deref(), Some("file"));
                if !is_file {
                    continue;
                }
                if let (Some(path), Some(hash)) = (
                    attr_str(&r.attrs, "file_path"),
                    attr_str(&r.attrs, "content_hash"),
                ) {
                    map.insert(path, hash);
                }
            }
            Ok(map)
        })
        .await
    }

    async fn list_documents(&self, ns: &Namespace) -> Result<Vec<VectorDocument>> {
        let ns = ns.as_str().to_string();
        self.with_db(move |db| Ok(db.get_all(&ns).into_iter().map(record_to_doc).collect()))
            .await
    }

    async fn search(
        &self,
        ns: &Namespace,
        query_vector: &[f32],
        opts: &SearchOptions,
    ) -> Result<Vec<SearchResult>> {
        let ns = ns.as_str().to_string();
        let qvec = query_vector.to_vec();
        let opts = opts.clone();
        self.with_db(move |db| {
            // Predicates nidus can apply pre-scoring as a conjunction (AND).
            let mut base: Vec<Predicate> = Vec::new();
            if let Some(ref kind) = opts.chunk_kind {
                base.push(Predicate::Eq(
                    "chunk_kind".into(),
                    Value::Str(kind.to_string()),
                ));
            }
            if let Some(ref lang) = opts.language {
                base.push(Predicate::Eq("language".into(), Value::Str(lang.clone())));
            }

            let run = |db: &mut Nidus, prefix: Option<&str>| -> Result<Vec<Hit>> {
                let mut preds = base.clone();
                if let Some(p) = prefix {
                    preds.push(Predicate::Glob("file_path".into(), format!("{p}*")));
                }
                let sopts = SearchOpts {
                    top_k: opts.top_k,
                    filter: Filter(preds),
                    min_score: opts.min_score,
                };
                db.search(Scope::from(ns.as_str()), &qvec, &sopts)
                    .with_context(|| format!("searching nidus collection {ns}"))
            };

            // nidus filters are AND-only, so multiple path prefixes (OR
            // semantics) are run as separate searches and merged by id,
            // keeping the best score, then truncated to top_k.
            let hits = match opts.path_prefixes.len() {
                0 => run(db, None)?,
                1 => run(db, Some(&opts.path_prefixes[0]))?,
                _ => {
                    let mut merged: HashMap<String, Hit> = HashMap::new();
                    for p in &opts.path_prefixes {
                        for h in run(db, Some(p))? {
                            merged
                                .entry(h.id.clone())
                                .and_modify(|e| {
                                    if h.score > e.score {
                                        *e = h.clone();
                                    }
                                })
                                .or_insert(h);
                        }
                    }
                    let mut v: Vec<Hit> = merged.into_values().collect();
                    v.sort_by(|a, b| {
                        b.score
                            .partial_cmp(&a.score)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                    v.truncate(opts.top_k);
                    v
                }
            };

            Ok(hits.into_iter().map(hit_to_result).collect())
        })
        .await
    }
}

// ── Conversion helpers (pure) ──────────────────────────────────────────────

fn to_record(doc: &VectorDocument) -> Record {
    let mut attrs: BTreeMap<String, Value> = BTreeMap::new();
    attrs.insert("summary".into(), Value::Str(doc.summary.clone()));
    attrs.insert("file_path".into(), Value::Str(doc.file_path.clone()));
    attrs.insert("chunk_kind".into(), Value::Str(doc.chunk_kind.to_string()));
    if let Some(ref s) = doc.symbol_name {
        attrs.insert("symbol_name".into(), Value::Str(s.clone()));
    }
    if let Some(ref s) = doc.symbol_kind {
        attrs.insert("symbol_kind".into(), Value::Str(s.clone()));
    }
    if let Some(n) = doc.start_line {
        attrs.insert("start_line".into(), Value::Int(i64::from(n)));
    }
    if let Some(n) = doc.end_line {
        attrs.insert("end_line".into(), Value::Int(i64::from(n)));
    }
    if let Some(ref s) = doc.language {
        attrs.insert("language".into(), Value::Str(s.clone()));
    }
    if let Some(ref s) = doc.content_hash {
        attrs.insert("content_hash".into(), Value::Str(s.clone()));
    }
    // Omitting the key for `None` (vs storing `List([])` for `Some(vec![])`)
    // preserves the not-indexed vs empty-list distinction on read-back.
    if let Some(ref v) = doc.calls {
        attrs.insert("calls".into(), Value::List(v.clone()));
    }
    if let Some(ref v) = doc.called_by {
        attrs.insert("called_by".into(), Value::List(v.clone()));
    }
    Record {
        id: doc.id.clone(),
        vector: Some(doc.vector.clone()),
        attrs,
    }
}

fn record_to_doc(r: Record) -> VectorDocument {
    VectorDocument {
        id: r.id,
        vector: r.vector.unwrap_or_default(),
        summary: attr_str(&r.attrs, "summary").unwrap_or_default(),
        file_path: attr_str(&r.attrs, "file_path").unwrap_or_default(),
        chunk_kind: attr_str(&r.attrs, "chunk_kind")
            .map(|s| parse_chunk_kind(&s))
            .unwrap_or(ChunkKind::File),
        symbol_name: attr_str(&r.attrs, "symbol_name"),
        symbol_kind: attr_str(&r.attrs, "symbol_kind"),
        start_line: attr_u32(&r.attrs, "start_line"),
        end_line: attr_u32(&r.attrs, "end_line"),
        language: attr_str(&r.attrs, "language"),
        content_hash: attr_str(&r.attrs, "content_hash"),
        calls: attr_list(&r.attrs, "calls"),
        called_by: attr_list(&r.attrs, "called_by"),
    }
}

fn hit_to_result(h: Hit) -> SearchResult {
    SearchResult {
        id: h.id,
        score: h.score,
        file_path: attr_str(&h.attrs, "file_path").unwrap_or_default(),
        chunk_kind: attr_str(&h.attrs, "chunk_kind")
            .map(|s| parse_chunk_kind(&s))
            .unwrap_or(ChunkKind::File),
        symbol_name: attr_str(&h.attrs, "symbol_name"),
        symbol_kind: attr_str(&h.attrs, "symbol_kind"),
        summary: attr_str(&h.attrs, "summary").unwrap_or_default(),
        start_line: attr_u32(&h.attrs, "start_line"),
        end_line: attr_u32(&h.attrs, "end_line"),
        language: attr_str(&h.attrs, "language"),
        calls: attr_list(&h.attrs, "calls"),
        called_by: attr_list(&h.attrs, "called_by"),
    }
}

fn attr_str(attrs: &BTreeMap<String, Value>, key: &str) -> Option<String> {
    match attrs.get(key) {
        Some(Value::Str(s)) => Some(s.clone()),
        _ => None,
    }
}

fn attr_u32(attrs: &BTreeMap<String, Value>, key: &str) -> Option<u32> {
    match attrs.get(key) {
        Some(Value::Int(i)) => u32::try_from(*i).ok(),
        _ => None,
    }
}

/// Absent key or non-list → `None`; `List(v)` → `Some(v)`. Preserves the
/// not-indexed (`None`) vs empty (`Some(vec![])`) call-graph distinction.
fn attr_list(attrs: &BTreeMap<String, Value>, key: &str) -> Option<Vec<String>> {
    match attrs.get(key) {
        Some(Value::List(v)) => Some(v.clone()),
        _ => None,
    }
}

fn parse_chunk_kind(s: &str) -> ChunkKind {
    match s {
        "symbol" => ChunkKind::Symbol,
        _ => ChunkKind::File,
    }
}

fn meta_to_map(meta: &NamespaceMetadata) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    if let Some(ref h) = meta.hwm_sha {
        map.insert(META_HWM_SHA.into(), h.clone());
    }
    if let Some(ref e) = meta.embedder {
        map.insert(META_EMBEDDER.into(), e.clone());
    }
    if !meta.extra.is_empty() {
        map.insert(
            META_EXTRA.into(),
            serde_json::to_string(&meta.extra).unwrap_or_else(|_| "{}".into()),
        );
    }
    map
}

fn meta_from_map(map: BTreeMap<String, String>) -> NamespaceMetadata {
    NamespaceMetadata {
        hwm_sha: map.get(META_HWM_SHA).cloned(),
        embedder: map.get(META_EMBEDDER).cloned(),
        extra: map
            .get(META_EXTRA)
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────
//
// The conversion helpers are pure Rust and Miri-safe. The store tests use a
// tokio runtime (its reactor needs kqueue/epoll FFI), so they carry
// `#[cfg_attr(miri, ignore)]` per the project's Miri rules — nidus itself is
// pure Rust.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{NidusConfig, TurbopufferConfig};

    fn store_config(path: &str) -> StoreConfig {
        StoreConfig {
            provider: "nidus".into(),
            turbopuffer: TurbopufferConfig {
                api_key: String::new(),
            },
            nidus: NidusConfig { path: path.into() },
        }
    }

    fn file_doc(id: &str, vector: Vec<f32>, file_path: &str, content_hash: &str) -> VectorDocument {
        VectorDocument {
            id: id.into(),
            vector,
            summary: format!("summary for {id}"),
            file_path: file_path.into(),
            chunk_kind: ChunkKind::File,
            symbol_name: None,
            symbol_kind: None,
            start_line: None,
            end_line: None,
            language: Some("rust".into()),
            content_hash: Some(content_hash.into()),
            calls: None,
            called_by: None,
        }
    }

    fn symbol_doc(id: &str, vector: Vec<f32>, file_path: &str, name: &str) -> VectorDocument {
        VectorDocument {
            id: id.into(),
            vector,
            summary: format!("summary for {id}"),
            file_path: file_path.into(),
            chunk_kind: ChunkKind::Symbol,
            symbol_name: Some(name.into()),
            symbol_kind: Some("function".into()),
            start_line: Some(10),
            end_line: Some(25),
            language: Some("rust".into()),
            content_hash: None,
            calls: Some(vec!["other_fn".into()]),
            called_by: Some(vec![]),
        }
    }

    async fn seeded_store() -> NidusStore {
        let store = NidusStore::open_in_memory(3).unwrap();
        let ns = Namespace::from("repo");
        store.create_namespace(&ns, 3).await.unwrap();
        store
    }

    // ── Conversion helpers (pure, Miri-safe) ──────────────────────────

    #[test]
    fn parse_chunk_kind_cases() {
        assert_eq!(parse_chunk_kind("symbol"), ChunkKind::Symbol);
        assert_eq!(parse_chunk_kind("file"), ChunkKind::File);
        assert_eq!(parse_chunk_kind("nonsense"), ChunkKind::File);
    }

    #[test]
    fn record_round_trip_preserves_fields() {
        let sym = symbol_doc("s1", vec![0.1, 0.2, 0.3], "src/lib.rs", "process");
        let back = record_to_doc(to_record(&sym));
        assert_eq!(back.id, "s1");
        assert_eq!(back.vector, vec![0.1, 0.2, 0.3]);
        assert_eq!(back.chunk_kind, ChunkKind::Symbol);
        assert_eq!(back.symbol_name.as_deref(), Some("process"));
        assert_eq!(back.start_line, Some(10));
        assert_eq!(back.end_line, Some(25));
        // None vs Some([]) distinction preserved through attr omission.
        assert_eq!(back.calls, Some(vec!["other_fn".to_string()]));
        assert_eq!(back.called_by, Some(vec![]));
    }

    #[test]
    fn record_round_trip_preserves_none_call_graph() {
        let doc = file_doc("a", vec![1.0, 0.0, 0.0], "src/a.rs", "h1");
        let back = record_to_doc(to_record(&doc));
        // file_doc leaves calls/called_by None — must stay None, not Some([]).
        assert_eq!(back.calls, None);
        assert_eq!(back.called_by, None);
        assert_eq!(back.content_hash.as_deref(), Some("h1"));
    }

    #[test]
    fn metadata_map_round_trip() {
        let mut extra = HashMap::new();
        extra.insert("version".to_string(), "2".to_string());
        let meta = NamespaceMetadata {
            hwm_sha: Some("abc123".into()),
            embedder: Some("voyage/voyage-code-3".into()),
            extra,
        };
        let back = meta_from_map(meta_to_map(&meta));
        assert_eq!(back.hwm_sha.as_deref(), Some("abc123"));
        assert_eq!(back.embedder.as_deref(), Some("voyage/voyage-code-3"));
        assert_eq!(back.extra["version"], "2");
    }

    #[test]
    fn metadata_map_empty_is_empty() {
        let map = meta_to_map(&NamespaceMetadata::default());
        assert!(map.is_empty());
        let back = meta_from_map(map);
        assert!(back.hwm_sha.is_none() && back.embedder.is_none() && back.extra.is_empty());
    }

    // ── Provider ──────────────────────────────────────────────────────

    #[test]
    fn provider_name() {
        assert_eq!(NidusProvider.name(), "nidus");
    }

    #[test]
    fn provider_validate_requires_path() {
        assert!(
            NidusProvider
                .validate(&store_config("/tmp/x-nidus"))
                .is_ok()
        );
        let err = NidusProvider.validate(&store_config("  ")).unwrap_err();
        assert!(err.to_string().contains("store.nidus.path"));
    }

    // ── Schema / dimension ────────────────────────────────────────────

    #[cfg_attr(miri, ignore)]
    #[test]
    fn open_in_memory_succeeds() {
        assert!(NidusStore::open_in_memory(8).is_ok());
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn reopen_with_mismatched_dimension_errors() {
        let dir = std::env::temp_dir().join(format!("wdpkr-nidus-dim-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let path = dir.to_str().unwrap();

        assert!(NidusStore::open(path, 3).is_ok());
        let reopened = NidusStore::open(path, 5);
        assert!(
            reopened.is_err(),
            "reopening with a different dimension should error"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    // ── Namespace lifecycle ───────────────────────────────────────────

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn namespace_lifecycle() {
        let store = NidusStore::open_in_memory(3).unwrap();
        let ns = Namespace::from("repo");
        assert!(!store.namespace_exists(&ns).await.unwrap());
        store.create_namespace(&ns, 3).await.unwrap();
        assert!(store.namespace_exists(&ns).await.unwrap());
        // Idempotent.
        store.create_namespace(&ns, 3).await.unwrap();
        store.delete_namespace(&ns).await.unwrap();
        assert!(!store.namespace_exists(&ns).await.unwrap());
    }

    // ── Metadata ──────────────────────────────────────────────────────

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn metadata_defaults_then_round_trips() {
        let store = seeded_store().await;
        let ns = Namespace::from("repo");
        let meta = store.get_metadata(&ns).await.unwrap();
        assert!(meta.hwm_sha.is_none() && meta.embedder.is_none() && meta.extra.is_empty());

        let mut extra = HashMap::new();
        extra.insert("version".to_string(), "2".to_string());
        store
            .set_metadata(
                &ns,
                &NamespaceMetadata {
                    hwm_sha: Some("abc123".into()),
                    embedder: Some("voyage/voyage-code-3".into()),
                    extra,
                },
            )
            .await
            .unwrap();

        let back = store.get_metadata(&ns).await.unwrap();
        assert_eq!(back.hwm_sha.as_deref(), Some("abc123"));
        assert_eq!(back.embedder.as_deref(), Some("voyage/voyage-code-3"));
        assert_eq!(back.extra["version"], "2");
    }

    // ── Upsert + search ───────────────────────────────────────────────

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn upsert_then_search_ranks_by_cosine() {
        let store = seeded_store().await;
        let ns = Namespace::from("repo");
        let docs = vec![
            file_doc("a", vec![1.0, 0.0, 0.0], "src/a.rs", "h1"),
            file_doc("b", vec![0.0, 1.0, 0.0], "src/b.rs", "h2"),
            file_doc("c", vec![0.9, 0.1, 0.0], "src/c.rs", "h3"),
        ];
        let stats = store.upsert(&ns, &docs).await.unwrap();
        assert_eq!(stats.upserted, 3);

        let opts = SearchOptions {
            top_k: 10,
            ..Default::default()
        };
        let results = store.search(&ns, &[1.0, 0.0, 0.0], &opts).await.unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].id, "a"); // exact match ranks first
        assert!((results[0].score - 1.0).abs() < 1e-5);
        assert_eq!(results[1].id, "c"); // near match second
        assert_eq!(results[2].id, "b"); // orthogonal last
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn upsert_is_idempotent_overwrite() {
        let store = seeded_store().await;
        let ns = Namespace::from("repo");
        store
            .upsert(&ns, &[file_doc("a", vec![1.0, 0.0, 0.0], "src/a.rs", "h1")])
            .await
            .unwrap();
        store
            .upsert(
                &ns,
                &[file_doc("a", vec![0.0, 1.0, 0.0], "src/a2.rs", "h2")],
            )
            .await
            .unwrap();
        let docs = store.list_documents(&ns).await.unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].file_path, "src/a2.rs");
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn search_min_score_filters() {
        let store = seeded_store().await;
        let ns = Namespace::from("repo");
        store
            .upsert(
                &ns,
                &[
                    file_doc("a", vec![1.0, 0.0, 0.0], "src/a.rs", "h1"),
                    file_doc("b", vec![0.0, 1.0, 0.0], "src/b.rs", "h2"),
                ],
            )
            .await
            .unwrap();
        let opts = SearchOptions {
            top_k: 10,
            min_score: Some(0.5),
            ..Default::default()
        };
        let results = store.search(&ns, &[1.0, 0.0, 0.0], &opts).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "a");
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn search_filters_by_prefix_kind_and_language() {
        let store = seeded_store().await;
        let ns = Namespace::from("repo");
        store
            .upsert(
                &ns,
                &[
                    file_doc("f1", vec![1.0, 0.0, 0.0], "src/keep/a.rs", "h1"),
                    file_doc("f2", vec![1.0, 0.0, 0.0], "other/b.rs", "h2"),
                    symbol_doc("s1", vec![1.0, 0.0, 0.0], "src/keep/c.rs", "go"),
                ],
            )
            .await
            .unwrap();

        let prefix = SearchOptions {
            top_k: 10,
            path_prefixes: vec!["src/keep/".into()],
            ..Default::default()
        };
        let r = store.search(&ns, &[1.0, 0.0, 0.0], &prefix).await.unwrap();
        assert_eq!(r.len(), 2);
        assert!(r.iter().all(|x| x.file_path.starts_with("src/keep/")));

        let symbols = SearchOptions {
            top_k: 10,
            chunk_kind: Some(ChunkKind::Symbol),
            ..Default::default()
        };
        let r = store.search(&ns, &[1.0, 0.0, 0.0], &symbols).await.unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].id, "s1");
        assert_eq!(r[0].symbol_name.as_deref(), Some("go"));
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn search_prefix_matches_nested_paths() {
        // Path-prefix scope must match files at any depth (mirrors DuckDB GLOB
        // and Turbopuffer Glob semantics: '*' crosses '/').
        let store = seeded_store().await;
        let ns = Namespace::from("repo");
        store
            .upsert(
                &ns,
                &[
                    file_doc("a", vec![1.0, 0.0, 0.0], "src/finance/rate.rs", "h1"),
                    file_doc("b", vec![1.0, 0.0, 0.0], "src/finance/deep/sub.rs", "h2"),
                    file_doc("c", vec![1.0, 0.0, 0.0], "src/other/x.rs", "h3"),
                ],
            )
            .await
            .unwrap();
        let opts = SearchOptions {
            top_k: 10,
            path_prefixes: vec!["src/finance/".into()],
            ..Default::default()
        };
        let r = store.search(&ns, &[1.0, 0.0, 0.0], &opts).await.unwrap();
        let ids: Vec<&str> = r.iter().map(|x| x.id.as_str()).collect();
        assert!(ids.contains(&"a"), "should match direct child");
        assert!(ids.contains(&"b"), "should match nested descendant");
        assert!(!ids.contains(&"c"), "should not match sibling dir");
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn search_multiple_prefixes_or_semantics() {
        let store = seeded_store().await;
        let ns = Namespace::from("repo");
        store
            .upsert(
                &ns,
                &[
                    file_doc("a", vec![1.0, 0.0, 0.0], "src/finance/x.rs", "h1"),
                    file_doc("b", vec![0.9, 0.1, 0.0], "src/annuity/y.rs", "h2"),
                    file_doc("c", vec![0.8, 0.2, 0.0], "src/other/z.rs", "h3"),
                ],
            )
            .await
            .unwrap();
        let opts = SearchOptions {
            top_k: 10,
            path_prefixes: vec!["src/finance/".into(), "src/annuity/".into()],
            ..Default::default()
        };
        let r = store.search(&ns, &[1.0, 0.0, 0.0], &opts).await.unwrap();
        let ids: Vec<&str> = r.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"a") && ids.contains(&"b"));
        assert!(!ids.contains(&"c"));
    }

    // ── Deletes ───────────────────────────────────────────────────────

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn delete_by_file_and_ids() {
        let store = seeded_store().await;
        let ns = Namespace::from("repo");
        store
            .upsert(
                &ns,
                &[
                    file_doc("a", vec![1.0, 0.0, 0.0], "src/a.rs", "h1"),
                    file_doc("b", vec![0.0, 1.0, 0.0], "src/b.rs", "h2"),
                    file_doc("c", vec![0.0, 0.0, 1.0], "src/c.rs", "h3"),
                ],
            )
            .await
            .unwrap();
        store.delete_by_file(&ns, "src/a.rs").await.unwrap();
        store.delete_by_ids(&ns, &["b"]).await.unwrap();
        let docs = store.list_documents(&ns).await.unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].id, "c");
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn delete_by_glob_returns_count() {
        let store = seeded_store().await;
        let ns = Namespace::from("repo");
        store
            .upsert(
                &ns,
                &[
                    file_doc("a", vec![1.0, 0.0, 0.0], "src/x/a.rs", "h1"),
                    file_doc("b", vec![0.0, 1.0, 0.0], "src/x/b.rs", "h2"),
                    file_doc("c", vec![0.0, 0.0, 1.0], "src/y/c.rs", "h3"),
                ],
            )
            .await
            .unwrap();
        let deleted = store.delete_by_glob(&ns, "src/x/*").await.unwrap();
        assert_eq!(deleted, 2);
        let docs = store.list_documents(&ns).await.unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].id, "c");
    }

    // ── Content hashes ────────────────────────────────────────────────

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn content_hashes_only_file_chunks() {
        let store = seeded_store().await;
        let ns = Namespace::from("repo");
        store
            .upsert(
                &ns,
                &[
                    file_doc("a", vec![1.0, 0.0, 0.0], "src/a.rs", "hash-a"),
                    symbol_doc("s", vec![0.0, 1.0, 0.0], "src/a.rs", "fn_a"),
                ],
            )
            .await
            .unwrap();
        let hashes = store.get_content_hashes(&ns).await.unwrap();
        assert_eq!(hashes.len(), 1);
        assert_eq!(hashes["src/a.rs"], "hash-a");
    }

    // ── Namespace isolation ───────────────────────────────────────────

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn namespaces_are_isolated() {
        let store = NidusStore::open_in_memory(3).unwrap();
        let a = Namespace::from("repo-a");
        let b = Namespace::from("repo-b");
        store.create_namespace(&a, 3).await.unwrap();
        store.create_namespace(&b, 3).await.unwrap();
        store
            .upsert(&a, &[file_doc("x", vec![1.0, 0.0, 0.0], "a.rs", "h")])
            .await
            .unwrap();
        assert_eq!(store.list_documents(&a).await.unwrap().len(), 1);
        assert_eq!(store.list_documents(&b).await.unwrap().len(), 0);
    }
}
