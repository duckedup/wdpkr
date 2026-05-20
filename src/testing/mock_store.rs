//! In-memory [`VectorStore`] implementation for tests and the eval harness.
//!
//! Uses real cosine similarity for search ranking so tests can verify
//! ordering invariants that mirror production behavior.

use std::collections::HashMap;
use std::sync::Mutex;

use anyhow::{Result, bail};
use async_trait::async_trait;

use crate::store::{
    Namespace, NamespaceMetadata, SearchOptions, SearchResult, UpsertStats, VectorDocument,
    VectorStore,
};

struct MockNamespace {
    dimension: usize,
    metadata: NamespaceMetadata,
    documents: HashMap<String, VectorDocument>,
}

pub struct MockVectorStore {
    namespaces: Mutex<HashMap<String, MockNamespace>>,
}

impl MockVectorStore {
    pub fn new() -> Self {
        Self {
            namespaces: Mutex::new(HashMap::new()),
        }
    }
}

impl MockVectorStore {
    pub fn document_count(&self, ns: &Namespace, file_path: &str) -> usize {
        let lock = self.namespaces.lock().unwrap();
        lock.get(ns.as_str())
            .map(|ns_data| {
                ns_data
                    .documents
                    .values()
                    .filter(|d| d.file_path == file_path)
                    .count()
            })
            .unwrap_or(0)
    }
}

impl Default for MockVectorStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl VectorStore for MockVectorStore {
    async fn create_namespace(&self, ns: &Namespace, dimension: usize) -> Result<()> {
        let mut lock = self.namespaces.lock().unwrap();
        if lock.contains_key(ns.as_str()) {
            bail!("namespace '{}' already exists", ns.as_str());
        }
        lock.insert(
            ns.as_str().to_string(),
            MockNamespace {
                dimension,
                metadata: NamespaceMetadata::default(),
                documents: HashMap::new(),
            },
        );
        Ok(())
    }

    async fn delete_namespace(&self, ns: &Namespace) -> Result<()> {
        let mut lock = self.namespaces.lock().unwrap();
        lock.remove(ns.as_str());
        Ok(())
    }

    async fn namespace_exists(&self, ns: &Namespace) -> Result<bool> {
        let lock = self.namespaces.lock().unwrap();
        Ok(lock.contains_key(ns.as_str()))
    }

    async fn get_metadata(&self, ns: &Namespace) -> Result<NamespaceMetadata> {
        let lock = self.namespaces.lock().unwrap();
        let ns_data = lock
            .get(ns.as_str())
            .ok_or_else(|| anyhow::anyhow!("namespace '{}' not found", ns.as_str()))?;
        Ok(ns_data.metadata.clone())
    }

    async fn set_metadata(&self, ns: &Namespace, meta: &NamespaceMetadata) -> Result<()> {
        let mut lock = self.namespaces.lock().unwrap();
        let ns_data = lock
            .get_mut(ns.as_str())
            .ok_or_else(|| anyhow::anyhow!("namespace '{}' not found", ns.as_str()))?;
        ns_data.metadata = meta.clone();
        Ok(())
    }

    async fn upsert(&self, ns: &Namespace, docs: &[VectorDocument]) -> Result<UpsertStats> {
        let mut lock = self.namespaces.lock().unwrap();
        let ns_data = lock
            .get_mut(ns.as_str())
            .ok_or_else(|| anyhow::anyhow!("namespace '{}' not found", ns.as_str()))?;

        let mut upserted = 0;
        for doc in docs {
            if doc.vector.len() != ns_data.dimension {
                bail!(
                    "vector dimension mismatch: expected {}, got {}",
                    ns_data.dimension,
                    doc.vector.len()
                );
            }
            ns_data.documents.insert(doc.id.clone(), doc.clone());
            upserted += 1;
        }

        Ok(UpsertStats {
            upserted,
            skipped: 0,
        })
    }

    async fn delete_by_ids(&self, ns: &Namespace, ids: &[&str]) -> Result<()> {
        let mut lock = self.namespaces.lock().unwrap();
        let ns_data = lock
            .get_mut(ns.as_str())
            .ok_or_else(|| anyhow::anyhow!("namespace '{}' not found", ns.as_str()))?;
        for id in ids {
            ns_data.documents.remove(*id);
        }
        Ok(())
    }

    async fn delete_by_file(&self, ns: &Namespace, file_path: &str) -> Result<()> {
        let mut lock = self.namespaces.lock().unwrap();
        let ns_data = lock
            .get_mut(ns.as_str())
            .ok_or_else(|| anyhow::anyhow!("namespace '{}' not found", ns.as_str()))?;
        ns_data
            .documents
            .retain(|_, doc| doc.file_path != file_path);
        Ok(())
    }

