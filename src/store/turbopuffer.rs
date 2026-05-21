//! Turbopuffer vector store adapter (v2 API).
//!
//! Implements [`VectorStore`] against Turbopuffer's v2 HTTP API. Metadata
//! (HWM SHA, embedder identity) is stored as a reserved vector with ID
//! `__wdpkr_meta__`.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{
    ChunkKind, Namespace, NamespaceMetadata, SearchOptions, SearchResult, UpsertStats,
    VectorDocument, VectorStore,
};
use crate::config::StoreConfig;

const META_VECTOR_ID: &str = "__wdpkr_meta__";
const MAX_RETRIES: usize = 3;
const UPSERT_BATCH_SIZE: usize = 200;

pub struct TurbopufferStore {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    dimension: usize,
}

impl TurbopufferStore {
    pub fn new(config: &StoreConfig, dimension: usize) -> Result<Self> {
        if config.api_key.is_empty() {
            bail!("TURBOPUFFER_API_KEY is required");
        }
        Ok(Self {
            client: reqwest::Client::new(),
            api_key: config.api_key.clone(),
            base_url: "https://api.turbopuffer.com".into(),
            dimension,
        })
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    fn ns_url(&self, ns: &Namespace) -> String {
        format!("{}/v2/namespaces/{}", self.base_url, ns.as_str())
    }

    async fn post_json<T: Serialize>(&self, url: &str, body: &T) -> Result<reqwest::Response> {
        for attempt in 0..=MAX_RETRIES {
            let resp = self
                .client
                .post(url)
                .bearer_auth(&self.api_key)
                .json(body)
                .send()
                .await;

            let resp = match resp {
                Ok(r) => r,
                Err(_) if attempt < MAX_RETRIES => {
                    tokio::time::sleep(backoff(attempt)).await;
                    continue;
                }
                Err(e) => return Err(e).context("Turbopuffer API request failed"),
            };

            let status = resp.status();
            if status.is_success() || status.is_client_error() {
                return Ok(resp);
            }

            if attempt < MAX_RETRIES {
                tokio::time::sleep(backoff(attempt)).await;
                continue;
            }

            let body = resp.text().await.unwrap_or_default();
            bail!("Turbopuffer API error ({status}): {body}");
        }
        bail!("Turbopuffer API: max retries exceeded")
    }
}

#[async_trait]
impl VectorStore for TurbopufferStore {
    async fn create_namespace(&self, ns: &Namespace, dimension: usize) -> Result<()> {
        let meta = NamespaceMetadata::default();
        let row = metadata_to_row(&meta, dimension);
        let body = WriteRequest {
            upsert_rows: Some(vec![row]),
            distance_metric: Some("cosine_distance".into()),
            ..Default::default()
        };
        let resp = self.post_json(&self.ns_url(ns), &body).await?;
        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            bail!("failed to create namespace '{}': {err}", ns.as_str());
        }
        Ok(())
    }

    async fn delete_namespace(&self, ns: &Namespace) -> Result<()> {
        for attempt in 0..=MAX_RETRIES {
            let resp = self
                .client
                .delete(self.ns_url(ns))
                .bearer_auth(&self.api_key)
                .send()
                .await;

            match resp {
                Ok(r) if r.status().is_success() || r.status().as_u16() == 404 => return Ok(()),
                Ok(r) if r.status().is_server_error() && attempt < MAX_RETRIES => {
                    tokio::time::sleep(backoff(attempt)).await;
                    continue;
                }
                Ok(r) => {
                    let body = r.text().await.unwrap_or_default();
                    bail!("failed to delete namespace '{}': {body}", ns.as_str());
                }
                Err(_) if attempt < MAX_RETRIES => {
                    tokio::time::sleep(backoff(attempt)).await;
                    continue;
                }
                Err(e) => return Err(e).context("delete namespace request failed"),
            }
        }
        bail!("delete namespace: max retries exceeded")
    }

    async fn namespace_exists(&self, ns: &Namespace) -> Result<bool> {
        let url = format!("{}/query", self.ns_url(ns));
        // Aggregate queries reject `limit`; only `aggregate_by`/`filters`/`group_by`/`top_k` are valid.
        let body = QueryRequest {
            aggregate_by: Some(serde_json::json!({"n": ["Count"]})),
            ..Default::default()
        };
        let resp = self
            .client
            .post(url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .context("namespace_exists probe failed")?;

        match resp.status().as_u16() {
            200 => Ok(true),
            404 => Ok(false),
            s => {
                let body = resp.text().await.unwrap_or_default();
                bail!("namespace_exists unexpected status {s}: {body}")
            }
        }
    }

    async fn get_metadata(&self, ns: &Namespace) -> Result<NamespaceMetadata> {
        let url = format!("{}/query", self.ns_url(ns));
        let zero_vec: Vec<f32> = vec![0.0; self.dimension];
        let body = QueryRequest {
            rank_by: Some(serde_json::json!(["vector", "ANN", zero_vec])),
            limit: Some(1),
            filters: Some(serde_json::json!(["id", "Eq", META_VECTOR_ID])),
            include_attributes: Some(serde_json::json!(true)),
            ..Default::default()
        };

        let resp = self.post_json(&url, &body).await?;
        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            bail!("get_metadata failed: {err}");
        }

        let query_resp: QueryResponse = resp.json().await.context("parsing metadata response")?;
        match query_resp.rows.first() {
            Some(row) => row_to_metadata(row),
            None => Ok(NamespaceMetadata::default()),
        }
    }

