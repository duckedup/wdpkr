//! Local DuckDB vector store adapter.
//!
//! A file-backed [`VectorStore`] so wdpkr's storage leg can run with no
//! hosted third party. One DuckDB file holds every namespace; the namespace
//! is a **column**, not a separate file or table.
//!
//! Search is **exact brute-force**: vectors are stored in a fixed-size
//! `FLOAT[dim]` ARRAY column and ranked with the core `array_cosine_distance`
//! function (no DuckDB extension required, fully offline). Score mirrors the
//! Turbopuffer adapter: `score = 1 - array_cosine_distance` (cosine
//! similarity), so `min_score` and the output layer are unchanged.
//!
//! The schema is **HNSW-ready**: because vectors are a fixed-size ARRAY ranked
//! by `array_cosine_distance ... ORDER BY ... LIMIT k`, adding the `vss`
//! extension's HNSW index later is a pure additive `CREATE INDEX` with no
//! query or schema change.
//!
//! ## Type handling
//!
//! The DuckDB driver's ARRAY/LIST/MAP binding is avoided for everything except
//! the `vector` column:
//! - **vector**: written as an inlined numeric list literal
//!   (`[..]::FLOAT[dim]`, numbers only → injection-safe); read back via
//!   `CAST(vector AS VARCHAR)` and parsed.
//! - **calls / called_by / metadata.extra**: stored as JSON text and
//!   (de)serialized in Rust. `NULL` distinguishes "not indexed" (`None`) from
//!   an empty list (`Some(vec![])`).
//!
//! ## Dimension constraint
//!
//! A single DuckDB file is tied to one embedding dimension (recorded in
//! `wdpkr_meta`). Opening it with a different dimension is a hard error — one
//! install uses one embedder. Use a separate `duckdb_path` or reindex to
//! change embedders.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use duckdb::{Connection, params, params_from_iter};

use super::{
    ChunkKind, Namespace, NamespaceMetadata, SearchOptions, SearchResult, StoreProvider,
    UpsertStats, VectorDocument, VectorStore,
};
use crate::config::StoreConfig;

const UPSERT_BATCH_SIZE: usize = 500;

// ── Provider ─────────────────────────────────────────────────────────────

pub struct DuckdbProvider;

impl StoreProvider for DuckdbProvider {
    fn name(&self) -> &str {
        "duckdb"
    }

    fn validate(&self, config: &StoreConfig) -> Result<()> {
        if config.duckdb.path.trim().is_empty() {
            bail!("store.duckdb.path is required when store.provider=duckdb");
        }
        Ok(())
    }

    fn build(&self, config: &StoreConfig, dimension: usize) -> Result<Box<dyn VectorStore>> {
        Ok(Box::new(DuckdbStore::open(&config.duckdb.path, dimension)?))
    }
}

// ── Store ────────────────────────────────────────────────────────────────

pub struct DuckdbStore {
    conn: Arc<Mutex<Connection>>,
    dimension: usize,
}

impl DuckdbStore {
    /// Open (or create) a DuckDB database at `path` and initialize the schema.
    pub fn open(path: &str, dimension: usize) -> Result<Self> {
        if let Some(parent) = Path::new(path).parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating DuckDB parent dir {}", parent.display()))?;
        }
        let conn =
            Connection::open(path).with_context(|| format!("opening DuckDB database at {path}"))?;
        Self::from_connection(conn, dimension)
    }

    /// In-memory store — used by tests.
    pub fn open_in_memory(dimension: usize) -> Result<Self> {
        let conn = Connection::open_in_memory().context("opening in-memory DuckDB")?;
        Self::from_connection(conn, dimension)
    }

    fn from_connection(conn: Connection, dimension: usize) -> Result<Self> {
        init_schema(&conn, dimension)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            dimension,
        })
    }

    /// Run a blocking DuckDB closure off the async runtime, holding the
    /// connection mutex only inside the closure (never across `.await`).
    async fn with_conn<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&mut Connection) -> Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut guard = conn.lock().map_err(|_| anyhow!("DuckDB mutex poisoned"))?;
            f(&mut guard)
        })
        .await
        .context("DuckDB blocking task failed")?
    }
}