    async fn get_content_hashes(&self, ns: &Namespace) -> Result<HashMap<String, String>> {
        let lock = self.namespaces.lock().unwrap();
        let ns_data = lock
            .get(ns.as_str())
            .ok_or_else(|| anyhow::anyhow!("namespace '{}' not found", ns.as_str()))?;
        let mut hashes = HashMap::new();
        for doc in ns_data.documents.values() {
            if doc.chunk_kind == crate::store::ChunkKind::File
                && let Some(ref hash) = doc.content_hash
            {
                hashes.insert(doc.file_path.clone(), hash.clone());
            }
        }
        Ok(hashes)
    }

    async fn list_documents(&self, ns: &Namespace) -> Result<Vec<VectorDocument>> {
        let lock = self.namespaces.lock().unwrap();
        let ns_data = lock
            .get(ns.as_str())
            .ok_or_else(|| anyhow::anyhow!("namespace '{}' not found", ns.as_str()))?;
        Ok(ns_data.documents.values().cloned().collect())
    }

    async fn search(
        &self,
        ns: &Namespace,
        query_vector: &[f32],
        opts: &SearchOptions,
    ) -> Result<Vec<SearchResult>> {
        let lock = self.namespaces.lock().unwrap();
        let ns_data = lock
            .get(ns.as_str())
            .ok_or_else(|| anyhow::anyhow!("namespace '{}' not found", ns.as_str()))?;

        let mut scored: Vec<(f32, &VectorDocument)> = ns_data
            .documents
            .values()
            .filter(|doc| {
                if !opts.path_prefixes.is_empty()
                    && !opts
                        .path_prefixes
                        .iter()
                        .any(|p| doc.file_path.starts_with(p.as_str()))
                {
                    return false;
                }
                if let Some(kind) = opts.chunk_kind
                    && doc.chunk_kind != kind
                {
                    return false;
                }
                if let Some(ref lang) = opts.language
                    && doc.language.as_deref() != Some(lang.as_str())
                {
                    return false;
                }
                true
            })
            .map(|doc| (cosine_similarity(query_vector, &doc.vector), doc))
            .filter(|(score, _)| opts.min_score.is_none_or(|min| *score >= min))
            .collect();

        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        let top_k = if opts.top_k > 0 {
            opts.top_k
        } else {
            scored.len()
        };

        Ok(scored
            .into_iter()
            .take(top_k)
            .map(|(score, doc)| SearchResult {
                id: doc.id.clone(),
                score,
                file_path: doc.file_path.clone(),
                chunk_kind: doc.chunk_kind,
                symbol_name: doc.symbol_name.clone(),
                symbol_kind: doc.symbol_kind.clone(),
                summary: doc.summary.clone(),
                start_line: doc.start_line,
                end_line: doc.end_line,
                language: doc.language.clone(),
                calls: doc.calls.clone(),
                called_by: doc.called_by.clone(),
            })
            .collect())
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let mag_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let mag_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if mag_a == 0.0 || mag_b == 0.0 {
        0.0
    } else {
        dot / (mag_a * mag_b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::ChunkKind;

    fn ns(name: &str) -> Namespace {
        Namespace::from(name)
    }

    fn doc_with_vector(id: &str, file_path: &str, vector: Vec<f32>) -> VectorDocument {
        VectorDocument {
            id: id.into(),
            vector,
            summary: format!("summary of {id}"),
            file_path: file_path.into(),
            chunk_kind: ChunkKind::File,
            symbol_name: None,
            symbol_kind: None,
            start_line: None,
            end_line: None,
            language: Some("rust".into()),
            content_hash: None,
            calls: None,
            called_by: None,
        }
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn create_and_check_namespace() {
        let store = MockVectorStore::new();
        assert!(!store.namespace_exists(&ns("repo")).await.unwrap());
        store.create_namespace(&ns("repo"), 3).await.unwrap();
        assert!(store.namespace_exists(&ns("repo")).await.unwrap());
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn create_duplicate_namespace_errors() {
        let store = MockVectorStore::new();
        store.create_namespace(&ns("repo"), 3).await.unwrap();
        assert!(store.create_namespace(&ns("repo"), 3).await.is_err());
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn delete_namespace() {
        let store = MockVectorStore::new();
        store.create_namespace(&ns("repo"), 3).await.unwrap();
        store.delete_namespace(&ns("repo")).await.unwrap();
        assert!(!store.namespace_exists(&ns("repo")).await.unwrap());
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn metadata_round_trip() {
        let store = MockVectorStore::new();
        store.create_namespace(&ns("repo"), 3).await.unwrap();

        let meta = NamespaceMetadata {
            hwm_sha: Some("abc123".into()),
            embedder: Some("voyage/voyage-code-3".into()),
            ..Default::default()
        };
        store.set_metadata(&ns("repo"), &meta).await.unwrap();

        let loaded = store.get_metadata(&ns("repo")).await.unwrap();
        assert_eq!(loaded.hwm_sha.as_deref(), Some("abc123"));
        assert_eq!(loaded.embedder.as_deref(), Some("voyage/voyage-code-3"));
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn metadata_on_missing_namespace_errors() {
        let store = MockVectorStore::new();
        assert!(store.get_metadata(&ns("nope")).await.is_err());
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn upsert_and_search() {
        let store = MockVectorStore::new();
        store.create_namespace(&ns("repo"), 3).await.unwrap();

        let docs = vec![
            doc_with_vector("a", "src/a.rs", vec![1.0, 0.0, 0.0]),
            doc_with_vector("b", "src/b.rs", vec![0.0, 1.0, 0.0]),
            doc_with_vector("c", "src/c.rs", vec![0.7, 0.7, 0.0]),
        ];
        let stats = store.upsert(&ns("repo"), &docs).await.unwrap();
        assert_eq!(stats.upserted, 3);

        // Query close to [1,0,0] → "a" should rank first, "c" second, "b" last
        let query = vec![0.9, 0.1, 0.0];
        let results = store
            .search(
                &ns("repo"),
                &query,
                &SearchOptions {
                    top_k: 3,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        assert_eq!(results.len(), 3);
        assert_eq!(results[0].id, "a");
        assert_eq!(results[1].id, "c");
        assert_eq!(results[2].id, "b");
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn search_respects_top_k() {
        let store = MockVectorStore::new();
        store.create_namespace(&ns("repo"), 3).await.unwrap();

        let docs = vec![
            doc_with_vector("a", "src/a.rs", vec![1.0, 0.0, 0.0]),
            doc_with_vector("b", "src/b.rs", vec![0.0, 1.0, 0.0]),
        ];
        store.upsert(&ns("repo"), &docs).await.unwrap();

        let results = store
            .search(
                &ns("repo"),
                &[1.0, 0.0, 0.0],
                &SearchOptions {
                    top_k: 1,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "a");
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn search_filters_by_path_prefix() {
        let store = MockVectorStore::new();
        store.create_namespace(&ns("repo"), 3).await.unwrap();

        let docs = vec![
            doc_with_vector("a", "src/finance/a.rs", vec![1.0, 0.0, 0.0]),
            doc_with_vector("b", "src/auth/b.rs", vec![1.0, 0.0, 0.0]),
        ];
        store.upsert(&ns("repo"), &docs).await.unwrap();

        let results = store
            .search(
                &ns("repo"),
                &[1.0, 0.0, 0.0],
                &SearchOptions {
                    top_k: 10,
                    path_prefixes: vec!["src/finance/".into()],
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].file_path, "src/finance/a.rs");
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn search_filters_by_multiple_prefixes() {
        let store = MockVectorStore::new();
        store.create_namespace(&ns("repo"), 3).await.unwrap();

        let docs = vec![
            doc_with_vector("a", "src/finance/a.rs", vec![1.0, 0.0, 0.0]),
            doc_with_vector("b", "src/auth/b.rs", vec![1.0, 0.0, 0.0]),
            doc_with_vector("c", "src/api/c.rs", vec![1.0, 0.0, 0.0]),
        ];
        store.upsert(&ns("repo"), &docs).await.unwrap();

        let results = store
            .search(
                &ns("repo"),
                &[1.0, 0.0, 0.0],
                &SearchOptions {
                    top_k: 10,
                    path_prefixes: vec!["src/finance/".into(), "src/auth/".into()],
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        let paths: Vec<&str> = results.iter().map(|r| r.file_path.as_str()).collect();
        assert!(paths.contains(&"src/finance/a.rs"));
        assert!(paths.contains(&"src/auth/b.rs"));
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn search_filters_by_chunk_kind() {
        let store = MockVectorStore::new();
        store.create_namespace(&ns("repo"), 3).await.unwrap();

        let mut file_doc = doc_with_vector("f", "src/a.rs", vec![1.0, 0.0, 0.0]);
        file_doc.chunk_kind = ChunkKind::File;

        let mut sym_doc = doc_with_vector("s", "src/a.rs", vec![1.0, 0.0, 0.0]);
        sym_doc.chunk_kind = ChunkKind::Symbol;
        sym_doc.symbol_name = Some("run".into());

        store
            .upsert(&ns("repo"), &[file_doc, sym_doc])
            .await
            .unwrap();

        let results = store
            .search(
                &ns("repo"),
                &[1.0, 0.0, 0.0],
                &SearchOptions {
                    top_k: 10,
                    chunk_kind: Some(ChunkKind::Symbol),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].symbol_name.as_deref(), Some("run"));
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn search_filters_by_min_score() {
        let store = MockVectorStore::new();
        store.create_namespace(&ns("repo"), 3).await.unwrap();

        let docs = vec![
            doc_with_vector("close", "src/a.rs", vec![1.0, 0.0, 0.0]),
            doc_with_vector("far", "src/b.rs", vec![0.0, 0.0, 1.0]),
        ];
        store.upsert(&ns("repo"), &docs).await.unwrap();

        let results = store
            .search(
                &ns("repo"),
                &[1.0, 0.0, 0.0],
                &SearchOptions {
                    top_k: 10,
                    min_score: Some(0.5),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "close");
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn delete_by_ids() {
        let store = MockVectorStore::new();
        store.create_namespace(&ns("repo"), 3).await.unwrap();

        let docs = vec![
            doc_with_vector("a", "src/a.rs", vec![1.0, 0.0, 0.0]),
            doc_with_vector("b", "src/b.rs", vec![0.0, 1.0, 0.0]),
        ];
        store.upsert(&ns("repo"), &docs).await.unwrap();
        store.delete_by_ids(&ns("repo"), &["a"]).await.unwrap();

        let results = store
            .search(
                &ns("repo"),
                &[1.0, 1.0, 0.0],
                &SearchOptions {
                    top_k: 10,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "b");
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn delete_by_file() {
        let store = MockVectorStore::new();
        store.create_namespace(&ns("repo"), 3).await.unwrap();

        let docs = vec![
            doc_with_vector("f1", "src/a.rs", vec![1.0, 0.0, 0.0]),
            doc_with_vector("s1", "src/a.rs", vec![0.9, 0.1, 0.0]),
            doc_with_vector("f2", "src/b.rs", vec![0.0, 1.0, 0.0]),
        ];
        store.upsert(&ns("repo"), &docs).await.unwrap();
        store.delete_by_file(&ns("repo"), "src/a.rs").await.unwrap();

        let results = store
            .search(
                &ns("repo"),
                &[1.0, 1.0, 0.0],
                &SearchOptions {
                    top_k: 10,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].file_path, "src/b.rs");
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn upsert_overwrites_existing_id() {
        let store = MockVectorStore::new();
        store.create_namespace(&ns("repo"), 3).await.unwrap();

        store
            .upsert(
                &ns("repo"),
                &[doc_with_vector("a", "src/a.rs", vec![1.0, 0.0, 0.0])],
            )
            .await
            .unwrap();

        // Upsert same ID with different summary
        let mut updated = doc_with_vector("a", "src/a.rs", vec![1.0, 0.0, 0.0]);
        updated.summary = "updated summary".into();
        store.upsert(&ns("repo"), &[updated]).await.unwrap();

        let results = store
            .search(
                &ns("repo"),
                &[1.0, 0.0, 0.0],
                &SearchOptions {
                    top_k: 10,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].summary, "updated summary");
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn upsert_dimension_mismatch_errors() {
        let store = MockVectorStore::new();
        store.create_namespace(&ns("repo"), 3).await.unwrap();

        let bad_doc = doc_with_vector("a", "src/a.rs", vec![1.0, 0.0]); // 2 dims, namespace is 3
        assert!(store.upsert(&ns("repo"), &[bad_doc]).await.is_err());
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn search_on_missing_namespace_errors() {
        let store = MockVectorStore::new();
        assert!(
            store
                .search(
                    &ns("nope"),
                    &[1.0],
                    &SearchOptions {
                        top_k: 5,
                        ..Default::default()
                    },
                )
                .await
                .is_err()
        );
    }

    #[test]
    fn cosine_similarity_identical_vectors() {
        let sim = cosine_similarity(&[1.0, 0.0, 0.0], &[1.0, 0.0, 0.0]);
        assert!((sim - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_orthogonal_vectors() {
        let sim = cosine_similarity(&[1.0, 0.0, 0.0], &[0.0, 1.0, 0.0]);
        assert!(sim.abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_zero_vector() {
        let sim = cosine_similarity(&[0.0, 0.0, 0.0], &[1.0, 0.0, 0.0]);
        assert_eq!(sim, 0.0);
    }
}
