//! DuckDB vector store adapter (local embedded database).
//!
//! Implements [`VectorStore`] against DuckDB, storing data on the local
//! filesystem. Each wdpkr [`Namespace`] maps to a pair of DuckDB tables:
//! one for documents+vectors, one for metadata.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;

use super::{
    ChunkKind, Namespace, NamespaceMetadata, SearchOptions, SearchResult, StoreProvider,
    UpsertStats, VectorDocument, VectorStore,
};
use crate::config::StoreConfig;

// ── Provider ─────────────────────────────────────────────────────────────

pub struct DuckdbProvider;

impl StoreProvider for DuckdbProvider {
    fn name(&self) -> &str {
        "duckdb"
    }

    fn validate(&self, config: &StoreConfig) -> Result<()> {
        if config.duckdb.data_path.is_empty() {
            bail!(
                "store.duckdb.data_path (or WDPKR_STORE_PATH) is required when store.provider=duckdb"
            );
        }
        Ok(())
    }

    fn build(&self, config: &StoreConfig, dimension: usize) -> Result<Box<dyn VectorStore>> {
        let path = &config.duckdb.data_path;
        std::fs::create_dir_all(path)
            .with_context(|| format!("creating DuckDB data directory: {path}"))?;
        let db_path = PathBuf::from(path).join("wdpkr.duckdb");
        let conn = duckdb::Connection::open(&db_path)
            .with_context(|| format!("opening DuckDB at {}", db_path.display()))?;
        conn.execute_batch("INSTALL vss; LOAD vss;")
            .context("loading DuckDB VSS extension (is DuckDB installed with vector support?)")?;
        Ok(Box::new(DuckdbStore {
            conn: Mutex::new(conn),
            dimension,
        }))
    }
}

// ── Store ────────────────────────────────────────────────────────────────

pub struct DuckdbStore {
    conn: Mutex<duckdb::Connection>,
    dimension: usize,
}

fn doc_table(ns: &Namespace) -> String {
    format!("\"ns_{}\"", ns.as_str().replace('"', "\"\""))
}

fn meta_table(ns: &Namespace) -> String {
    format!("\"ns_{}_meta\"", ns.as_str().replace('"', "\"\""))
}