/// Create tables if absent and pin / verify the embedding dimension.
fn init_schema(conn: &Connection, dimension: usize) -> Result<()> {
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS wdpkr_meta (dimension UINTEGER);
         CREATE TABLE IF NOT EXISTS namespaces (
             namespace TEXT PRIMARY KEY,
             hwm_sha TEXT,
             embedder TEXT,
             extra_json TEXT
         );
         CREATE TABLE IF NOT EXISTS documents (
             namespace TEXT NOT NULL,
             id TEXT NOT NULL,
             vector FLOAT[{dimension}] NOT NULL,
             summary TEXT NOT NULL,
             file_path TEXT NOT NULL,
             chunk_kind TEXT NOT NULL,
             symbol_name TEXT,
             symbol_kind TEXT,
             start_line UINTEGER,
             end_line UINTEGER,
             language TEXT,
             content_hash TEXT,
             calls_json TEXT,
             called_by_json TEXT,
             PRIMARY KEY (namespace, id)
         );"
    ))
    .context("initializing DuckDB schema")?;

    let existing: Vec<u32> = {
        let mut stmt = conn.prepare("SELECT dimension FROM wdpkr_meta LIMIT 1")?;
        let rows = stmt.query_map([], |row| row.get::<_, u32>(0))?;
        rows.collect::<duckdb::Result<Vec<_>>>()?
    };
    match existing.first() {
        Some(&d) if d as usize != dimension => bail!(
            "DuckDB database was created with embedding dimension {d}, but the current \
             embedder produces dimension {dimension}; use a different store.duckdb.path \
             or reindex with --full"
        ),
        Some(_) => {}
        None => {
            conn.execute(
                "INSERT INTO wdpkr_meta (dimension) VALUES (?)",
                params![dimension as u32],
            )?;
        }
    }
    Ok(())
}