    async fn set_metadata(&self, ns: &Namespace, meta: &NamespaceMetadata) -> Result<()> {
        let row = metadata_to_row(meta, self.dimension);
        let body = WriteRequest {
            upsert_rows: Some(vec![row]),
            distance_metric: Some("cosine_distance".into()),
            ..Default::default()
        };
        let resp = self.post_json(&self.ns_url(ns), &body).await?;
        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            bail!("set_metadata failed: {err}");
        }
        Ok(())
    }

    async fn upsert(&self, ns: &Namespace, docs: &[VectorDocument]) -> Result<UpsertStats> {
        let mut upserted = 0;
        for chunk in docs.chunks(UPSERT_BATCH_SIZE) {
            let rows: Vec<HashMap<String, serde_json::Value>> =
                chunk.iter().map(doc_to_row).collect();
            let body = WriteRequest {
                upsert_rows: Some(rows),
                distance_metric: Some("cosine_distance".into()),
                ..Default::default()
            };
            let resp = self.post_json(&self.ns_url(ns), &body).await?;
            if !resp.status().is_success() {
                let err = resp.text().await.unwrap_or_default();
                bail!("upsert failed: {err}");
            }
            upserted += chunk.len();
        }
        Ok(UpsertStats {
            upserted,
            skipped: 0,
        })
    }

    async fn delete_by_ids(&self, ns: &Namespace, ids: &[&str]) -> Result<()> {
        let body = WriteRequest {
            deletes: Some(ids.iter().map(|s| serde_json::json!(s)).collect()),
            ..Default::default()
        };
        let resp = self.post_json(&self.ns_url(ns), &body).await?;
        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            bail!("delete_by_ids failed: {err}");
        }
        Ok(())
    }

    async fn delete_by_file(&self, ns: &Namespace, file_path: &str) -> Result<()> {
        let body = WriteRequest {
            delete_by_filter: Some(serde_json::json!(["file_path", "Eq", file_path])),
            ..Default::default()
        };
        let resp = self.post_json(&self.ns_url(ns), &body).await?;
        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            bail!("delete_by_file failed: {err}");
        }
        Ok(())
    }

    async fn delete_by_glob(&self, ns: &Namespace, pattern: &str) -> Result<usize> {
        let url = format!("{}/query", self.ns_url(ns));
        let count_body = QueryRequest {
            filters: Some(serde_json::json!(["file_path", "Glob", pattern])),
            aggregate_by: Some(serde_json::json!({"n": ["Count"]})),
            ..Default::default()
        };
        let count_resp = self.post_json(&url, &count_body).await?;
        let count: usize = if count_resp.status().is_success() {
            let json: serde_json::Value = count_resp.json().await.unwrap_or_default();
            json["aggregations"]["n"].as_u64().unwrap_or(0) as usize
        } else {
            0
        };

        let body = WriteRequest {
            delete_by_filter: Some(serde_json::json!(["file_path", "Glob", pattern])),
            ..Default::default()
        };
        let resp = self.post_json(&self.ns_url(ns), &body).await?;
        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            bail!("delete_by_glob failed: {err}");
        }
        Ok(count)
    }

    async fn get_content_hashes(&self, ns: &Namespace) -> Result<HashMap<String, String>> {
        let url = format!("{}/query", self.ns_url(ns));

        let body = QueryRequest {
            filters: Some(serde_json::json!([
                "And",
                [
                    ["chunk_kind", "Eq", "file"],
                    ["id", "NotEq", META_VECTOR_ID]
                ]
            ])),
            include_attributes: Some(serde_json::json!(["file_path", "content_hash"])),
            limit: Some(10_000),
            ..Default::default()
        };

        let resp = self.post_json(&url, &body).await?;
        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            bail!("get_content_hashes failed: {err}");
        }

        let query_resp: QueryResponse =
            resp.json().await.context("parsing content hash response")?;
        let mut hashes = HashMap::new();
        for row in query_resp.rows {
            if let (Some(path), Some(hash)) = (
                row.get("file_path").and_then(|v| v.as_str()),
                row.get("content_hash").and_then(|v| v.as_str()),
            ) {
                hashes.insert(path.to_string(), hash.to_string());
            }
        }
        Ok(hashes)
    }

    async fn list_documents(&self, ns: &Namespace) -> Result<Vec<VectorDocument>> {
        let url = format!("{}/query", self.ns_url(ns));
        let mut all_docs = Vec::new();
        let mut cursor: Option<String> = None;

        loop {
            let body = QueryRequest {
                filters: Some(serde_json::json!(["id", "NotEq", META_VECTOR_ID])),
                include_attributes: Some(serde_json::json!(true)),
                limit: Some(10_000),
                cursor: cursor.clone(),
                ..Default::default()
            };

            let resp = self.post_json(&url, &body).await?;
            if !resp.status().is_success() {
                let err = resp.text().await.unwrap_or_default();
                bail!("list_documents failed: {err}");
            }

            let query_resp: QueryResponse = resp.json().await.context("parsing list response")?;

            for row in query_resp.rows {
                all_docs.push(row_to_document(row)?);
            }

            match query_resp.next_cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }

        Ok(all_docs)
    }

    async fn search(
        &self,
        ns: &Namespace,
        query_vector: &[f32],
        opts: &SearchOptions,
    ) -> Result<Vec<SearchResult>> {
        let url = format!("{}/query", self.ns_url(ns));
        let filters = build_search_filters(opts);

        let body = QueryRequest {
            rank_by: Some(serde_json::json!(["vector", "ANN", query_vector])),
            limit: Some(opts.top_k),
            filters,
            include_attributes: Some(serde_json::json!(true)),
            ..Default::default()
        };

        let resp = self.post_json(&url, &body).await?;
        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            bail!("search failed: {err}");
        }

        let query_resp: QueryResponse = resp.json().await.context("parsing search response")?;
        let results = score_and_filter_rows(query_resp.rows, opts.min_score)?;

        Ok(results)
    }
}