#[async_trait]
impl VectorStore for DuckdbStore {
    async fn create_namespace(&self, ns: &Namespace, dimension: usize) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let doc = doc_table(ns);
        let meta = meta_table(ns);
        conn.execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS {doc} (
                id VARCHAR PRIMARY KEY,
                vector FLOAT[{dimension}],
                summary VARCHAR NOT NULL,
                file_path VARCHAR NOT NULL,
                chunk_kind VARCHAR NOT NULL,
                symbol_name VARCHAR,
                symbol_kind VARCHAR,
                start_line UINTEGER,
                end_line UINTEGER,
                language VARCHAR,
                content_hash VARCHAR,
                calls VARCHAR[],
                called_by VARCHAR[]
            );
            CREATE TABLE IF NOT EXISTS {meta} (
                key VARCHAR PRIMARY KEY,
                value VARCHAR NOT NULL
            );"
        ))
        .context("creating DuckDB namespace tables")?;
        Ok(())
    }

    async fn delete_namespace(&self, ns: &Namespace) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let doc = doc_table(ns);
        let meta = meta_table(ns);
        conn.execute_batch(&format!(
            "DROP TABLE IF EXISTS {doc};
             DROP TABLE IF EXISTS {meta};"
        ))
        .context("deleting DuckDB namespace tables")?;
        Ok(())
    }

    async fn namespace_exists(&self, ns: &Namespace) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let doc = doc_table(ns);
        let table_name = doc.trim_matches('"');
        let mut stmt = conn
            .prepare("SELECT COUNT(*) FROM information_schema.tables WHERE table_name = ?")
            .context("checking namespace existence")?;
        let count: i64 = stmt
            .query_row([table_name], |row| row.get(0))
            .context("querying table existence")?;
        Ok(count > 0)
    }

    async fn get_metadata(&self, ns: &Namespace) -> Result<NamespaceMetadata> {
        let conn = self.conn.lock().unwrap();
        let meta = meta_table(ns);

        let table_name = meta.trim_matches('"');
        let exists: i64 = conn
            .prepare("SELECT COUNT(*) FROM information_schema.tables WHERE table_name = ?")
            .and_then(|mut s| s.query_row([table_name], |row| row.get(0)))
            .unwrap_or(0);
        if exists == 0 {
            return Ok(NamespaceMetadata::default());
        }

        let mut stmt = conn
            .prepare(&format!("SELECT key, value FROM {meta}"))
            .context("querying metadata")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .context("reading metadata rows")?;

        let mut result = NamespaceMetadata::default();
        for row in rows {
            let (k, v) = row.context("reading metadata row")?;
            match k.as_str() {
                "hwm_sha" => result.hwm_sha = Some(v),
                "embedder" => result.embedder = Some(v),
                _ => {
                    result.extra.insert(k, v);
                }
            }
        }
        Ok(result)
    }

    async fn set_metadata(&self, ns: &Namespace, meta_data: &NamespaceMetadata) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let meta = meta_table(ns);

        conn.execute_batch(&format!("DELETE FROM {meta}"))
            .context("clearing metadata")?;

        let mut stmt = conn
            .prepare(&format!("INSERT INTO {meta} (key, value) VALUES (?, ?)"))
            .context("preparing metadata insert")?;

        if let Some(ref sha) = meta_data.hwm_sha {
            stmt.execute(duckdb::params!["hwm_sha", sha])
                .context("inserting hwm_sha")?;
        }
        if let Some(ref embedder) = meta_data.embedder {
            stmt.execute(duckdb::params!["embedder", embedder])
                .context("inserting embedder")?;
        }
        for (k, v) in &meta_data.extra {
            stmt.execute(duckdb::params![k, v])
                .context("inserting extra metadata")?;
        }
        Ok(())
    }

    async fn upsert(&self, ns: &Namespace, docs: &[VectorDocument]) -> Result<UpsertStats> {
        if docs.is_empty() {
            return Ok(UpsertStats::default());
        }

        let conn = self.conn.lock().unwrap();
        let doc = doc_table(ns);

        let mut stmt = conn
            .prepare(&format!(
                "INSERT OR REPLACE INTO {doc}
                 (id, vector, summary, file_path, chunk_kind,
                  symbol_name, symbol_kind, start_line, end_line,
                  language, content_hash, calls, called_by)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
            ))
            .context("preparing upsert")?;

        for d in docs {
            let vector_str = format!(
                "[{}]",
                d.vector
                    .iter()
                    .map(|f| f.to_string())
                    .collect::<Vec<_>>()
                    .join(",")
            );
            let calls_val = d.calls.as_ref().map(|c| {
                format!(
                    "[{}]",
                    c.iter()
                        .map(|s| format!("'{}'", escape_sql(s)))
                        .collect::<Vec<_>>()
                        .join(",")
                )
            });
            let called_by_val = d.called_by.as_ref().map(|c| {
                format!(
                    "[{}]",
                    c.iter()
                        .map(|s| format!("'{}'", escape_sql(s)))
                        .collect::<Vec<_>>()
                        .join(",")
                )
            });

            stmt.execute(duckdb::params![
                d.id,
                vector_str,
                d.summary,
                d.file_path,
                d.chunk_kind.to_string(),
                d.symbol_name,
                d.symbol_kind,
                d.start_line,
                d.end_line,
                d.language,
                d.content_hash,
                calls_val,
                called_by_val,
            ])
            .with_context(|| format!("upserting doc {}", d.id))?;
        }

        Ok(UpsertStats {
            upserted: docs.len(),
            skipped: 0,
        })
    }

    async fn delete_by_ids(&self, ns: &Namespace, ids: &[&str]) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let conn = self.conn.lock().unwrap();
        let doc = doc_table(ns);
        let placeholders: Vec<String> = ids
            .iter()
            .map(|id| format!("'{}'", escape_sql(id)))
            .collect();
        conn.execute_batch(&format!(
            "DELETE FROM {doc} WHERE id IN ({})",
            placeholders.join(", ")
        ))
        .context("DuckDB delete by ids")?;
        Ok(())
    }

    async fn delete_by_file(&self, ns: &Namespace, file_path: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        let doc = doc_table(ns);
        conn.execute(
            &format!("DELETE FROM {doc} WHERE file_path = ?"),
            [file_path],
        )
        .context("DuckDB delete by file")?;
        Ok(())
    }

    async fn delete_by_glob(&self, ns: &Namespace, pattern: &str) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let doc = doc_table(ns);

        let glob = globset::GlobBuilder::new(pattern)
            .literal_separator(false)
            .build()
            .context("invalid glob pattern")?
            .compile_matcher();

        let mut stmt = conn
            .prepare(&format!("SELECT id, file_path FROM {doc}"))
            .context("querying for glob delete")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .context("reading rows for glob")?;

        let mut matching_ids = Vec::new();
        for row in rows {
            let (id, path) = row.context("reading glob row")?;
            if glob.is_match(&path) {
                matching_ids.push(id);
            }
        }

        let count = matching_ids.len();
        if !matching_ids.is_empty() {
            let escaped: Vec<String> = matching_ids
                .iter()
                .map(|id| format!("'{}'", escape_sql(id)))
                .collect();
            conn.execute_batch(&format!(
                "DELETE FROM {doc} WHERE id IN ({})",
                escaped.join(", ")
            ))
            .context("glob delete")?;
        }
        Ok(count)
    }

    async fn get_content_hashes(&self, ns: &Namespace) -> Result<HashMap<String, String>> {
        let conn = self.conn.lock().unwrap();
        let doc = doc_table(ns);

        let mut stmt = conn
            .prepare(&format!(
                "SELECT file_path, content_hash FROM {doc}
                 WHERE chunk_kind = 'file' AND content_hash IS NOT NULL"
            ))
            .context("querying content hashes")?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .context("reading content hashes")?;

        let mut hashes = HashMap::new();
        for row in rows {
            let (path, hash) = row.context("reading hash row")?;
            hashes.insert(path, hash);
        }
        Ok(hashes)
    }

    async fn list_documents(&self, ns: &Namespace) -> Result<Vec<VectorDocument>> {
        let conn = self.conn.lock().unwrap();
        let doc = doc_table(ns);

        let mut stmt = conn
            .prepare(&format!("SELECT * FROM {doc}"))
            .context("querying all documents")?;
        let rows = stmt
            .query_map([], row_to_document)
            .context("reading documents")?;

        let mut docs = Vec::new();
        for row in rows {
            docs.push(row.context("reading document row")?);
        }
        Ok(docs)
    }

    async fn search(
        &self,
        ns: &Namespace,
        query_vector: &[f32],
        opts: &SearchOptions,
    ) -> Result<Vec<SearchResult>> {
        let conn = self.conn.lock().unwrap();
        let doc = doc_table(ns);

        let vector_str = format!(
            "[{}]",
            query_vector
                .iter()
                .map(|f| f.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );

        let mut where_clauses = Vec::new();
        if let Some(filter) = build_filter(opts) {
            where_clauses.push(filter);
        }
        let where_sql = if where_clauses.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", where_clauses.join(" AND "))
        };

        let query = format!(
            "SELECT *, array_cosine_distance(vector, '{vector_str}'::FLOAT[{dim}]) as _distance
             FROM {doc}
             {where_sql}
             ORDER BY _distance
             LIMIT {limit}",
            dim = self.dimension,
            limit = opts.top_k,
        );

        let mut stmt = conn.prepare(&query).context("preparing search query")?;
        let rows = stmt
            .query_map([], |row| {
                let dist: f64 = row.get_unwrap(row.as_ref().column_count() - 1);
                let score = 1.0 - dist as f32;
                let doc = row_to_search_result(row, score)?;
                Ok((doc, score))
            })
            .context("executing search")?;

        let mut results = Vec::new();
        for row in rows {
            let (result, score) = row.context("reading search result")?;
            if let Some(min) = opts.min_score
                && score < min
            {
                continue;
            }
            results.push(result);
        }
        Ok(results)
    }
}

