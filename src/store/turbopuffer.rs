//! Turbopuffer vector store adapter.
//!
//! Implements [`VectorStore`] against Turbopuffer's HTTP API. Metadata
//! (HWM SHA, embedder identity) is stored as a reserved vector with ID
//! `__megagrep_meta__` — portable across API versions.

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

const META_VECTOR_ID: &str = "__megagrep_meta__";
const MAX_RETRIES: usize = 3;
const UPSERT_BATCH_SIZE: usize = 200;

pub struct TurbopufferStore {
    client: reqwest::Client,
    api_key: String,
    base_url: String,
}

impl TurbopufferStore {
    pub fn new(config: &StoreConfig) -> Result<Self> {
        if config.api_key.is_empty() {
            bail!("TURBOPUFFER_API_KEY is required");
        }
        Ok(Self {
            client: reqwest::Client::new(),
            api_key: config.api_key.clone(),
            base_url: "https://api.turbopuffer.com".into(),
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
        let meta_vec = metadata_to_vector(&meta, dimension);
        let body = UpsertRequest {
            upserts: vec![meta_vec],
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
        let resp = self
            .client
            .get(self.ns_url(ns))
            .bearer_auth(&self.api_key)
            .send()
            .await
            .context("namespace_exists request failed")?;

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
        let body = QueryRequest {
            vector: vec![0.0; 1],
            top_k: 1,
            filters: Some(serde_json::json!({
                "id": ["Eq", META_VECTOR_ID]
            })),
            include_attributes: true,
        };

        let resp = self.post_json(&url, &body).await?;
        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            bail!("get_metadata failed: {err}");
        }

        let query_resp: QueryResponse = resp.json().await.context("parsing metadata response")?;
        match query_resp.results.first() {
            Some(result) => vector_to_metadata(result),
            None => Ok(NamespaceMetadata::default()),
        }
    }

    async fn set_metadata(&self, ns: &Namespace, meta: &NamespaceMetadata) -> Result<()> {
        let existing = self.get_metadata(ns).await.ok();
        let dimension = if let Some(ref _existing) = existing {
            // Preserve the dimension from the existing meta vector.
            // We use a small dimension for the meta vector itself.
            1
        } else {
            1
        };

        let meta_vec = metadata_to_vector(meta, dimension);
        let body = UpsertRequest {
            upserts: vec![meta_vec],
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
            let upserts: Vec<UpsertVector> = chunk.iter().map(doc_to_upsert_vector).collect();
            let body = UpsertRequest { upserts };
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
        let url = format!("{}/delete", self.ns_url(ns));
        let body = DeleteRequest {
            ids: ids.iter().map(|s| (*s).to_string()).collect(),
            filters: None,
        };
        let resp = self.post_json(&url, &body).await?;
        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            bail!("delete_by_ids failed: {err}");
        }
        Ok(())
    }

    async fn delete_by_file(&self, ns: &Namespace, file_path: &str) -> Result<()> {
        let url = format!("{}/delete", self.ns_url(ns));
        let body = DeleteRequest {
            ids: vec![],
            filters: Some(serde_json::json!({
                "file_path": ["Eq", file_path]
            })),
        };
        let resp = self.post_json(&url, &body).await?;
        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            bail!("delete_by_file failed: {err}");
        }
        Ok(())
    }

    async fn search(
        &self,
        ns: &Namespace,
        query_vector: &[f32],
        opts: &SearchOptions,
    ) -> Result<Vec<SearchResult>> {
        let url = format!("{}/query", self.ns_url(ns));

        let mut filters = serde_json::Map::new();
        // Exclude the metadata vector from search results.
        filters.insert("id".into(), serde_json::json!(["NotEq", META_VECTOR_ID]));
        if let Some(ref prefix) = opts.path_prefix {
            filters.insert(
                "file_path".into(),
                serde_json::json!(["StartsWith", prefix]),
            );
        }
        if let Some(ref kind) = opts.chunk_kind {
            filters.insert(
                "chunk_kind".into(),
                serde_json::json!(["Eq", kind.to_string()]),
            );
        }
        if let Some(ref lang) = opts.language {
            filters.insert("language".into(), serde_json::json!(["Eq", lang]));
        }

        let body = QueryRequest {
            vector: query_vector.to_vec(),
            top_k: opts.top_k,
            filters: if filters.is_empty() {
                None
            } else {
                Some(serde_json::Value::Object(filters))
            },
            include_attributes: true,
        };

        let resp = self.post_json(&url, &body).await?;
        if !resp.status().is_success() {
            let err = resp.text().await.unwrap_or_default();
            bail!("search failed: {err}");
        }

        let query_resp: QueryResponse = resp.json().await.context("parsing search response")?;
        let results = query_resp
            .results
            .into_iter()
            .filter(|r| {
                if let Some(min) = opts.min_score {
                    r.dist >= min
                } else {
                    true
                }
            })
            .map(query_result_to_search_result)
            .collect::<Result<Vec<_>>>()?;

        Ok(results)
    }
}

// ── Conversion helpers ────────────────────────────────────────────────────

fn doc_to_upsert_vector(doc: &VectorDocument) -> UpsertVector {
    let mut attrs = HashMap::new();
    attrs.insert("file_path".into(), serde_json::json!(doc.file_path));
    attrs.insert(
        "chunk_kind".into(),
        serde_json::json!(doc.chunk_kind.to_string()),
    );
    attrs.insert("summary".into(), serde_json::json!(doc.summary));
    if let Some(ref name) = doc.symbol_name {
        attrs.insert("symbol_name".into(), serde_json::json!(name));
    }
    if let Some(ref kind) = doc.symbol_kind {
        attrs.insert("symbol_kind".into(), serde_json::json!(kind));
    }
    if let Some(line) = doc.start_line {
        attrs.insert("start_line".into(), serde_json::json!(line));
    }
    if let Some(line) = doc.end_line {
        attrs.insert("end_line".into(), serde_json::json!(line));
    }
    if let Some(ref lang) = doc.language {
        attrs.insert("language".into(), serde_json::json!(lang));
    }
    UpsertVector {
        id: doc.id.clone(),
        vector: doc.vector.clone(),
        attributes: attrs,
    }
}

fn metadata_to_vector(meta: &NamespaceMetadata, dimension: usize) -> UpsertVector {
    let mut attrs = HashMap::new();
    attrs.insert("__is_meta__".into(), serde_json::json!(true));
    if let Some(ref sha) = meta.hwm_sha {
        attrs.insert("hwm_sha".into(), serde_json::json!(sha));
    }
    if let Some(ref embedder) = meta.embedder {
        attrs.insert("embedder".into(), serde_json::json!(embedder));
    }
    for (k, v) in &meta.extra {
        attrs.insert(k.clone(), serde_json::json!(v));
    }
    UpsertVector {
        id: META_VECTOR_ID.into(),
        vector: vec![0.0; dimension],
        attributes: attrs,
    }
}

fn vector_to_metadata(result: &QueryResult) -> Result<NamespaceMetadata> {
    let attrs = result.attributes.as_ref().cloned().unwrap_or_default();
    Ok(NamespaceMetadata {
        hwm_sha: attrs
            .get("hwm_sha")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        embedder: attrs
            .get("embedder")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        extra: attrs
            .iter()
            .filter(|(k, _)| !matches!(k.as_str(), "hwm_sha" | "embedder" | "__is_meta__"))
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect(),
    })
}

fn query_result_to_search_result(r: QueryResult) -> Result<SearchResult> {
    let attrs = r.attributes.unwrap_or_default();
    let get_str = |key: &str| -> Option<String> {
        attrs
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };
    let get_u32 =
        |key: &str| -> Option<u32> { attrs.get(key).and_then(|v| v.as_u64()).map(|n| n as u32) };

    let chunk_kind_str = get_str("chunk_kind").unwrap_or_else(|| "file".into());
    let chunk_kind = match chunk_kind_str.as_str() {
        "symbol" => ChunkKind::Symbol,
        _ => ChunkKind::File,
    };

    Ok(SearchResult {
        id: r.id,
        score: r.dist,
        file_path: get_str("file_path").unwrap_or_default(),
        chunk_kind,
        symbol_name: get_str("symbol_name"),
        symbol_kind: get_str("symbol_kind"),
        summary: get_str("summary").unwrap_or_default(),
        start_line: get_u32("start_line"),
        end_line: get_u32("end_line"),
        language: get_str("language"),
    })
}

// ── HTTP types ────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct UpsertRequest {
    upserts: Vec<UpsertVector>,
}