#[async_trait]
impl VectorStore for DuckdbStore {
    async fn create_namespace(&self, ns: &Namespace, _dimension: usize) -> Result<()> {
        // Schema (incl. dimension) is pinned at open; here we just register
        // the namespace row.
        let ns = ns.as_str().to_string();
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO namespaces (namespace) VALUES (?) ON CONFLICT (namespace) DO NOTHING",
                params![ns],
            )?;
            Ok(())
        })
        .await
    }

    async fn delete_namespace(&self, ns: &Namespace) -> Result<()> {
        let ns = ns.as_str().to_string();
        self.with_conn(move |conn| {
            conn.execute("DELETE FROM documents WHERE namespace = ?", params![ns])?;
            conn.execute("DELETE FROM namespaces WHERE namespace = ?", params![ns])?;
            Ok(())
        })
        .await
    }

    async fn namespace_exists(&self, ns: &Namespace) -> Result<bool> {
        let ns = ns.as_str().to_string();
        self.with_conn(move |conn| {
            let count: i64 = conn.query_row(
                "SELECT count(*) FROM namespaces WHERE namespace = ?",
                params![ns],
                |row| row.get(0),
            )?;
            Ok(count > 0)
        })
        .await
    }

    async fn get_metadata(&self, ns: &Namespace) -> Result<NamespaceMetadata> {
        let ns = ns.as_str().to_string();
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT hwm_sha, embedder, extra_json FROM namespaces WHERE namespace = ?",
            )?;
            let rows = stmt.query_map(params![ns], |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?;
            let row = rows.collect::<duckdb::Result<Vec<_>>>()?.into_iter().next();
            match row {
                Some((hwm_sha, embedder, extra_json)) => Ok(NamespaceMetadata {
                    hwm_sha,
                    embedder,
                    extra: extra_json
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default(),
                }),
                None => Ok(NamespaceMetadata::default()),
            }
        })
        .await
    }

    async fn set_metadata(&self, ns: &Namespace, meta: &NamespaceMetadata) -> Result<()> {
        let ns = ns.as_str().to_string();
        let hwm_sha = meta.hwm_sha.clone();
        let embedder = meta.embedder.clone();
        let extra_json = if meta.extra.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&meta.extra).unwrap_or_else(|_| "{}".into()))
        };
        self.with_conn(move |conn| {
            conn.execute(
                "INSERT INTO namespaces (namespace, hwm_sha, embedder, extra_json) \
                 VALUES (?, ?, ?, ?) \
                 ON CONFLICT (namespace) DO UPDATE SET \
                 hwm_sha = excluded.hwm_sha, embedder = excluded.embedder, \
                 extra_json = excluded.extra_json",
                params![ns, hwm_sha, embedder, extra_json],
            )?;
            Ok(())
        })
        .await
    }

    async fn upsert(&self, ns: &Namespace, docs: &[VectorDocument]) -> Result<UpsertStats> {
        let ns = ns.as_str().to_string();
        let docs = docs.to_vec();
        let dimension = self.dimension;
        self.with_conn(move |conn| {
            let mut upserted = 0usize;
            for chunk in docs.chunks(UPSERT_BATCH_SIZE) {
                let tx = conn.transaction()?;
                for doc in chunk {
                    if doc.vector.len() != dimension {
                        bail!(
                            "document '{}' has vector dimension {}, expected {dimension}",
                            doc.id,
                            doc.vector.len()
                        );
                    }
                    let sql = format!(
                        "INSERT INTO documents \
                         (namespace, id, vector, summary, file_path, chunk_kind, symbol_name, \
                          symbol_kind, start_line, end_line, language, content_hash, calls_json, \
                          called_by_json) \
                         VALUES (?, ?, {vec_lit}, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
                         ON CONFLICT (namespace, id) DO UPDATE SET \
                         vector = excluded.vector, summary = excluded.summary, \
                         file_path = excluded.file_path, chunk_kind = excluded.chunk_kind, \
                         symbol_name = excluded.symbol_name, symbol_kind = excluded.symbol_kind, \
                         start_line = excluded.start_line, end_line = excluded.end_line, \
                         language = excluded.language, content_hash = excluded.content_hash, \
                         calls_json = excluded.calls_json, called_by_json = excluded.called_by_json",
                        vec_lit = vector_literal(&doc.vector),
                    );
                    tx.execute(
                        &sql,
                        params![
                            ns,
                            doc.id,
                            doc.summary,
                            doc.file_path,
                            doc.chunk_kind.to_string(),
                            doc.symbol_name,
                            doc.symbol_kind,
                            doc.start_line,
                            doc.end_line,
                            doc.language,
                            doc.content_hash,
                            vec_to_json(&doc.calls),
                            vec_to_json(&doc.called_by),
                        ],
                    )?;
                    upserted += 1;
                }
                tx.commit()?;
            }
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
        self.with_conn(move |conn| {
            let placeholders = vec!["?"; ids.len()].join(", ");
            let sql =
                format!("DELETE FROM documents WHERE namespace = ? AND id IN ({placeholders})");
            let mut bind: Vec<String> = Vec::with_capacity(ids.len() + 1);
            bind.push(ns);
            bind.extend(ids);
            conn.execute(&sql, params_from_iter(bind.iter()))?;
            Ok(())
        })
        .await
    }

    async fn delete_by_file(&self, ns: &Namespace, file_path: &str) -> Result<()> {
        let ns = ns.as_str().to_string();
        let file_path = file_path.to_string();
        self.with_conn(move |conn| {
            conn.execute(
                "DELETE FROM documents WHERE namespace = ? AND file_path = ?",
                params![ns, file_path],
            )?;
            Ok(())
        })
        .await
    }

    async fn delete_by_glob(&self, ns: &Namespace, pattern: &str) -> Result<usize> {
        let ns = ns.as_str().to_string();
        let pattern = pattern.to_string();
        self.with_conn(move |conn| {
            let count: i64 = conn.query_row(
                "SELECT count(*) FROM documents WHERE namespace = ? AND file_path GLOB ?",
                params![ns, pattern],
                |row| row.get(0),
            )?;
            conn.execute(
                "DELETE FROM documents WHERE namespace = ? AND file_path GLOB ?",
                params![ns, pattern],
            )?;
            Ok(count as usize)
        })
        .await
    }

    async fn get_content_hashes(&self, ns: &Namespace) -> Result<HashMap<String, String>> {
        let ns = ns.as_str().to_string();
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT file_path, content_hash FROM documents \
                 WHERE namespace = ? AND chunk_kind = 'file' AND content_hash IS NOT NULL",
            )?;
            let rows = stmt.query_map(params![ns], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut map = HashMap::new();
            for row in rows {
                let (path, hash) = row?;
                map.insert(path, hash);
            }
            Ok(map)
        })
        .await
    }

    async fn list_documents(&self, ns: &Namespace) -> Result<Vec<VectorDocument>> {
        let ns = ns.as_str().to_string();
        self.with_conn(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT id, CAST(vector AS VARCHAR), summary, file_path, chunk_kind, symbol_name, \
                 symbol_kind, start_line, end_line, language, content_hash, calls_json, \
                 called_by_json FROM documents WHERE namespace = ?",
            )?;
            let rows = stmt.query_map(params![ns], |row| {
                let chunk_kind_str: String = row.get(4)?;
                Ok(VectorDocument {
                    id: row.get(0)?,
                    vector: parse_vector(&row.get::<_, String>(1)?),
                    summary: row.get(2)?,
                    file_path: row.get(3)?,
                    chunk_kind: parse_chunk_kind(&chunk_kind_str),
                    symbol_name: row.get(5)?,
                    symbol_kind: row.get(6)?,
                    start_line: row.get(7)?,
                    end_line: row.get(8)?,
                    language: row.get(9)?,
                    content_hash: row.get(10)?,
                    calls: json_to_vec(row.get::<_, Option<String>>(11)?),
                    called_by: json_to_vec(row.get::<_, Option<String>>(12)?),
                })
            })?;
            rows.collect::<duckdb::Result<Vec<_>>>()
                .context("reading DuckDB documents")
        })
        .await
    }

    async fn search(
        &self,
        ns: &Namespace,
        query_vector: &[f32],
        opts: &SearchOptions,
    ) -> Result<Vec<SearchResult>> {
        let ns = ns.as_str().to_string();
        let qvec = vector_literal(query_vector);
        let opts = opts.clone();
        self.with_conn(move |conn| {
            let mut where_clauses = vec!["namespace = ?".to_string()];
            let mut bind: Vec<String> = vec![ns];

            match opts.path_prefixes.len() {
                0 => {}
                _ => {
                    let ors: Vec<&str> = opts
                        .path_prefixes
                        .iter()
                        .map(|_| "file_path GLOB ?")
                        .collect();
                    where_clauses.push(format!("({})", ors.join(" OR ")));
                    for p in &opts.path_prefixes {
                        bind.push(format!("{p}*"));
                    }
                }
            }
            if let Some(ref kind) = opts.chunk_kind {
                where_clauses.push("chunk_kind = ?".into());
                bind.push(kind.to_string());
            }
            if let Some(ref lang) = opts.language {
                where_clauses.push("language = ?".into());
                bind.push(lang.clone());
            }

            let sql = format!(
                "SELECT id, summary, file_path, chunk_kind, symbol_name, symbol_kind, \
                 start_line, end_line, language, calls_json, called_by_json, \
                 CAST(array_cosine_distance(vector, {qvec}) AS DOUBLE) AS dist \
                 FROM documents WHERE {where_clause} ORDER BY dist ASC LIMIT {limit}",
                where_clause = where_clauses.join(" AND "),
                limit = opts.top_k,
            );

            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(bind.iter()), |row| {
                let dist: f64 = row.get(11)?;
                let chunk_kind_str: String = row.get(3)?;
                Ok(SearchResult {
                    id: row.get(0)?,
                    score: 1.0 - dist as f32,
                    file_path: row.get(2)?,
                    chunk_kind: parse_chunk_kind(&chunk_kind_str),
                    symbol_name: row.get(4)?,
                    symbol_kind: row.get(5)?,
                    summary: row.get(1)?,
                    start_line: row.get(6)?,
                    end_line: row.get(7)?,
                    language: row.get(8)?,
                    calls: json_to_vec(row.get::<_, Option<String>>(9)?),
                    called_by: json_to_vec(row.get::<_, Option<String>>(10)?),
                })
            })?;
            let mut results = rows
                .collect::<duckdb::Result<Vec<_>>>()
                .context("reading DuckDB search rows")?;
            if let Some(min) = opts.min_score {
                results.retain(|r| r.score >= min);
            }
            Ok(results)
        })
        .await
    }
}