// ── Row extraction helpers ───────────────────────────────────────────────

fn row_to_document(row: &duckdb::Row<'_>) -> duckdb::Result<VectorDocument> {
    let chunk_kind_str: String = row.get(4)?;
    let vector_str: String = row.get(1)?;
    let vector = parse_vector(&vector_str);

    Ok(VectorDocument {
        id: row.get(0)?,
        vector,
        summary: row.get(2)?,
        file_path: row.get(3)?,
        chunk_kind: parse_chunk_kind(&chunk_kind_str),
        symbol_name: row.get(5)?,
        symbol_kind: row.get(6)?,
        start_line: row.get(7)?,
        end_line: row.get(8)?,
        language: row.get(9)?,
        content_hash: row.get(10)?,
        calls: row
            .get::<_, Option<String>>(11)?
            .map(|s| parse_string_list(&s)),
        called_by: row
            .get::<_, Option<String>>(12)?
            .map(|s| parse_string_list(&s)),
    })
}

fn row_to_search_result(row: &duckdb::Row<'_>, score: f32) -> duckdb::Result<SearchResult> {
    let chunk_kind_str: String = row.get(4)?;

    Ok(SearchResult {
        id: row.get(0)?,
        score,
        file_path: row.get(3)?,
        chunk_kind: parse_chunk_kind(&chunk_kind_str),
        symbol_name: row.get(5)?,
        symbol_kind: row.get(6)?,
        summary: row.get(2)?,
        start_line: row.get(7)?,
        end_line: row.get(8)?,
        language: row.get(9)?,
        calls: row
            .get::<_, Option<String>>(11)?
            .map(|s| parse_string_list(&s)),
        called_by: row
            .get::<_, Option<String>>(12)?
            .map(|s| parse_string_list(&s)),
    })
}