#[derive(Serialize)]
struct UpsertVector {
    id: String,
    vector: Vec<f32>,
    attributes: HashMap<String, serde_json::Value>,
}

#[derive(Serialize)]
struct QueryRequest {
    vector: Vec<f32>,
    top_k: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    filters: Option<serde_json::Value>,
    include_attributes: bool,
}

#[derive(Serialize)]
struct DeleteRequest {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    filters: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct QueryResponse {
    results: Vec<QueryResult>,
}

#[derive(Deserialize)]
struct QueryResult {
    id: String,
    dist: f32,
    attributes: Option<HashMap<String, serde_json::Value>>,
}

fn backoff(attempt: usize) -> Duration {
    Duration::from_millis(1000 * 2u64.pow(attempt as u32))
}

// ── build_store factory ───────────────────────────────────────────────────

pub fn build_store(config: &StoreConfig) -> Result<Box<dyn VectorStore>> {
    match config.provider.as_str() {
        "turbopuffer" => Ok(Box::new(TurbopufferStore::new(config)?)),
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
        assert!(TurbopufferStore::new(&config).is_err());
    }

    #[test]
    fn constructs_with_key() {
        let config = StoreConfig {
            provider: "turbopuffer".into(),
            api_key: "test-key".into(),
        };
        let store = TurbopufferStore::new(&config).unwrap();
        assert_eq!(store.base_url, "https://api.turbopuffer.com");
    }

    #[test]
    fn custom_base_url() {
        let config = StoreConfig {
            provider: "turbopuffer".into(),
            api_key: "key".into(),
        };
        let store = TurbopufferStore::new(&config)
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
        let store = TurbopufferStore::new(&config).unwrap();
        let ns = Namespace::from("my-repo");
        assert_eq!(
            store.ns_url(&ns),
            "https://api.turbopuffer.com/v2/namespaces/my-repo"
        );
    }

    // ── Serialization ─────────────────────────────────────────────────

    #[test]
    fn upsert_vector_serialization() {
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
        };
        let uv = doc_to_upsert_vector(&doc);
        assert_eq!(uv.id, "test-1");
        assert_eq!(uv.vector, vec![0.1, 0.2]);
        assert_eq!(uv.attributes["file_path"], "src/main.rs");
        assert_eq!(uv.attributes["chunk_kind"], "file");
        assert!(!uv.attributes.contains_key("symbol_name"));
    }