// ── Conversion helpers ────────────────────────────────────────────────────

/// Render a vector as an injection-safe DuckDB ARRAY literal:
/// `[0.1, 0.2, ...]::FLOAT[N]`. Contains only numeric tokens.
fn vector_literal(v: &[f32]) -> String {
    let mut s = String::with_capacity(v.len() * 8 + 16);
    s.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&x.to_string());
    }
    s.push_str(&format!("]::FLOAT[{}]", v.len()));
    s
}

/// Parse a DuckDB `CAST(FLOAT[] AS VARCHAR)` rendering (`[0.1, 0.2, 0.3]`).
fn parse_vector(s: &str) -> Vec<f32> {
    s.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .filter_map(|tok| {
            let t = tok.trim();
            if t.is_empty() {
                None
            } else {
                t.parse::<f32>().ok()
            }
        })
        .collect()
}

fn parse_chunk_kind(s: &str) -> ChunkKind {
    match s {
        "symbol" => ChunkKind::Symbol,
        _ => ChunkKind::File,
    }
}

/// `None` → SQL NULL; `Some(vec)` → JSON array text. Preserves the
/// not-indexed (`None`) vs empty (`Some([])`) distinction.
fn vec_to_json(v: &Option<Vec<String>>) -> Option<String> {
    v.as_ref()
        .map(|items| serde_json::to_string(items).unwrap_or_else(|_| "[]".into()))
}

