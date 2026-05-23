//! Indexing pipeline: summarize → embed → build VectorDocuments.

use std::time::{Duration, Instant};

use anyhow::Result;

use crate::chunk::SymbolChunk;
use crate::embed::Embedder;
use crate::store::{ChunkKind, VectorDocument};
use crate::summarize::rollup::{DEFAULT_TOKEN_THRESHOLD, summarize_file_and_symbols};
use crate::summarize::{FileSummaryInput, Summarizer};
use crate::tap::SourceItem;

pub struct PipelineResult {
    pub documents: Vec<VectorDocument>,
    pub timing: PipelineTiming,
    pub symbol_count: usize,
    pub content_hash: String,
}

#[derive(Debug, Clone, Default)]
pub struct PipelineTiming {
    pub chunk: Duration,
    pub summarize: Duration,
    pub embed: Duration,
}

/// Process a [`SourceItem`] (already chunked by a tap) through the
/// summarize → embed → build VectorDocuments pipeline.
pub async fn process_item(
    item: &SourceItem,
    summarizer: &dyn Summarizer,
    embedder: &dyn Embedder,
) -> Result<PipelineResult> {
    let language = item.language.as_deref().unwrap_or("unknown");

    let symbols: Vec<SymbolChunk> = item
        .children
        .iter()
        .map(|c| SymbolChunk {
            name: c.name.clone(),
            kind: c.kind.clone(),
            body: c.content.clone(),
            signature: c.signature.clone(),
            doc_comment: None,
            start_line: c.start_line.unwrap_or(0),
            end_line: c.end_line.unwrap_or(0),
            references: c.references.clone(),
        })
        .collect();

    let symbol_count = symbols.len();

    let t1 = Instant::now();
    let file_input = FileSummaryInput {
        file_path: item.source_path.clone(),
        content: item.content.clone(),
        imports: vec![],
        language: language.to_string(),
    };

    let summary_result =
        summarize_file_and_symbols(summarizer, &file_input, &symbols, DEFAULT_TOKEN_THRESHOLD)
            .await?;
    let summarize_time = t1.elapsed();

    let t2 = Instant::now();
    let mut documents = Vec::new();

    let file_embedding = embedder.embed(&summary_result.file_summary).await?;
    documents.push(VectorDocument {
        id: document_id(&item.source_path, ChunkKind::File, None, &item.content),
        vector: file_embedding,
        summary: summary_result.file_summary.clone(),
        file_path: item.source_path.clone(),
        chunk_kind: ChunkKind::File,
        symbol_name: None,
        symbol_kind: None,
        start_line: None,
        end_line: None,
        language: Some(language.to_string()),
        content_hash: Some(item.content_hash.clone()),
        calls: None,
        called_by: None,
    });

    for (child, sym_result) in item
        .children
        .iter()
        .zip(summary_result.symbol_summaries.iter())
    {
        let embedding = embedder.embed(&sym_result.summary).await?;
        documents.push(VectorDocument {
            id: document_id(
                &item.source_path,
                ChunkKind::Symbol,
                Some(&child.name),
                &child.content,
            ),
            vector: embedding,
            summary: sym_result.summary.clone(),
            file_path: item.source_path.clone(),
            chunk_kind: ChunkKind::Symbol,
            symbol_name: Some(child.name.clone()),
            symbol_kind: Some(child.kind.clone()),
            start_line: child.start_line,
            end_line: child.end_line,
            language: Some(language.to_string()),
            content_hash: None,
            calls: Some(child.references.clone()),
            called_by: None,
        });
    }
    let embed_time = t2.elapsed();

    Ok(PipelineResult {
        documents,
        timing: PipelineTiming {
            chunk: Duration::ZERO,
            summarize: summarize_time,
            embed: embed_time,
        },
        symbol_count,
        content_hash: item.content_hash.clone(),
    })
}