    #[test]
    fn upsert_vector_with_symbol_fields() {
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
        };
        let uv = doc_to_upsert_vector(&doc);
        assert_eq!(uv.attributes["symbol_name"], "process");
        assert_eq!(uv.attributes["symbol_kind"], "function");
        assert_eq!(uv.attributes["start_line"], 10);
        assert_eq!(uv.attributes["end_line"], 25);
    }

    // ── Metadata round-trip ───────────────────────────────────────────

    #[test]
    fn metadata_to_vector_and_back() {
        let meta = NamespaceMetadata {
            hwm_sha: Some("abc123".into()),
            embedder: Some("voyage/voyage-code-3".into()),
            extra: HashMap::new(),
        };
        let uv = metadata_to_vector(&meta, 3);
        assert_eq!(uv.id, META_VECTOR_ID);
        assert_eq!(uv.vector.len(), 3);
        assert_eq!(uv.attributes["hwm_sha"], "abc123");
        assert_eq!(uv.attributes["embedder"], "voyage/voyage-code-3");

        let qr = QueryResult {
            id: META_VECTOR_ID.into(),
            dist: 0.0,
            attributes: Some(uv.attributes),
        };
        let back = vector_to_metadata(&qr).unwrap();
        assert_eq!(back.hwm_sha.as_deref(), Some("abc123"));
        assert_eq!(back.embedder.as_deref(), Some("voyage/voyage-code-3"));
    }

    #[test]
    fn metadata_with_no_attributes_returns_defaults() {
        let qr = QueryResult {
            id: META_VECTOR_ID.into(),
            dist: 0.0,
            attributes: None,
        };
        let meta = vector_to_metadata(&qr).unwrap();
        assert!(meta.hwm_sha.is_none());
        assert!(meta.embedder.is_none());
    }

    // ── Query response parsing ────────────────────────────────────────

    #[test]
    fn parse_query_response() {
        let json = r#"{
            "results": [
                {
                    "id": "doc-1",
                    "dist": 0.87,
                    "attributes": {
                        "file_path": "src/main.rs",
                        "chunk_kind": "file",
                        "summary": "Entry point",
                        "language": "rust"
                    }
                }
            ]
        }"#;
        let resp: QueryResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.results.len(), 1);

        let sr = query_result_to_search_result(resp.results.into_iter().next().unwrap()).unwrap();
        assert_eq!(sr.id, "doc-1");
        assert_eq!(sr.score, 0.87);
        assert_eq!(sr.file_path, "src/main.rs");
        assert_eq!(sr.chunk_kind, ChunkKind::File);
        assert_eq!(sr.language.as_deref(), Some("rust"));
    }

    #[test]
    fn parse_symbol_query_result() {
        let json = r#"{
            "id": "sym-1",
            "dist": 0.91,
            "attributes": {
                "file_path": "src/lib.rs",
                "chunk_kind": "symbol",
                "summary": "Processes things",
                "symbol_name": "process",
                "symbol_kind": "function",
                "start_line": 10,
                "end_line": 25
            }
        }"#;
        let qr: QueryResult = serde_json::from_str(json).unwrap();
        let sr = query_result_to_search_result(qr).unwrap();
        assert_eq!(sr.chunk_kind, ChunkKind::Symbol);
        assert_eq!(sr.symbol_name.as_deref(), Some("process"));
        assert_eq!(sr.start_line, Some(10));
    }

    // ── Query request serialization ───────────────────────────────────

    #[test]
    fn query_request_with_filters() {
        let req = QueryRequest {
            vector: vec![0.1, 0.2],
            top_k: 5,
            filters: Some(serde_json::json!({
                "file_path": ["StartsWith", "src/finance/"],
                "chunk_kind": ["Eq", "file"]
            })),
            include_attributes: true,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["top_k"], 5);
        assert!(json["filters"]["file_path"].is_array());
    }

    #[test]
    fn query_request_without_filters() {
        let req = QueryRequest {
            vector: vec![0.1],
            top_k: 10,
            filters: None,
            include_attributes: true,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("filters").is_none());
    }

    // ── Delete request serialization ──────────────────────────────────

    #[test]
    fn delete_by_ids_request() {
        let req = DeleteRequest {
            ids: vec!["a".into(), "b".into()],
            filters: None,
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["ids"].as_array().unwrap().len(), 2);
        assert!(json.get("filters").is_none());
    }

    #[test]
    fn delete_by_filter_request() {
        let req = DeleteRequest {
            ids: vec![],
            filters: Some(serde_json::json!({"file_path": ["Eq", "src/main.rs"]})),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert!(json.get("ids").is_none());
        assert!(json["filters"]["file_path"].is_array());
    }

    // ── Factory ───────────────────────────────────────────────────────

    #[test]
    fn factory_turbopuffer_with_key() {
        let config = StoreConfig {
            provider: "turbopuffer".into(),
            api_key: "key".into(),
        };
        assert!(build_store(&config).is_ok());
    }

    #[test]
    fn factory_turbopuffer_without_key_errors() {
        let config = StoreConfig {
            provider: "turbopuffer".into(),
            api_key: String::new(),
        };
        assert!(build_store(&config).is_err());
    }

    #[test]
    fn factory_unknown_provider_errors() {
        let config = StoreConfig {
            provider: "qdrant".into(),
            api_key: "key".into(),
        };
        assert!(build_store(&config).is_err());
    }
}
