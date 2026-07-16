//! Indexing pipeline: summarize → embed → build VectorDocuments.

use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use crate::chunk::SymbolChunk;
use crate::embed::Embedder;
use crate::indexer::docstring::{file_toc_text, symbol_embed_text};
use crate::indexer::prose::{
    has_code_structure, prose_doc_summary, prose_doc_text, prose_section_summary,
    prose_section_text,
};
use crate::store::{ChunkKind, VectorDocument};
use crate::summarize::rollup::{DEFAULT_TOKEN_THRESHOLD, summarize_file_and_symbols};
use crate::summarize::{FileSummaryInput, Summarizer};
use crate::tap::SourceItem;

/// What text gets embedded: LLM summaries, or code documentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedMode {
    Summary,
    Docstring,
}

impl EmbedMode {
    /// Parse from the resolved `embedder.embed_mode` config string.
    pub fn from_config(s: &str) -> Self {
        match s {
            "docstring" => EmbedMode::Docstring,
            _ => EmbedMode::Summary,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            EmbedMode::Summary => "summary",
            EmbedMode::Docstring => "docstring",
        }
    }
}

#[derive(Debug)]
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
/// embed → build VectorDocuments pipeline. In [`EmbedMode::Summary`] the
/// item is summarized by the LLM first; in [`EmbedMode::Docstring`] the
/// summarizer is skipped entirely and code documentation is embedded.
pub async fn process_item(
    item: &SourceItem,
    summarizer: Option<&dyn Summarizer>,
    embedder: &dyn Embedder,
    mode: EmbedMode,
) -> Result<PipelineResult> {
    let language = item.language.as_deref().unwrap_or("unknown");
    let symbol_count = item.children.len();

    // Determine, for the file and each child, the text to embed and the concise
    // summary to store/display (they differ only for prose, where we embed the
    // full text but store a short summary). Plus how long summarization took.
    let t1 = Instant::now();
    #[allow(clippy::type_complexity)]
    let (file_embed, file_summary, symbol_pairs): (String, String, Vec<(String, String)>) =
        match mode {
            EmbedMode::Summary => {
                let summarizer = match summarizer {
                    Some(s) => s,
                    None => bail!("summary embed mode requires a summarizer"),
                };
                let symbols: Vec<SymbolChunk> = item
                    .children
                    .iter()
                    .map(|c| SymbolChunk {
                        name: c.name.clone(),
                        kind: c.kind.clone(),
                        body: c.content.clone(),
                        signature: c.signature.clone(),
                        doc_comment: c.doc_comment.clone(),
                        start_line: c.start_line.unwrap_or(0),
                        end_line: c.end_line.unwrap_or(0),
                        references: c.references.clone(),
                    })
                    .collect();
                let file_input = FileSummaryInput {
                    file_path: item.source_path.clone(),
                    content: item.content.clone(),
                    imports: vec![],
                    language: language.to_string(),
                };
                let summary_result = summarize_file_and_symbols(
                    summarizer,
                    &file_input,
                    &symbols,
                    DEFAULT_TOKEN_THRESHOLD,
                )
                .await?;
                // Summary/docstring modes embed and display the same text.
                let symbol_pairs = summary_result
                    .symbol_summaries
                    .into_iter()
                    .map(|s| (s.summary.clone(), s.summary))
                    .collect::<Vec<_>>();
                (
                    summary_result.file_summary.clone(),
                    summary_result.file_summary,
                    symbol_pairs,
                )
            }
            EmbedMode::Docstring => {
                // Code items embed a docstring/signature TOC; prose items (notion,
                // linear, repo markdown) have no such metadata, so they embed their
                // raw text directly — still no LLM in the loop.
                if has_code_structure(item.module_doc.as_deref(), &item.children) {
                    let file_text = file_toc_text(
                        &item.source_path,
                        item.module_doc.as_deref(),
                        &item.children,
                    );
                    let symbol_pairs = item
                        .children
                        .iter()
                        .map(|c| {
                            let t = symbol_embed_text(
                                c.doc_comment.as_deref(),
                                c.signature.as_deref(),
                                &c.name,
                            );
                            (t.clone(), t)
                        })
                        .collect::<Vec<_>>();
                    (file_text.clone(), file_text, symbol_pairs)
                } else {
                    // Prose: embed the full text, but store a concise summary so
                    // search output shows the scored sections, not the whole doc.
                    let file_embed = prose_doc_text(&item.content);
                    let file_summary = prose_doc_summary(&item.content);
                    let symbol_pairs = item
                        .children
                        .iter()
                        .map(|c| {
                            (
                                prose_section_text(&c.name, &c.content),
                                prose_section_summary(&c.name, &c.content),
                            )
                        })
                        .collect::<Vec<_>>();
                    (file_embed, file_summary, symbol_pairs)
                }
            }
        };
    let summarize_time = t1.elapsed();

    let t2 = Instant::now();
    let mut documents = Vec::new();

    let file_embedding = embedder.embed(&file_embed).await?;
    documents.push(VectorDocument {
        id: document_id(&item.source_path, ChunkKind::File, None, &item.content),
        vector: file_embedding,
        summary: file_summary,
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

    for (child, (embed_text, summary)) in item.children.iter().zip(symbol_pairs) {
        let embedding = embedder.embed(&embed_text).await?;
        documents.push(VectorDocument {
            id: document_id(
                &item.source_path,
                ChunkKind::Symbol,
                Some(&child.name),
                &child.content,
            ),
            vector: embedding,
            summary,
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
            module_doc: None,
            children: vec![
                SourceChunk {
                    name: "hello".into(),
                    kind: "function".into(),
                    content: "pub fn hello() {}".into(),
                    signature: Some("pub fn hello()".into()),
                    doc_comment: None,
                    start_line: Some(1),
                    end_line: Some(1),
                    references: vec!["println".into()],
                },
                SourceChunk {
                    name: "world".into(),
                    kind: "function".into(),
                    content: "pub fn world() {}".into(),
                    signature: Some("pub fn world()".into()),
                    doc_comment: None,
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

        let result = process_item(&item, Some(&summarizer), &embedder, EmbedMode::Summary)
            .await
            .unwrap();

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
            module_doc: None,
            children: vec![],
        };

        let result = process_item(&item, Some(&summarizer), &embedder, EmbedMode::Summary)
            .await
            .unwrap();

        assert_eq!(result.documents.len(), 1);
        assert_eq!(result.symbol_count, 0);
        assert_eq!(result.documents[0].chunk_kind, ChunkKind::File);
        assert_eq!(result.documents[0].file_path, "linear://ENG-123");
        assert_eq!(result.documents[0].language.as_deref(), Some("unknown"));
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn process_item_docstring_mode_skips_summarizer() {
        let embedder = MockEmbedder::new(8);
        let mut item = sample_source_item();
        item.module_doc = Some("Greeting helpers.".into());
        item.children[0].doc_comment = Some("Says hello.".into());

        // No summarizer at all — docstring mode must not require one.
        let result = process_item(&item, None, &embedder, EmbedMode::Docstring)
            .await
            .unwrap();

        assert_eq!(result.documents.len(), 3);

        let hello = result
            .documents
            .iter()
            .find(|d| d.symbol_name.as_deref() == Some("hello"))
            .expect("hello symbol");
        assert_eq!(hello.summary, "Says hello.\npub fn hello()");

        // Undocumented symbol falls back to its signature.
        let world = result
            .documents
            .iter()
            .find(|d| d.symbol_name.as_deref() == Some("world"))
            .expect("world symbol");
        assert_eq!(world.summary, "pub fn world()");

        let file_doc = result
            .documents
            .iter()
            .find(|d| d.chunk_kind == ChunkKind::File)
            .unwrap();
        assert!(file_doc.summary.contains("Greeting helpers."));
        assert!(file_doc.summary.contains("pub fn hello()"));
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn process_item_docstring_mode_no_docs_does_not_error() {
        // The critical case: a file whose symbols have NO docstrings and no
        // module doc must still index successfully.
        let embedder = MockEmbedder::new(8);
        let item = sample_source_item(); // all doc_comments are None, module_doc None

        let result = process_item(&item, None, &embedder, EmbedMode::Docstring)
            .await
            .expect("undocumented code must not error");

        assert_eq!(result.documents.len(), 3);
        for doc in &result.documents {
            assert!(!doc.summary.is_empty(), "embed text must be non-empty");
            assert_eq!(doc.vector.len(), 8);
        }
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn process_item_docstring_mode_prose_embeds_content() {
        // A notion-like prose item (no language, sections without signatures)
        // must embed its raw prose, not just headings.
        let embedder = MockEmbedder::new(8);
        let item = SourceItem {
            source_path: "notion://abc".into(),
            content: "# Widget Spec\n\n## Cooling\nThe widget vents heat through the top fins."
                .into(),
            content_hash: "h".into(),
            language: None,
            module_doc: None,
            children: vec![SourceChunk {
                name: "Cooling".into(),
                kind: "section".into(),
                content: "The widget vents heat through the top fins.".into(),
                signature: None,
                doc_comment: None,
                start_line: None,
                end_line: None,
                references: vec![],
            }],
        };

        let result = process_item(&item, None, &embedder, EmbedMode::Docstring)
            .await
            .unwrap();

        let file_doc = result
            .documents
            .iter()
            .find(|d| d.chunk_kind == ChunkKind::File)
            .unwrap();
        // File vector embeds the doc prose (title + body), not a heading list.
        assert!(
            file_doc.summary.contains("Widget Spec"),
            "{}",
            file_doc.summary
        );

        let section = result
            .documents
            .iter()
            .find(|d| d.chunk_kind == ChunkKind::Symbol)
            .unwrap();
        // Section vector embeds the section CONTENT, not just its heading.
        assert!(
            section.summary.contains("vents heat through the top fins"),
            "section embedded only its heading: {}",
            section.summary
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn process_item_summary_mode_requires_summarizer() {
        let embedder = MockEmbedder::new(8);
        let item = sample_source_item();
        let err = process_item(&item, None, &embedder, EmbedMode::Summary)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("summarizer"));
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn process_item_ids_are_deterministic() {
        let summarizer = MockSummarizer::new();
        let embedder = MockEmbedder::new(8);
        let item = sample_source_item();

        let r1 = process_item(&item, Some(&summarizer), &embedder, EmbedMode::Summary)
            .await
            .unwrap();
        let r2 = process_item(&item, Some(&summarizer), &embedder, EmbedMode::Summary)
            .await
            .unwrap();

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

        let result = process_item(&item, Some(&summarizer), &embedder, EmbedMode::Summary)
            .await
            .unwrap();

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

        let result = process_item(&item, Some(&summarizer), &embedder, EmbedMode::Summary)
            .await
            .unwrap();

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

        let result = process_item(&item, Some(&summarizer), &embedder, EmbedMode::Summary)
            .await
            .unwrap();

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