// ── Helpers ──────────────────────────────────────────────────────────────

fn build_filter(opts: &SearchOptions) -> Option<String> {
    let mut conditions = Vec::new();

    match opts.path_prefixes.len() {
        0 => {}
        1 => {
            conditions.push(format!(
                "file_path LIKE '{}%'",
                escape_sql(&opts.path_prefixes[0])
            ));
        }
        _ => {
            let clauses: Vec<String> = opts
                .path_prefixes
                .iter()
                .map(|p| format!("file_path LIKE '{}%'", escape_sql(p)))
                .collect();
            conditions.push(format!("({})", clauses.join(" OR ")));
        }
    }

    if let Some(ref kind) = opts.chunk_kind {
        conditions.push(format!("chunk_kind = '{kind}'"));
    }
    if let Some(ref lang) = opts.language {
        conditions.push(format!("language = '{}'", escape_sql(lang)));
    }

    if conditions.is_empty() {
        None
    } else {
        Some(conditions.join(" AND "))
    }
}

fn escape_sql(s: &str) -> String {
    s.replace('\'', "''")
}

fn parse_chunk_kind(s: &str) -> ChunkKind {
    match s {
        "symbol" => ChunkKind::Symbol,
        _ => ChunkKind::File,
    }
}

fn parse_vector(s: &str) -> Vec<f32> {
    let trimmed = s.trim_start_matches('[').trim_end_matches(']');
    if trimmed.is_empty() {
        return Vec::new();
    }
    trimmed
        .split(',')
        .filter_map(|v| v.trim().parse::<f32>().ok())
        .collect()
}