/// Deterministic document ID: blake3 hash of (file_path, chunk_kind, symbol_name, content).
/// Ensures idempotent upserts — re-indexing the same file with the same
/// content produces the same IDs.
pub fn document_id(
    file_path: &str,
    chunk_kind: ChunkKind,
    symbol_name: Option<&str>,
    content: &str,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(file_path.as_bytes());
    hasher.update(chunk_kind.to_string().as_bytes());
    if let Some(name) = symbol_name {
        hasher.update(name.as_bytes());
    }
    hasher.update(content.as_bytes());
    hasher.finalize().to_hex()[..16].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tap::SourceChunk;
    use crate::testing::mock_embed::MockEmbedder;
    use crate::testing::mock_summarize::MockSummarizer;

    #[test]
    fn document_id_is_deterministic() {
        let a = document_id("src/main.rs", ChunkKind::File, None, "fn main() {}");
        let b = document_id("src/main.rs", ChunkKind::File, None, "fn main() {}");
        assert_eq!(a, b);
    }

    #[test]
    fn document_id_differs_by_path() {
        let a = document_id("src/a.rs", ChunkKind::File, None, "content");
        let b = document_id("src/b.rs", ChunkKind::File, None, "content");
        assert_ne!(a, b);
    }

    #[test]
    fn document_id_differs_by_kind() {
        let a = document_id("src/a.rs", ChunkKind::File, None, "content");
        let b = document_id("src/a.rs", ChunkKind::Symbol, Some("foo"), "content");
        assert_ne!(a, b);
    }

    #[test]
    fn document_id_differs_by_content() {
        let a = document_id("src/a.rs", ChunkKind::File, None, "version 1");
        let b = document_id("src/a.rs", ChunkKind::File, None, "version 2");
        assert_ne!(a, b);
    }

    fn sample_source_item() -> SourceItem {
        SourceItem {
            source_path: "src/main.rs".into(),
            content: "pub fn hello() {}\npub fn world() {}".into(),
            content_hash: "abc123".into(),
            language: Some("rust".into()),
            children: vec![
                SourceChunk {
                    name: "hello".into(),
                    kind: "function".into(),
                    content: "pub fn hello() {}".into(),
                    signature: Some("pub fn hello()".into()),
                    start_line: Some(1),
                    end_line: Some(1),
                    references: vec!["println".into()],
                },
                SourceChunk {
                    name: "world".into(),
                    kind: "function".into(),
                    content: "pub fn world() {}".into(),
                    signature: Some("pub fn world()".into()),
                    start_line: Some(2),
                    end_line: Some(2),
                    references: vec![],
                },
            ],
        }
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn process_item_with_children() {
        let summarizer = MockSummarizer::new();
        let embedder = MockEmbedder::new(8);
        let item = sample_source_item();

        let result = process_item(&item, &summarizer, &embedder).await.unwrap();

        assert_eq!(result.documents.len(), 3);
        assert_eq!(result.symbol_count, 2);

        let file_doc = result
            .documents
            .iter()
            .find(|d| d.chunk_kind == ChunkKind::File)
            .expect("should have file-level doc");
        assert_eq!(file_doc.file_path, "src/main.rs");
        assert_eq!(file_doc.content_hash.as_deref(), Some("abc123"));
        assert_eq!(file_doc.language.as_deref(), Some("rust"));
        assert!(!file_doc.summary.is_empty());
        assert_eq!(file_doc.vector.len(), 8);

        let sym_docs: Vec<_> = result
            .documents
            .iter()
            .filter(|d| d.chunk_kind == ChunkKind::Symbol)
            .collect();
        assert_eq!(sym_docs.len(), 2);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn process_item_no_children() {
        let summarizer = MockSummarizer::new();
        let embedder = MockEmbedder::new(8);
        let item = SourceItem {
            source_path: "linear://ENG-123".into(),
            content: "Fix the login bug".into(),
            content_hash: "def456".into(),
            language: None,
            children: vec![],
        };

        let result = process_item(&item, &summarizer, &embedder).await.unwrap();

        assert_eq!(result.documents.len(), 1);
        assert_eq!(result.symbol_count, 0);
        assert_eq!(result.documents[0].chunk_kind, ChunkKind::File);
        assert_eq!(result.documents[0].file_path, "linear://ENG-123");
        assert_eq!(result.documents[0].language.as_deref(), Some("unknown"));
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn process_item_ids_are_deterministic() {
        let summarizer = MockSummarizer::new();
        let embedder = MockEmbedder::new(8);
        let item = sample_source_item();

        let r1 = process_item(&item, &summarizer, &embedder).await.unwrap();
        let r2 = process_item(&item, &summarizer, &embedder).await.unwrap();

        for (d1, d2) in r1.documents.iter().zip(r2.documents.iter()) {
            assert_eq!(d1.id, d2.id);
        }
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn process_item_ids_are_unique() {
        let summarizer = MockSummarizer::new();
        let embedder = MockEmbedder::new(8);
        let item = sample_source_item();

        let result = process_item(&item, &summarizer, &embedder).await.unwrap();

        let ids: Vec<&str> = result.documents.iter().map(|d| d.id.as_str()).collect();
        let unique: std::collections::HashSet<&str> = ids.iter().copied().collect();
        assert_eq!(ids.len(), unique.len(), "duplicate IDs: {ids:?}");
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn process_item_symbol_metadata_propagates() {
        let summarizer = MockSummarizer::new();
        let embedder = MockEmbedder::new(8);
        let item = sample_source_item();

        let result = process_item(&item, &summarizer, &embedder).await.unwrap();

        let hello = result
            .documents
            .iter()
            .find(|d| d.symbol_name.as_deref() == Some("hello"))
            .expect("should have hello symbol");
        assert_eq!(hello.symbol_kind.as_deref(), Some("function"));
        assert_eq!(hello.start_line, Some(1));
        assert_eq!(hello.end_line, Some(1));
        assert_eq!(hello.calls.as_deref(), Some(&["println".to_string()][..]));
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn process_item_content_hash_on_file_doc_only() {
        let summarizer = MockSummarizer::new();
        let embedder = MockEmbedder::new(8);
        let item = sample_source_item();

        let result = process_item(&item, &summarizer, &embedder).await.unwrap();

        let file_doc = result
            .documents
            .iter()
            .find(|d| d.chunk_kind == ChunkKind::File)
            .unwrap();
        assert!(file_doc.content_hash.is_some());

        for sym in result
            .documents
            .iter()
            .filter(|d| d.chunk_kind == ChunkKind::Symbol)
        {
            assert!(sym.content_hash.is_none());
        }
    }
}
