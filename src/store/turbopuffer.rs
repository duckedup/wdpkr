//! Turbopuffer vector store adapter (v2 API).
//!
//! Implements [`VectorStore`] against Turbopuffer's v2 HTTP API. Metadata
//! (HWM SHA, embedder identity) is stored as a reserved vector with ID
//! `__wdpkr_meta__`.

use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::{
    ChunkKind, Namespace, NamespaceMetadata, SearchOptions, SearchResult, StoreProvider,
    UpsertStats, VectorDocument, VectorStore,
};
use crate::config::StoreConfig;
use crate::http::{self, RetryPolicy};

const META_VECTOR_ID: &str = "__wdpkr_meta__";
const MAX_RETRIES: usize = 3;
const UPSERT_BATCH_SIZE: usize = 200;

// ── Provider ─────────────────────────────────────────────────────────────

pub struct TurbopufferProvider;

impl StoreProvider for TurbopufferProvider {
    fn name(&self) -> &str {
        "turbopuffer"
    }

    fn validate(&self, config: &StoreConfig) -> Result<()> {
        if config.turbopuffer.api_key.is_empty() {
            bail!("TURBOPUFFER_API_KEY is required when store.provider=turbopuffer");
        }
        Ok(())
    }

    fn build(&self, config: &StoreConfig, dimension: usize) -> Result<Box<dyn VectorStore>> {
        Ok(Box::new(TurbopufferStore::new(config, dimension)?))
    }
}

// ── Store ────────────────────────────────────────────────────────────────

pub struct TurbopufferStore {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
    dimension: usize,
}