// ── Search logic (extracted for testability) ─────────────────────────────

fn build_search_filters(opts: &SearchOptions) -> Option<serde_json::Value> {
    let mut conditions: Vec<serde_json::Value> = Vec::new();
    conditions.push(serde_json::json!(["id", "NotEq", META_VECTOR_ID]));

    match opts.path_prefixes.len() {
        0 => {}
        1 => {
            conditions.push(serde_json::json!([
                "file_path",
                "Glob",
                format!("{}*", opts.path_prefixes[0])
            ]));
        }
        _ => {
            let prefix_conditions: Vec<_> = opts
                .path_prefixes
                .iter()
                .map(|p| serde_json::json!(["file_path", "Glob", format!("{p}*")]))
                .collect();
            conditions.push(serde_json::json!(["Or", prefix_conditions]));
        }
    }
    if let Some(ref kind) = opts.chunk_kind {
        conditions.push(serde_json::json!(["chunk_kind", "Eq", kind.to_string()]));
    }
    if let Some(ref lang) = opts.language {
        conditions.push(serde_json::json!(["language", "Eq", lang]));
    }

    if conditions.len() == 1 {
        conditions.into_iter().next()
    } else {
        Some(serde_json::json!(["And", conditions]))
    }
}

fn dist_to_score(dist: f32) -> f32 {
    1.0 - dist
}

fn score_and_filter_rows(
    rows: Vec<HashMap<String, serde_json::Value>>,
    min_score: Option<f32>,
) -> Result<Vec<SearchResult>> {
    rows.into_iter()
        .map(|row| {
            let dist = row.get("$dist").and_then(|v| v.as_f64()).unwrap_or(2.0) as f32;
            let score = dist_to_score(dist);
            (row, score)
        })
        .filter(|(_, score)| {
            if let Some(min) = min_score {
                *score >= min
            } else {
                true
            }
        })
        .map(|(row, score)| row_to_search_result(row, score))
        .collect()
}

// ── Conversion helpers ────────────────────────────────────────────────────

fn doc_to_row(doc: &VectorDocument) -> HashMap<String, serde_json::Value> {
    let mut row = HashMap::new();
    row.insert("id".into(), serde_json::json!(doc.id));
    row.insert("vector".into(), serde_json::json!(doc.vector));
    row.insert("file_path".into(), serde_json::json!(doc.file_path));
    row.insert(
        "chunk_kind".into(),
        serde_json::json!(doc.chunk_kind.to_string()),
    );
    row.insert("summary".into(), serde_json::json!(doc.summary));
    if let Some(ref name) = doc.symbol_name {
        row.insert("symbol_name".into(), serde_json::json!(name));
    }
    if let Some(ref kind) = doc.symbol_kind {
        row.insert("symbol_kind".into(), serde_json::json!(kind));
    }
    if let Some(line) = doc.start_line {
        row.insert("start_line".into(), serde_json::json!(line));
    }
    if let Some(line) = doc.end_line {
        row.insert("end_line".into(), serde_json::json!(line));
    }
    if let Some(ref lang) = doc.language {
        row.insert("language".into(), serde_json::json!(lang));
    }
    if let Some(ref hash) = doc.content_hash {
        row.insert("content_hash".into(), serde_json::json!(hash));
    }
    if let Some(ref calls) = doc.calls {
        row.insert("calls".into(), serde_json::json!(calls));
    }
    if let Some(ref called_by) = doc.called_by {
        row.insert("called_by".into(), serde_json::json!(called_by));
    }
    row
}

fn metadata_to_row(
    meta: &NamespaceMetadata,
    dimension: usize,
) -> HashMap<String, serde_json::Value> {
    let mut row = HashMap::new();
    row.insert("id".into(), serde_json::json!(META_VECTOR_ID));
    row.insert("vector".into(), serde_json::json!(vec![0.0f32; dimension]));
    row.insert("__is_meta__".into(), serde_json::json!(true));
    if let Some(ref sha) = meta.hwm_sha {
        row.insert("hwm_sha".into(), serde_json::json!(sha));
    }
    if let Some(ref embedder) = meta.embedder {
        row.insert("embedder".into(), serde_json::json!(embedder));
    }
    for (k, v) in &meta.extra {
        row.insert(k.clone(), serde_json::json!(v));
    }
    row
}

fn row_to_metadata(row: &HashMap<String, serde_json::Value>) -> Result<NamespaceMetadata> {
    Ok(NamespaceMetadata {
        hwm_sha: row
            .get("hwm_sha")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        embedder: row
            .get("embedder")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        extra: row
            .iter()
            .filter(|(k, _)| {
                !matches!(
                    k.as_str(),
                    "id" | "vector" | "$dist" | "hwm_sha" | "embedder" | "__is_meta__"
                )
            })
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect(),
    })
}

