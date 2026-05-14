//! Per-file indexing pipeline: chunk → summarize → embed → build VectorDocuments.

use std::time::{Duration, Instant};

use anyhow::Result;

use crate::chunk::{Chunker, detect_language};
use crate::embed::Embedder;
use crate::store::{ChunkKind, VectorDocument};
use crate::summarize::rollup::{DEFAULT_TOKEN_THRESHOLD, summarize_file_and_symbols};
use crate::summarize::{FileSummaryInput, Summarizer};

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

/// Process a single file through the full indexing pipeline.
pub async fn process_file(
    file_path: &str,
    content: &str,
    chunker: &dyn Chunker,
    summarizer: &dyn Summarizer,
    embedder: &dyn Embedder,
) -> Result<PipelineResult> {
    let language = detect_language(file_path).unwrap_or("unknown");
    let content_hash = blake3::hash(content.as_bytes()).to_hex()[..16].to_string();

    // Chunk
    let t0 = Instant::now();
    let chunks = chunker.chunk(file_path, content, language)?;
    let chunk_time = t0.elapsed();
    let symbol_count = chunks.symbols.len();

    // Summarize
    let t1 = Instant::now();
    let file_input = FileSummaryInput {
        file_path: file_path.to_string(),
        content: content.to_string(),
        imports: chunks.imports.clone(),
        language: language.to_string(),
    };

    let summary_result = summarize_file_and_symbols(
        summarizer,
        &file_input,
        &chunks.symbols,
        DEFAULT_TOKEN_THRESHOLD,
    )
    .await?;
    let summarize_time = t1.elapsed();

    // Embed
    let t2 = Instant::now();
    let mut documents = Vec::new();

    // File-level document
    let file_embedding = embedder.embed(&summary_result.file_summary).await?;
    documents.push(VectorDocument {
        id: document_id(file_path, ChunkKind::File, None, content),
        vector: file_embedding,
        summary: summary_result.file_summary.clone(),
        file_path: file_path.to_string(),
        chunk_kind: ChunkKind::File,
        symbol_name: None,
        symbol_kind: None,
        start_line: None,
        end_line: None,
        language: Some(language.to_string()),
        content_hash: Some(content_hash.clone()),
    });

    // Symbol-level documents
    for (sym, sym_result) in chunks
        .symbols
        .iter()
        .zip(summary_result.symbol_summaries.iter())
    {
        let embedding = embedder.embed(&sym_result.summary).await?;
        documents.push(VectorDocument {
            id: document_id(file_path, ChunkKind::Symbol, Some(&sym.name), &sym.body),
            vector: embedding,
            summary: sym_result.summary.clone(),
            file_path: file_path.to_string(),
            chunk_kind: ChunkKind::Symbol,
            symbol_name: Some(sym.name.clone()),
            symbol_kind: Some(sym.kind.clone()),
            start_line: Some(sym.start_line),
            end_line: Some(sym.end_line),
            language: Some(language.to_string()),
            content_hash: None,
        });
    }
    let embed_time = t2.elapsed();

    Ok(PipelineResult {
        documents,
        timing: PipelineTiming {
            chunk: chunk_time,
            summarize: summarize_time,
            embed: embed_time,
        },
        symbol_count,
        content_hash,
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
    use crate::chunk::tree_sitter::TreeSitterChunker;
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

    #[tokio::test]
    async fn process_rust_file() {
        let chunker = TreeSitterChunker::new();
        let summarizer = MockSummarizer::new();
        let embedder = MockEmbedder::new(8);

        let content = r#"
use std::io;

pub fn hello() {
    println!("hi");
}

pub fn goodbye() {
    println!("bye");
}
"#;
        let result = process_file("src/greet.rs", content, &chunker, &summarizer, &embedder)
            .await
            .unwrap();

        // Should have: 1 file-level + 2 symbol-level documents
        assert!(
            result.documents.len() >= 2,
            "expected at least 2 docs, got {}",
            result.documents.len()
        );

        let file_doc = result
            .documents
            .iter()
            .find(|d| d.chunk_kind == ChunkKind::File)
            .expect("should have a file-level document");
        assert_eq!(file_doc.file_path, "src/greet.rs");
        assert_eq!(file_doc.language.as_deref(), Some("rust"));
        assert!(!file_doc.summary.is_empty());
        assert_eq!(file_doc.vector.len(), 8);

        let symbol_docs: Vec<_> = result
            .documents
            .iter()
            .filter(|d| d.chunk_kind == ChunkKind::Symbol)
            .collect();
        assert!(!symbol_docs.is_empty());
        for sym in &symbol_docs {
            assert!(sym.symbol_name.is_some());
            assert!(sym.start_line.is_some());
        }
    }

    #[tokio::test]
    async fn process_rust_file_has_timing() {
        let chunker = TreeSitterChunker::new();
        let summarizer = MockSummarizer::new();
        let embedder = MockEmbedder::new(8);

        let result = process_file(
            "src/greet.rs",
            "pub fn hello() {}",
            &chunker,
            &summarizer,
            &embedder,
        )
        .await
        .unwrap();

        assert!(result.timing.chunk < Duration::from_secs(5));
        assert!(result.timing.summarize < Duration::from_secs(5));
        assert!(result.timing.embed < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn process_unknown_language_file() {
        let chunker = TreeSitterChunker::new();
        let summarizer = MockSummarizer::new();
        let embedder = MockEmbedder::new(8);

        let result = process_file(
            "README.md",
            "# Hello World",
            &chunker,
            &summarizer,
            &embedder,
        )
        .await
        .unwrap();

        // File-level only, no symbols
        assert_eq!(result.documents.len(), 1);
        assert_eq!(result.documents[0].chunk_kind, ChunkKind::File);
        assert_eq!(result.symbol_count, 0);
    }

    #[tokio::test]
    async fn all_document_ids_are_unique() {
        let chunker = TreeSitterChunker::new();
        let summarizer = MockSummarizer::new();
        let embedder = MockEmbedder::new(8);

        let content = "pub fn a() {}\npub fn b() {}\npub fn c() {}";
        let result = process_file("src/lib.rs", content, &chunker, &summarizer, &embedder)
            .await
            .unwrap();

        let ids: Vec<&str> = result.documents.iter().map(|d| d.id.as_str()).collect();
        let unique: std::collections::HashSet<&str> = ids.iter().copied().collect();
        assert_eq!(ids.len(), unique.len(), "duplicate IDs found: {ids:?}");
    }
}