impl TurbopufferStore {
    pub fn new(config: &StoreConfig, dimension: usize) -> Result<Self> {
        if config.turbopuffer.api_key.is_empty() {
            bail!("TURBOPUFFER_API_KEY is required");
        }
        Ok(Self {
            client: reqwest::Client::new(),
            api_key: config.turbopuffer.api_key.clone(),
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

    /// POST with bounded retry on 5xx. Returns the response for any
    /// success or client-error status; callers inspect and parse it.
    async fn post_json<T: Serialize>(&self, url: &str, body: &T) -> Result<reqwest::Response> {
        let policy = RetryPolicy::server_errors(MAX_RETRIES, 1000);
        http::send_with_retry(&policy, "Turbopuffer API", || {
            self.client.post(url).bearer_auth(&self.api_key).json(body)
        })
        .await
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
        let policy = RetryPolicy::server_errors(MAX_RETRIES, 1000);
        let resp = http::send_with_retry(&policy, "delete namespace", || {
            self.client
                .delete(self.ns_url(ns))
                .bearer_auth(&self.api_key)
        })
        .await?;

        let status = resp.status();
        if status.is_success() || status.as_u16() == 404 {
            return Ok(());
        }
        let body = resp.text().await.unwrap_or_default();
        bail!("failed to delete namespace '{}': {body}", ns.as_str());
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
        // Turbopuffer rejects any batch containing duplicate document IDs. IDs
        // are content-derived, so a duplicate ID means duplicate content —
        // collapse to the last occurrence and warn instead of failing the run.
        let (deduped, dup_ids) = dedupe_by_id(docs);
        if !dup_ids.is_empty() {
            eprintln!(
                "warning: skipped {} duplicate document ID(s) before upsert: {}",
                dup_ids.len(),
                dup_ids.join(", ")
            );
        }

        let mut upserted = 0;
        for chunk in deduped.chunks(UPSERT_BATCH_SIZE) {
            let rows: Vec<HashMap<String, serde_json::Value>> =
                chunk.iter().copied().map(doc_to_row).collect();
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
            skipped: dup_ids.len(),
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
            if is_missing_attribute_error(&err) {
                return Ok(());
            }
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
            if is_missing_attribute_error(&err) {
                return Ok(count);
            }
            bail!("delete_by_glob failed: {err}");
        }
        Ok(count)
    }

    async fn touch_by_file(&self, ns: &Namespace, file_path: &str, ts: i64) -> Result<usize> {
        let url = format!("{}/query", self.ns_url(ns));
        // Find the ids of all rows for this file_path (doc + section children).
        let query = QueryRequest {
            filters: Some(serde_json::json!(["file_path", "Eq", file_path])),
            include_attributes: Some(serde_json::json!(["file_path"])),
            limit: Some(10_000),
            ..Default::default()
        };
        let resp = self.post_json(&url, &query).await?;
        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            bail!("touch_by_file query failed: {err}");
        }
        let query_resp: QueryResponse = resp.json().await.context("parsing touch query")?;
        let ids: Vec<String> = query_resp
            .rows
            .into_iter()
            .filter_map(|row| row.get("id").and_then(|v| v.as_str()).map(String::from))
            .collect();
        if ids.is_empty() {
            return Ok(0);
        }
        // Patch only `last_used_at` — vectors and other attributes are preserved.
        let patch_rows: Vec<HashMap<String, serde_json::Value>> = ids
            .iter()
            .map(|id| {
                let mut row = HashMap::new();
                row.insert("id".to_string(), serde_json::json!(id));
                row.insert("last_used_at".to_string(), serde_json::json!(ts));
                row
            })
            .collect();
        let body = WriteRequest {
            patch_rows: Some(patch_rows),
            ..Default::default()
        };
        let resp = self.post_json(&self.ns_url(ns), &body).await?;
        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            bail!("touch_by_file patch failed: {err}");
        }
        Ok(ids.len())
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
        let mut after_id: Option<String> = None;
        let page_size = 10_000;

        loop {
            let filters = match &after_id {
                Some(id) => {
                    serde_json::json!(["And", [["id", "NotEq", META_VECTOR_ID], ["id", "Gt", id]]])
                }
                None => serde_json::json!(["id", "NotEq", META_VECTOR_ID]),
            };

            let body = QueryRequest {
                rank_by: Some(serde_json::json!(["id", "asc"])),
                filters: Some(filters),
                include_attributes: Some(serde_json::json!(true)),
                limit: Some(page_size),
                ..Default::default()
            };

            let resp = self.post_json(&url, &body).await?;
            if !resp.status().is_success() {
                let err = resp.text().await.unwrap_or_default();
                bail!("list_documents failed: {err}");
            }

            let query_resp: QueryResponse = resp.json().await.context("parsing list response")?;
            let page_len = query_resp.rows.len();

            for row in query_resp.rows {
                let doc = row_to_document(row)?;
                after_id = Some(doc.id.clone());
                all_docs.push(doc);
            }

            if page_len < page_size {
                break;
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

/// Drop documents with duplicate IDs, keeping the last occurrence of each (at
/// its last position). Turbopuffer rejects an upsert batch outright if it
/// contains duplicate IDs; content-derived IDs mean a duplicate ID is duplicate
/// content, so last-wins is a safe collapse. Order within a batch does not
/// affect the upsert. Returns the kept documents plus the list of IDs that
/// appeared more than once.
fn dedupe_by_id(docs: &[VectorDocument]) -> (Vec<&VectorDocument>, Vec<String>) {
    let mut seen = std::collections::HashSet::new();
    let mut kept = Vec::with_capacity(docs.len());
    let mut dup_ids = Vec::new();
    // Walk in reverse so the last occurrence of each ID wins, then restore order.
    for doc in docs.iter().rev() {
        if seen.insert(doc.id.as_str()) {
            kept.push(doc);
        } else {
            dup_ids.push(doc.id.clone());
        }
    }
    kept.reverse();
    dup_ids.reverse();
    (kept, dup_ids)
}

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
    if let Some(ts) = doc.last_used_at {
        row.insert("last_used_at".into(), serde_json::json!(ts));
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
        last_used_at: row.get("last_used_at").and_then(|v| v.as_i64()),
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
        last_used_at: row.get("last_used_at").and_then(|v| v.as_i64()),
    })
}

// ── HTTP types (v2 API) ──────────────────────────────────────────────────

#[derive(Serialize, Default)]
struct WriteRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    upsert_rows: Option<Vec<HashMap<String, serde_json::Value>>>,
    /// Row-based partial update: only the listed keys are written, the vector
    /// and other attributes are preserved. Used by `touch_by_file`.
    #[serde(skip_serializing_if = "Option::is_none")]
    patch_rows: Option<Vec<HashMap<String, serde_json::Value>>>,
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
}

#[derive(Deserialize)]
struct QueryResponse {
    #[serde(default)]
    rows: Vec<HashMap<String, serde_json::Value>>,
}

/// Turbopuffer rejects a filter that references an attribute the namespace has
/// never stored. For a delete, that means the rows it would match cannot exist
/// yet, so the delete is a no-op rather than a failure. Tolerating it lets the
/// first index into a fresh namespace bootstrap, where the delete-before-upsert
/// runs before any write has established the `file_path` attribute.
fn is_missing_attribute_error(body: &str) -> bool {
    body.contains("attribute not found")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{NidusConfig, TurbopufferConfig};

    #[test]
    fn missing_attribute_error_detected() {
        // Real Turbopuffer 400 body for an `Eq` filter on an attribute the
        // namespace has never stored (delete_by_file on a fresh namespace).
        let eq_err =
            r#"{"error":"filter error in key `file_path`: attribute not found","status":"error"}"#;
        assert!(is_missing_attribute_error(eq_err));
        // Same class of error for the `Glob` filter (delete_by_glob).
        let glob_err = r#"{"error":"filter error in key `file_path`: attribute not found"}"#;
        assert!(is_missing_attribute_error(glob_err));
    }

    #[test]
    fn unrelated_errors_not_treated_as_missing_attribute() {
        // Errors we must still surface as failures — never swallow these.
        assert!(!is_missing_attribute_error(r#"{"error":"rate limited"}"#));
        assert!(!is_missing_attribute_error(
            r#"{"error":"unauthorized: invalid API key"}"#
        ));
        assert!(!is_missing_attribute_error(
            r#"{"error":"namespace not found"}"#
        ));
        assert!(!is_missing_attribute_error(""));
    }

    /// Build a turbopuffer `StoreConfig` with the given API key.
    fn tp_config(api_key: &str) -> StoreConfig {
        StoreConfig {
            provider: "turbopuffer".into(),
            turbopuffer: TurbopufferConfig {
                api_key: api_key.into(),
            },
            nidus: NidusConfig {
                path: ":memory:".into(),
            },
        }
    }

    // ── Constructor ───────────────────────────────────────────────────

    #[test]
    fn requires_api_key() {
        let config = tp_config("");
        assert!(TurbopufferStore::new(&config, 1024).is_err());
    }

    #[test]
    fn constructs_with_key() {
        let config = tp_config("test-key");
        let store = TurbopufferStore::new(&config, 1024).unwrap();
        assert_eq!(store.base_url, "https://api.turbopuffer.com");
        assert_eq!(store.dimension, 1024);
    }

    #[test]
    fn custom_base_url() {
        let config = tp_config("key");
        let store = TurbopufferStore::new(&config, 512)
            .unwrap()
            .with_base_url("http://localhost:9090");
        assert_eq!(store.base_url, "http://localhost:9090");
    }

    // ── URL construction ──────────────────────────────────────────────

    #[test]
    fn namespace_url() {
        let config = tp_config("key");
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
            last_used_at: None,
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
            last_used_at: None,
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
            last_used_at: None,
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

    // ── TurbopufferProvider ─────────────────────────────────────────

    #[test]
    fn provider_name() {
        assert_eq!(TurbopufferProvider.name(), "turbopuffer");
    }

    #[test]
    fn provider_validate_with_key() {
        let config = tp_config("key");
        assert!(TurbopufferProvider.validate(&config).is_ok());
    }

    #[test]
    fn provider_validate_without_key() {
        let config = tp_config("");
        let err = TurbopufferProvider.validate(&config).unwrap_err();
        assert!(err.to_string().contains("TURBOPUFFER_API_KEY"));
    }

    #[test]
    fn provider_build_succeeds() {
        let config = tp_config("key");
        assert!(TurbopufferProvider.build(&config, 1024).is_ok());
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

    // ── dedupe_by_id ─────────────────────────────────────────────────

    fn doc_with_id(id: &str, summary: &str) -> VectorDocument {
        VectorDocument {
            id: id.into(),
            vector: vec![0.1],
            summary: summary.into(),
            file_path: "src/main.rs".into(),
            chunk_kind: ChunkKind::Symbol,
            symbol_name: Some("dup".into()),
            symbol_kind: Some("function".into()),
            start_line: None,
            end_line: None,
            language: Some("rust".into()),
            content_hash: None,
            calls: None,
            called_by: None,
            last_used_at: None,
        }
    }

    #[test]
    fn dedupe_no_duplicates_keeps_all_in_order() {
        let docs = vec![doc_with_id("a", "1"), doc_with_id("b", "2")];
        let (kept, dups) = dedupe_by_id(&docs);
        assert!(dups.is_empty());
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].id, "a");
        assert_eq!(kept[1].id, "b");
    }

    #[test]
    fn dedupe_keeps_last_occurrence_and_reports_dup() {
        let docs = vec![
            doc_with_id("a", "first-a"),
            doc_with_id("b", "b"),
            doc_with_id("a", "second-a"),
        ];
        let (kept, dups) = dedupe_by_id(&docs);
        assert_eq!(dups, vec!["a".to_string()]);
        assert_eq!(kept.len(), 2);
        // Last-wins: the kept "a" carries the later content.
        let a = kept.iter().find(|d| d.id == "a").unwrap();
        assert_eq!(a.summary, "second-a");
        assert!(kept.iter().any(|d| d.id == "b"));
    }

    #[test]
    fn dedupe_multiple_repeats_of_same_id() {
        let docs = vec![
            doc_with_id("x", "1"),
            doc_with_id("x", "2"),
            doc_with_id("x", "3"),
        ];
        let (kept, dups) = dedupe_by_id(&docs);
        // Two extras dropped, both reported.
        assert_eq!(dups, vec!["x".to_string(), "x".to_string()]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].summary, "3");
    }

    #[test]
    fn dedupe_empty_input() {
        let (kept, dups) = dedupe_by_id(&[]);
        assert!(kept.is_empty());
        assert!(dups.is_empty());
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
            last_used_at: None,
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
            last_used_at: None,
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