fn json_to_vec(s: Option<String>) -> Option<Vec<String>> {
    s.and_then(|s| serde_json::from_str(&s).ok())
}

// ── Tests ─────────────────────────────────────────────────────────────────
//
// These exercise a real (in-memory) DuckDB connection — FFI + tokio runtime,
// so they are ignored under Miri per the project's Miri rules.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DuckdbConfig, TurbopufferConfig};

    fn store_config(path: &str) -> StoreConfig {
        StoreConfig {
            provider: "duckdb".into(),
            turbopuffer: TurbopufferConfig {
                api_key: String::new(),
            },
            duckdb: DuckdbConfig { path: path.into() },
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

    async fn seeded_store() -> DuckdbStore {
        let store = DuckdbStore::open_in_memory(3).unwrap();
        let ns = Namespace::from("repo");
        store.create_namespace(&ns, 3).await.unwrap();
        store
    }

    // ── Conversion helpers (pure, Miri-safe) ──────────────────────────

    #[test]
    fn vector_literal_format() {
        assert_eq!(vector_literal(&[1.0, 2.5, -0.5]), "[1,2.5,-0.5]::FLOAT[3]");
    }

    #[test]
    fn parse_vector_round_trip() {
        assert_eq!(parse_vector("[1.0, 2.5, -0.5]"), vec![1.0, 2.5, -0.5]);
        assert_eq!(parse_vector("[]"), Vec::<f32>::new());
    }

    #[test]
    fn parse_chunk_kind_cases() {
        assert_eq!(parse_chunk_kind("symbol"), ChunkKind::Symbol);
        assert_eq!(parse_chunk_kind("file"), ChunkKind::File);
        assert_eq!(parse_chunk_kind("nonsense"), ChunkKind::File);
    }

    #[test]
    fn vec_json_round_trip_preserves_none_vs_empty() {
        assert_eq!(vec_to_json(&None), None);
        assert_eq!(vec_to_json(&Some(vec![])), Some("[]".to_string()));
        assert_eq!(
            vec_to_json(&Some(vec!["a".into(), "b".into()])),
            Some(r#"["a","b"]"#.to_string())
        );
        assert_eq!(json_to_vec(None), None);
        assert_eq!(json_to_vec(Some("[]".into())), Some(vec![]));
        assert_eq!(
            json_to_vec(Some(r#"["a","b"]"#.into())),
            Some(vec!["a".to_string(), "b".to_string()])
        );
    }

    // ── Provider ──────────────────────────────────────────────────────

    #[test]
    fn provider_name() {
        assert_eq!(DuckdbProvider.name(), "duckdb");
    }

    #[test]
    fn provider_validate_requires_path() {
        assert!(
            DuckdbProvider
                .validate(&store_config("/tmp/x.duckdb"))
                .is_ok()
        );
        let err = DuckdbProvider.validate(&store_config("  ")).unwrap_err();
        assert!(err.to_string().contains("store.duckdb.path"));
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn provider_build_succeeds() {
        assert!(DuckdbProvider.build(&store_config(":memory:"), 3).is_ok());
    }

    // ── Schema / dimension ────────────────────────────────────────────

    #[cfg_attr(miri, ignore)]
    #[test]
    fn open_in_memory_initializes_schema() {
        assert!(DuckdbStore::open_in_memory(8).is_ok());
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn reopen_with_mismatched_dimension_errors() {
        // A file-backed db pins its dimension; reopening with a different one fails.
        let dir = std::env::temp_dir().join(format!("wdpkr-duckdb-dim-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("dim.duckdb");
        let p = path.to_str().unwrap();

        assert!(DuckdbStore::open(p, 3).is_ok());
        let reopened = DuckdbStore::open(p, 5);
        match reopened {
            Ok(_) => panic!("reopening with a different dimension should error"),
            Err(e) => assert!(e.to_string().contains("dimension"), "{e}"),
        }

        std::fs::remove_dir_all(&dir).ok();
    }

    // ── Namespace lifecycle ───────────────────────────────────────────

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn namespace_lifecycle() {
        let store = DuckdbStore::open_in_memory(3).unwrap();
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
        // Re-upsert same id with new content.
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

    // ── list_documents round-trip ─────────────────────────────────────

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn list_documents_round_trips_all_fields() {
        let store = seeded_store().await;
        let ns = Namespace::from("repo");
        let sym = symbol_doc("s1", vec![0.1, 0.2, 0.3], "src/lib.rs", "process");
        store.upsert(&ns, std::slice::from_ref(&sym)).await.unwrap();

        let docs = store.list_documents(&ns).await.unwrap();
        assert_eq!(docs.len(), 1);
        let d = &docs[0];
        assert_eq!(d.id, "s1");
        assert_eq!(d.vector, vec![0.1, 0.2, 0.3]);
        assert_eq!(d.chunk_kind, ChunkKind::Symbol);
        assert_eq!(d.symbol_name.as_deref(), Some("process"));
        assert_eq!(d.start_line, Some(10));
        assert_eq!(d.end_line, Some(25));
        // None vs Some([]) distinction preserved through JSON storage.
        assert_eq!(d.calls, Some(vec!["other_fn".to_string()]));
        assert_eq!(d.called_by, Some(vec![]));
    }

    // ── Namespace isolation ───────────────────────────────────────────

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn namespaces_are_isolated() {
        let store = DuckdbStore::open_in_memory(3).unwrap();
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