fn parse_string_list(s: &str) -> Vec<String> {
    let trimmed = s.trim_start_matches('[').trim_end_matches(']');
    if trimmed.is_empty() {
        return Vec::new();
    }
    trimmed
        .split(',')
        .map(|v| v.trim().trim_matches('\'').to_string())
        .collect()
}

// ── Tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Provider ─────────────────────────────────────────────────────

    #[test]
    fn provider_name() {
        assert_eq!(DuckdbProvider.name(), "duckdb");
    }

    #[test]
    fn validate_passes_with_data_path() {
        let config = StoreConfig {
            provider: "duckdb".into(),
            turbopuffer: crate::config::TurbopufferStoreConfig {
                api_key: String::new(),
            },
            duckdb: crate::config::DuckdbStoreConfig {
                data_path: "/tmp/test".into(),
            },
        };
        assert!(DuckdbProvider.validate(&config).is_ok());
    }

    #[test]
    fn validate_fails_empty_data_path() {
        let config = StoreConfig {
            provider: "duckdb".into(),
            turbopuffer: crate::config::TurbopufferStoreConfig {
                api_key: String::new(),
            },
            duckdb: crate::config::DuckdbStoreConfig {
                data_path: String::new(),
            },
        };
        let err = DuckdbProvider.validate(&config).unwrap_err();
        assert!(err.to_string().contains("data_path"));
    }

    // ── Filter construction ──────────────────────────────────────────

    #[test]
    fn no_opts_no_filter() {
        let opts = SearchOptions::default();
        assert!(build_filter(&opts).is_none());
    }

    #[test]
    fn single_prefix_filter() {
        let opts = SearchOptions {
            path_prefixes: vec!["src/".into()],
            ..Default::default()
        };
        assert_eq!(build_filter(&opts).unwrap(), "file_path LIKE 'src/%'");
    }

    #[test]
    fn multi_prefix_filter() {
        let opts = SearchOptions {
            path_prefixes: vec!["src/".into(), "lib/".into()],
            ..Default::default()
        };
        assert_eq!(
            build_filter(&opts).unwrap(),
            "(file_path LIKE 'src/%' OR file_path LIKE 'lib/%')"
        );
    }

    #[test]
    fn chunk_kind_filter() {
        let opts = SearchOptions {
            chunk_kind: Some(ChunkKind::File),
            ..Default::default()
        };
        assert_eq!(build_filter(&opts).unwrap(), "chunk_kind = 'file'");
    }

    #[test]
    fn combined_filter() {
        let opts = SearchOptions {
            path_prefixes: vec!["src/".into()],
            chunk_kind: Some(ChunkKind::Symbol),
            language: Some("rust".into()),
            ..Default::default()
        };
        assert_eq!(
            build_filter(&opts).unwrap(),
            "file_path LIKE 'src/%' AND chunk_kind = 'symbol' AND language = 'rust'"
        );
    }

    #[test]
    fn escape_sql_quotes() {
        assert_eq!(escape_sql("it's"), "it''s");
        assert_eq!(escape_sql("normal"), "normal");
    }

    // ── Parse helpers ────────────────────────────────────────────────

    #[test]
    fn parse_vector_basic() {
        let v = parse_vector("[0.1, 0.2, 0.3]");
        assert_eq!(v, vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn parse_vector_empty() {
        let v = parse_vector("[]");
        assert!(v.is_empty());
    }

    #[test]
    fn parse_string_list_basic() {
        let v = parse_string_list("['foo', 'bar']");
        assert_eq!(v, vec!["foo", "bar"]);
    }

    #[test]
    fn parse_string_list_empty() {
        let v = parse_string_list("[]");
        assert!(v.is_empty());
    }

    #[test]
    fn table_names_escape_quotes() {
        let ns = Namespace::from("my-repo");
        assert_eq!(doc_table(&ns), "\"ns_my-repo\"");
        assert_eq!(meta_table(&ns), "\"ns_my-repo_meta\"");
    }
}