fn row_to_document(row: HashMap<String, serde_json::Value>) -> Result<VectorDocument> {
    let get_str = |key: &str| -> Option<String> {
        row.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
    };
    let get_u32 =
        |key: &str| -> Option<u32> { row.get(key).and_then(|v| v.as_u64()).map(|n| n as u32) };
    let get_string_vec = |key: &str| -> Option<Vec<String>> {
        row.get(key).and_then(|v| {
            v.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|item| item.as_str().map(|s| s.to_string()))
                    .collect()
            })
        })
    };

    let vector: Vec<f32> = row
        .get("vector")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_f64().map(|f| f as f32))
                .collect()
        })
        .unwrap_or_default();

    let chunk_kind_str = get_str("chunk_kind").unwrap_or_else(|| "file".into());
    let chunk_kind = match chunk_kind_str.as_str() {
        "symbol" => ChunkKind::Symbol,
        _ => ChunkKind::File,
    };

    Ok(VectorDocument {
        id: get_str("id").unwrap_or_default(),
        vector,
        summary: get_str("summary").unwrap_or_default(),
        file_path: get_str("file_path").unwrap_or_default(),
        chunk_kind,
        symbol_name: get_str("symbol_name"),
        symbol_kind: get_str("symbol_kind"),
        start_line: get_u32("start_line"),
        end_line: get_u32("end_line"),
        language: get_str("language"),
        content_hash: get_str("content_hash"),
        calls: get_string_vec("calls"),
        called_by: get_string_vec("called_by"),
    })
}

fn row_to_search_result(
    row: HashMap<String, serde_json::Value>,
    score: f32,
) -> Result<SearchResult> {
    let get_str = |key: &str| -> Option<String> {
        row.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
    };
    let get_u32 =
        |key: &str| -> Option<u32> { row.get(key).and_then(|v| v.as_u64()).map(|n| n as u32) };
    let get_string_vec = |key: &str| -> Option<Vec<String>> {
        row.get(key).and_then(|v| {
            v.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|item| item.as_str().map(|s| s.to_string()))
                    .collect()
            })
        })
    };

    let chunk_kind_str = get_str("chunk_kind").unwrap_or_else(|| "file".into());
    let chunk_kind = match chunk_kind_str.as_str() {
        "symbol" => ChunkKind::Symbol,
        _ => ChunkKind::File,
    };

    Ok(SearchResult {
        id: get_str("id").unwrap_or_default(),
        score,
        file_path: get_str("file_path").unwrap_or_default(),
        chunk_kind,
        symbol_name: get_str("symbol_name"),
        symbol_kind: get_str("symbol_kind"),
        summary: get_str("summary").unwrap_or_default(),
        start_line: get_u32("start_line"),
        end_line: get_u32("end_line"),
        language: get_str("language"),
        calls: get_string_vec("calls"),
        called_by: get_string_vec("called_by"),
    })
}

// ── HTTP types (v2 API) ──────────────────────────────────────────────────

#[derive(Serialize, Default)]
struct WriteRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    upsert_rows: Option<Vec<HashMap<String, serde_json::Value>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deletes: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delete_by_filter: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    distance_metric: Option<String>,
}

#[derive(Serialize, Default)]
struct QueryRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    rank_by: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    filters: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    include_attributes: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    aggregate_by: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cursor: Option<String>,
}

#[derive(Deserialize)]
struct QueryResponse {
    #[serde(default)]
    rows: Vec<HashMap<String, serde_json::Value>>,
    next_cursor: Option<String>,
}

fn backoff(attempt: usize) -> Duration {
    Duration::from_millis(1000 * 2u64.pow(attempt as u32))
}

// ── build_store factory ───────────────────────────────────────────────────

pub fn build_store(config: &StoreConfig, dimension: usize) -> Result<Box<dyn VectorStore>> {
    match config.provider.as_str() {
        "turbopuffer" => Ok(Box::new(TurbopufferStore::new(config, dimension)?)),
        other => bail!("store provider '{other}' is not yet implemented"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Constructor ───────────────────────────────────────────────────

    #[test]
    fn requires_api_key() {
        let config = StoreConfig {
            provider: "turbopuffer".into(),
            api_key: String::new(),
        };
        assert!(TurbopufferStore::new(&config, 1024).is_err());
    }

    #[test]
    fn constructs_with_key() {
        let config = StoreConfig {
            provider: "turbopuffer".into(),
            api_key: "test-key".into(),
        };
        let store = TurbopufferStore::new(&config, 1024).unwrap();
        assert_eq!(store.base_url, "https://api.turbopuffer.com");
        assert_eq!(store.dimension, 1024);
    }

    #[test]
    fn custom_base_url() {
        let config = StoreConfig {
            provider: "turbopuffer".into(),
            api_key: "key".into(),
        };
        let store = TurbopufferStore::new(&config, 512)
            .unwrap()
            .with_base_url("http://localhost:9090");
        assert_eq!(store.base_url, "http://localhost:9090");
    }

    // ── URL construction ──────────────────────────────────────────────

    #[test]
    fn namespace_url() {
        let config = StoreConfig {
            provider: "turbopuffer".into(),
            api_key: "key".into(),
        };
        let store = TurbopufferStore::new(&config, 1024).unwrap();
        let ns = Namespace::from("my-repo");
        assert_eq!(
            store.ns_url(&ns),
            "https://api.turbopuffer.com/v2/namespaces/my-repo"
        );
    }

    // ── Write request serialization ──────────────────────────────────

    #[test]
    fn upsert_row_serialization() {
        let doc = VectorDocument {
            id: "test-1".into(),
            vector: vec![0.1, 0.2],
            summary: "A test doc".into(),
            file_path: "src/main.rs".into(),
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
        let row = doc_to_row(&doc);
        assert_eq!(row["id"], "test-1");
        assert_eq!(row["vector"].as_array().unwrap().len(), 2);
        assert_eq!(row["file_path"], "src/main.rs");
        assert_eq!(row["chunk_kind"], "file");
        assert_eq!(row["summary"], "A test doc");
        assert_eq!(row["language"], "rust");
        assert!(!row.contains_key("symbol_name"));
    }

    #[test]
    fn upsert_row_with_symbol_fields() {
        let doc = VectorDocument {
            id: "sym-1".into(),
            vector: vec![0.3],
            summary: "A function".into(),
            file_path: "src/lib.rs".into(),
            chunk_kind: ChunkKind::Symbol,
            symbol_name: Some("process".into()),
            symbol_kind: Some("function".into()),
            start_line: Some(10),
            end_line: Some(25),
            language: Some("rust".into()),
            content_hash: None,
            calls: None,
            called_by: None,
        };
        let row = doc_to_row(&doc);
        assert_eq!(row["symbol_name"], "process");
        assert_eq!(row["symbol_kind"], "function");
        assert_eq!(row["start_line"], 10);
        assert_eq!(row["end_line"], 25);
    }

    #[test]
    fn write_request_upsert_format() {
        let doc = VectorDocument {
            id: "test-1".into(),
            vector: vec![0.1, 0.2],
            summary: "A doc".into(),
            file_path: "src/main.rs".into(),
            chunk_kind: ChunkKind::File,
            symbol_name: None,
            symbol_kind: None,
            start_line: None,
            end_line: None,
            language: None,
            content_hash: None,
            calls: None,
            called_by: None,
        };
        let body = WriteRequest {
            upsert_rows: Some(vec![doc_to_row(&doc)]),
            distance_metric: Some("cosine_distance".into()),
            ..Default::default()
        };
        let json = serde_json::to_value(&body).unwrap();
        assert!(json["upsert_rows"].is_array());
        assert_eq!(json["upsert_rows"][0]["id"], "test-1");
        assert_eq!(json["distance_metric"], "cosine_distance");
        assert!(json.get("deletes").is_none());
        assert!(json.get("delete_by_filter").is_none());
    }

    #[test]
    fn write_request_delete_by_ids() {
        let body = WriteRequest {
            deletes: Some(vec![serde_json::json!("a"), serde_json::json!("b")]),
            ..Default::default()
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["deletes"].as_array().unwrap().len(), 2);
        assert!(json.get("upsert_rows").is_none());
    }

    #[test]
    fn write_request_delete_by_filter() {
        let body = WriteRequest {
            delete_by_filter: Some(serde_json::json!(["file_path", "Eq", "src/main.rs"])),
            ..Default::default()
        };
        let json = serde_json::to_value(&body).unwrap();
        assert!(json["delete_by_filter"].is_array());
        assert_eq!(json["delete_by_filter"][0], "file_path");
        assert_eq!(json["delete_by_filter"][1], "Eq");
    }

    // ── Metadata round-trip ───────────────────────────────────────────

    #[test]
    fn metadata_to_row_and_back() {
        let meta = NamespaceMetadata {
            hwm_sha: Some("abc123".into()),
            embedder: Some("voyage/voyage-code-3".into()),
            extra: HashMap::new(),
        };
        let row = metadata_to_row(&meta, 3);
        assert_eq!(row["id"], META_VECTOR_ID);
        assert_eq!(row["vector"], serde_json::json!([0.0, 0.0, 0.0]));
        assert_eq!(row["hwm_sha"], "abc123");
        assert_eq!(row["embedder"], "voyage/voyage-code-3");
        assert_eq!(row["__is_meta__"], true);

        let back = row_to_metadata(&row).unwrap();
        assert_eq!(back.hwm_sha.as_deref(), Some("abc123"));
        assert_eq!(back.embedder.as_deref(), Some("voyage/voyage-code-3"));
    }

    #[test]
    fn metadata_with_empty_row_returns_defaults() {
        let row = HashMap::new();
        let meta = row_to_metadata(&row).unwrap();
        assert!(meta.hwm_sha.is_none());
        assert!(meta.embedder.is_none());
    }

    // ── Query request serialization ──────────────────────────────────

    #[test]
    fn query_request_v2_format() {
        let body = QueryRequest {
            rank_by: Some(serde_json::json!(["vector", "ANN", [0.1, 0.2]])),
            limit: Some(5),
            filters: Some(serde_json::json!([
                "And",
                [
                    ["id", "NotEq", META_VECTOR_ID],
                    ["file_path", "Glob", "src/*"]
                ]
            ])),
            include_attributes: Some(serde_json::json!(true)),
            ..Default::default()
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["rank_by"][0], "vector");
        assert_eq!(json["rank_by"][1], "ANN");
        assert_eq!(json["limit"], 5);
        assert_eq!(json["filters"][0], "And");
        assert_eq!(json["include_attributes"], true);
        assert!(json.get("aggregate_by").is_none());
    }

    #[test]
    fn query_request_aggregate_format() {
        // Aggregate queries must not include `limit` — the v2 API rejects it
        // with: unknown field `limit`, expected one of `aggregate_by`,
        // `filters`, `group_by`, `top_k`.
        let body = QueryRequest {
            aggregate_by: Some(serde_json::json!({"n": ["Count"]})),
            ..Default::default()
        };
        let json = serde_json::to_value(&body).unwrap();
        assert!(json["aggregate_by"]["n"].is_array());
        assert!(json.get("rank_by").is_none());
        assert!(json.get("limit").is_none());
    }

    #[test]
    fn query_request_single_filter_no_and_wrapper() {
        let body = QueryRequest {
            rank_by: Some(serde_json::json!(["vector", "ANN", [0.1]])),
            limit: Some(10),
            filters: Some(serde_json::json!(["id", "NotEq", META_VECTOR_ID])),
            include_attributes: Some(serde_json::json!(true)),
            ..Default::default()
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["filters"][0], "id");
        assert_eq!(json["filters"][1], "NotEq");
    }

    // ── Query response parsing ────────────────────────────────────────

    #[test]
    fn parse_v2_query_response() {
        let json = r#"{
            "rows": [
                {
                    "$dist": 0.13,
                    "id": "doc-1",
                    "file_path": "src/main.rs",
                    "chunk_kind": "file",
                    "summary": "Entry point",
                    "language": "rust"
                }
            ]
        }"#;
        let resp: QueryResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.rows.len(), 1);

        let row = resp.rows.into_iter().next().unwrap();
        let dist = row.get("$dist").unwrap().as_f64().unwrap() as f32;
        let score = 1.0 - dist;
        let sr = row_to_search_result(row, score).unwrap();
        assert_eq!(sr.id, "doc-1");
        assert!((sr.score - 0.87).abs() < 0.01);
        assert_eq!(sr.file_path, "src/main.rs");
        assert_eq!(sr.chunk_kind, ChunkKind::File);
        assert_eq!(sr.language.as_deref(), Some("rust"));
    }

    #[test]
    fn parse_symbol_query_result() {
        let json = r#"{
            "$dist": 0.09,
            "id": "sym-1",
            "file_path": "src/lib.rs",
            "chunk_kind": "symbol",
            "summary": "Processes things",
            "symbol_name": "process",
            "symbol_kind": "function",
            "start_line": 10,
            "end_line": 25
        }"#;
        let row: HashMap<String, serde_json::Value> = serde_json::from_str(json).unwrap();
        let dist = row.get("$dist").unwrap().as_f64().unwrap() as f32;
        let sr = row_to_search_result(row, 1.0 - dist).unwrap();
        assert_eq!(sr.chunk_kind, ChunkKind::Symbol);
        assert_eq!(sr.symbol_name.as_deref(), Some("process"));
        assert_eq!(sr.start_line, Some(10));
        assert!((sr.score - 0.91).abs() < 0.01);
    }

    #[test]
    fn cosine_distance_to_similarity_conversion() {
        // dist=0 → score=1.0 (perfect match)
        assert!((1.0f32 - 0.0 - 1.0).abs() < f32::EPSILON);
        // dist=1 → score=0.0 (orthogonal)
        assert!((1.0f32 - 1.0 - 0.0).abs() < f32::EPSILON);
        // dist=2 → score=-1.0 (opposite)
        assert!((1.0f32 - 2.0 - (-1.0)).abs() < f32::EPSILON);
    }

    // ── Factory ───────────────────────────────────────────────────────

    #[test]
    fn factory_turbopuffer_with_key() {
        let config = StoreConfig {
            provider: "turbopuffer".into(),
            api_key: "key".into(),
        };
        assert!(build_store(&config, 1024).is_ok());
    }

    #[test]
    fn factory_turbopuffer_without_key_errors() {
        let config = StoreConfig {
            provider: "turbopuffer".into(),
            api_key: String::new(),
        };
        assert!(build_store(&config, 1024).is_err());
    }

    #[test]
    fn factory_unknown_provider_errors() {
        let config = StoreConfig {
            provider: "qdrant".into(),
            api_key: "key".into(),
        };
        assert!(build_store(&config, 1024).is_err());
    }

    // ── Search filter construction ───────────────────────────────────

    #[test]
    fn filters_always_exclude_meta_vector() {
        let opts = SearchOptions::default();
        let filters = build_search_filters(&opts).unwrap();
        assert_eq!(filters[0], "id");
        assert_eq!(filters[1], "NotEq");
        assert_eq!(filters[2], META_VECTOR_ID);
    }

    #[test]
    fn filters_no_options_produces_single_filter() {
        let opts = SearchOptions::default();
        let filters = build_search_filters(&opts).unwrap();
        // Single filter = bare array, not wrapped in And
        assert_eq!(filters[0], "id");
        assert!(filters.get(0).unwrap().is_string());
    }

    #[test]
    fn filters_single_prefix_uses_glob() {
        let opts = SearchOptions {
            path_prefixes: vec!["src/finance/".into()],
            ..Default::default()
        };
        let filters = build_search_filters(&opts).unwrap();
        assert_eq!(filters[0], "And");
        let inner = filters[1].as_array().unwrap();
        assert_eq!(inner.len(), 2);
        assert_eq!(inner[1][0], "file_path");
        assert_eq!(inner[1][1], "Glob");
        assert_eq!(inner[1][2], "src/finance/*");
    }

    #[test]
    fn filters_multiple_prefixes_uses_or() {
        let opts = SearchOptions {
            path_prefixes: vec!["src/finance/".into(), "src/annuity/".into()],
            ..Default::default()
        };
        let filters = build_search_filters(&opts).unwrap();
        assert_eq!(filters[0], "And");
        let inner = filters[1].as_array().unwrap();
        assert_eq!(inner.len(), 2);
        let or_cond = &inner[1];
        assert_eq!(or_cond[0], "Or");
        let or_inner = or_cond[1].as_array().unwrap();
        assert_eq!(or_inner.len(), 2);
        assert_eq!(or_inner[0][2], "src/finance/*");
        assert_eq!(or_inner[1][2], "src/annuity/*");
    }

    #[test]
    fn filters_chunk_kind_adds_eq() {
        let opts = SearchOptions {
            chunk_kind: Some(ChunkKind::Symbol),
            ..Default::default()
        };
        let filters = build_search_filters(&opts).unwrap();
        assert_eq!(filters[0], "And");
        let inner = filters[1].as_array().unwrap();
        assert_eq!(inner[1][0], "chunk_kind");
        assert_eq!(inner[1][1], "Eq");
        assert_eq!(inner[1][2], "symbol");
    }

    #[test]
    fn filters_language_adds_eq() {
        let opts = SearchOptions {
            language: Some("rust".into()),
            ..Default::default()
        };
        let filters = build_search_filters(&opts).unwrap();
        assert_eq!(filters[0], "And");
        let inner = filters[1].as_array().unwrap();
        assert_eq!(inner[1][0], "language");
        assert_eq!(inner[1][1], "Eq");
        assert_eq!(inner[1][2], "rust");
    }

    #[test]
    fn filters_all_options_produces_four_conditions() {
        let opts = SearchOptions {
            path_prefixes: vec!["src/".into()],
            chunk_kind: Some(ChunkKind::File),
            language: Some("python".into()),
            ..Default::default()
        };
        let filters = build_search_filters(&opts).unwrap();
        assert_eq!(filters[0], "And");
        let inner = filters[1].as_array().unwrap();
        assert_eq!(inner.len(), 4);
        assert_eq!(inner[0][0], "id"); // meta exclusion
        assert_eq!(inner[1][0], "file_path"); // path prefix
        assert_eq!(inner[2][0], "chunk_kind"); // chunk kind
        assert_eq!(inner[3][0], "language"); // language
    }

    // ── Score conversion + min_score filtering ───────────────────────

    #[test]
    fn dist_to_score_perfect_match() {
        assert!((dist_to_score(0.0) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn dist_to_score_orthogonal() {
        assert!((dist_to_score(1.0) - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn dist_to_score_opposite() {
        assert!((dist_to_score(2.0) - (-1.0)).abs() < f32::EPSILON);
    }

    fn make_row(id: &str, dist: f64) -> HashMap<String, serde_json::Value> {
        let mut row = HashMap::new();
        row.insert("id".into(), serde_json::json!(id));
        row.insert("$dist".into(), serde_json::json!(dist));
        row.insert("file_path".into(), serde_json::json!("src/main.rs"));
        row.insert("chunk_kind".into(), serde_json::json!("file"));
        row.insert("summary".into(), serde_json::json!("test"));
        row
    }

    #[test]
    fn score_filter_passes_all_when_no_min() {
        let rows = vec![make_row("a", 0.1), make_row("b", 1.5), make_row("c", 1.99)];
        let results = score_and_filter_rows(rows, None).unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn score_filter_excludes_below_threshold() {
        let rows = vec![
            make_row("good", 0.1), // score = 0.9
            make_row("ok", 0.5),   // score = 0.5
            make_row("bad", 1.5),  // score = -0.5
        ];
        let results = score_and_filter_rows(rows, Some(0.5)).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].id, "good");
        assert_eq!(results[1].id, "ok");
    }

    #[test]
    fn score_filter_boundary_includes_exact_match() {
        let rows = vec![make_row("exact", 0.2)]; // score = 0.8
        let results = score_and_filter_rows(rows, Some(0.8)).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn score_filter_empty_rows_returns_empty() {
        let results = score_and_filter_rows(vec![], Some(0.5)).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn missing_dist_defaults_to_worst_score() {
        let mut row = HashMap::new();
        row.insert("id".into(), serde_json::json!("no-dist"));
        row.insert("file_path".into(), serde_json::json!("src/main.rs"));
        row.insert("chunk_kind".into(), serde_json::json!("file"));
        row.insert("summary".into(), serde_json::json!("test"));
        // No $dist field — should default to dist=2.0, score=-1.0
        let results = score_and_filter_rows(vec![row], None).unwrap();
        assert_eq!(results.len(), 1);
        assert!((results[0].score - (-1.0)).abs() < 0.01);
    }

    // ── row_to_search_result edge cases ──────────────────────────────

    #[test]
    fn search_result_missing_optional_fields() {
        let mut row = HashMap::new();
        row.insert("id".into(), serde_json::json!("minimal"));
        row.insert("file_path".into(), serde_json::json!("src/lib.rs"));
        row.insert("chunk_kind".into(), serde_json::json!("file"));
        row.insert("summary".into(), serde_json::json!("a file"));
        let sr = row_to_search_result(row, 0.9).unwrap();
        assert_eq!(sr.id, "minimal");
        assert_eq!(sr.file_path, "src/lib.rs");
        assert!(sr.symbol_name.is_none());
        assert!(sr.symbol_kind.is_none());
        assert!(sr.start_line.is_none());
        assert!(sr.end_line.is_none());
        assert!(sr.language.is_none());
    }

    #[test]
    fn search_result_unknown_chunk_kind_defaults_to_file() {
        let mut row = HashMap::new();
        row.insert("id".into(), serde_json::json!("x"));
        row.insert("file_path".into(), serde_json::json!("a.rs"));
        row.insert("chunk_kind".into(), serde_json::json!("unknown_kind"));
        row.insert("summary".into(), serde_json::json!("test"));
        let sr = row_to_search_result(row, 0.5).unwrap();
        assert_eq!(sr.chunk_kind, ChunkKind::File);
    }

    #[test]
    fn search_result_missing_chunk_kind_defaults_to_file() {
        let mut row = HashMap::new();
        row.insert("id".into(), serde_json::json!("x"));
        row.insert("file_path".into(), serde_json::json!("a.rs"));
        row.insert("summary".into(), serde_json::json!("test"));
        let sr = row_to_search_result(row, 0.5).unwrap();
        assert_eq!(sr.chunk_kind, ChunkKind::File);
    }

    #[test]
    fn search_result_missing_id_defaults_to_empty() {
        let mut row = HashMap::new();
        row.insert("file_path".into(), serde_json::json!("a.rs"));
        row.insert("summary".into(), serde_json::json!("test"));
        let sr = row_to_search_result(row, 0.5).unwrap();
        assert_eq!(sr.id, "");
    }

    // ── row_to_metadata edge cases ───────────────────────────────────

    #[test]
    fn metadata_filters_reserved_keys() {
        let mut row = HashMap::new();
        row.insert("id".into(), serde_json::json!(META_VECTOR_ID));
        row.insert("vector".into(), serde_json::json!([0.0]));
        row.insert("$dist".into(), serde_json::json!(0.0));
        row.insert("__is_meta__".into(), serde_json::json!(true));
        row.insert("hwm_sha".into(), serde_json::json!("abc"));
        row.insert("embedder".into(), serde_json::json!("voyage/v3"));
        row.insert("custom_key".into(), serde_json::json!("custom_val"));

        let meta = row_to_metadata(&row).unwrap();
        assert_eq!(meta.hwm_sha.as_deref(), Some("abc"));
        assert_eq!(meta.embedder.as_deref(), Some("voyage/v3"));
        assert_eq!(meta.extra.len(), 1);
        assert_eq!(meta.extra["custom_key"], "custom_val");
    }

    #[test]
    fn metadata_extra_skips_non_string_values() {
        let mut row = HashMap::new();
        row.insert("numeric_field".into(), serde_json::json!(42));
        row.insert("string_field".into(), serde_json::json!("kept"));

        let meta = row_to_metadata(&row).unwrap();
        assert!(!meta.extra.contains_key("numeric_field"));
        assert_eq!(meta.extra["string_field"], "kept");
    }

    #[test]
    fn metadata_round_trip_with_extra_fields() {
        let mut extra = HashMap::new();
        extra.insert("version".into(), "2".into());
        extra.insert("created_by".into(), "wdpkr".into());
        let meta = NamespaceMetadata {
            hwm_sha: Some("def456".into()),
            embedder: Some("openai/text-embedding-3-large".into()),
            extra,
        };
        let row = metadata_to_row(&meta, 2);
        let back = row_to_metadata(&row).unwrap();
        assert_eq!(back.hwm_sha.as_deref(), Some("def456"));
        assert_eq!(
            back.embedder.as_deref(),
            Some("openai/text-embedding-3-large")
        );
        assert_eq!(back.extra.len(), 2);
        assert_eq!(back.extra["version"], "2");
        assert_eq!(back.extra["created_by"], "wdpkr");
    }

    // ── doc_to_row completeness ──────────────────────────────────────

    #[test]
    fn doc_to_row_all_fields_populated() {
        let doc = VectorDocument {
            id: "full-doc".into(),
            vector: vec![0.1, 0.2, 0.3],
            summary: "Full document".into(),
            file_path: "src/deep/nested/file.rs".into(),
            chunk_kind: ChunkKind::Symbol,
            symbol_name: Some("my_function".into()),
            symbol_kind: Some("method".into()),
            start_line: Some(100),
            end_line: Some(200),
            language: Some("rust".into()),
            content_hash: None,
            calls: None,
            called_by: None,
        };
        let row = doc_to_row(&doc);
        assert_eq!(row.len(), 10); // id, vector, summary, file_path, chunk_kind + 5 optional
        assert_eq!(row["id"], "full-doc");
        assert_eq!(row["chunk_kind"], "symbol");
        assert_eq!(row["symbol_name"], "my_function");
        assert_eq!(row["symbol_kind"], "method");
        assert_eq!(row["start_line"], 100);
        assert_eq!(row["end_line"], 200);
        assert_eq!(row["language"], "rust");
    }

    #[test]
    fn doc_to_row_minimal_fields() {
        let doc = VectorDocument {
            id: "min".into(),
            vector: vec![0.5],
            summary: "Minimal".into(),
            file_path: "file.txt".into(),
            chunk_kind: ChunkKind::File,
            symbol_name: None,
            symbol_kind: None,
            start_line: None,
            end_line: None,
            language: None,
            content_hash: None,
            calls: None,
            called_by: None,
        };
        let row = doc_to_row(&doc);
        assert_eq!(row.len(), 5); // id, vector, summary, file_path, chunk_kind
        assert!(!row.contains_key("symbol_name"));
        assert!(!row.contains_key("symbol_kind"));
        assert!(!row.contains_key("start_line"));
        assert!(!row.contains_key("end_line"));
        assert!(!row.contains_key("language"));
    }
}
