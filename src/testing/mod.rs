//! Test infrastructure: mocks, fixtures, and helpers.
//!
//! This module is **not** gated on `#[cfg(test)]` — the eval harness
//! (`src/eval/`) needs to import mocks at library-compile-time. Binary
//! size cost is negligible; `strip = true` in the release profile removes
//! unused symbols.
//!
//! ## Contents
//!
//! - **Fixture helpers** (this file): `sample_document`, `sample_result` —
//!   reduce boilerplate in tests that need realistic-looking data.
//! - **Mock VectorStore** (`mock_store.rs`): in-memory, deterministic,
//!   real cosine similarity for faithful search ranking.
//! - **Mock Embedder** (`mock_embed.rs`, issue megagrep-zog): deterministic
//!   vectors for reproducible search tests.

pub mod mock_embed;
pub mod mock_store;
pub mod mock_summarize;

use crate::store::{ChunkKind, SearchResult, VectorDocument};

/// Build a [`VectorDocument`] with sensible defaults. Useful when a test
/// needs a document but doesn't care about most fields.
pub fn sample_document(file_path: &str, chunk_kind: ChunkKind) -> VectorDocument {
    VectorDocument {
        id: format!("test-{file_path}-{chunk_kind}"),
        vector: vec![0.1; 1024],
        summary: format!("Summary of {file_path}"),
        file_path: file_path.to_string(),
        chunk_kind,
        symbol_name: None,
        symbol_kind: None,
        start_line: None,
        end_line: None,
        language: Some("rust".into()),
    }
}

/// Build a symbol-level [`VectorDocument`] with name and line range.
pub fn sample_symbol_document(
    file_path: &str,
    symbol_name: &str,
    symbol_kind: &str,
    start_line: u32,
    end_line: u32,
) -> VectorDocument {
    VectorDocument {
        id: format!("test-{file_path}-{symbol_name}"),
        vector: vec![0.2; 1024],
        summary: format!("{symbol_kind} {symbol_name} in {file_path}"),
        file_path: file_path.to_string(),
        chunk_kind: ChunkKind::Symbol,
        symbol_name: Some(symbol_name.to_string()),
        symbol_kind: Some(symbol_kind.to_string()),
        start_line: Some(start_line),
        end_line: Some(end_line),
        language: Some("rust".into()),
    }
}

/// Build a [`SearchResult`] with sensible defaults.
pub fn sample_result(file_path: &str, score: f32, chunk_kind: ChunkKind) -> SearchResult {
    SearchResult {
        id: format!("test-{file_path}-{chunk_kind}"),
        score,
        file_path: file_path.to_string(),
        chunk_kind,
        symbol_name: None,
        symbol_kind: None,
        summary: format!("Summary of {file_path}"),
        start_line: None,
        end_line: None,
        language: Some("rust".into()),
    }
}

/// Build a symbol-level [`SearchResult`].
pub fn sample_symbol_result(
    file_path: &str,
    symbol_name: &str,
    symbol_kind: &str,
    score: f32,
    lines: (u32, u32),
) -> SearchResult {
    SearchResult {
        id: format!("test-{file_path}-{symbol_name}"),
        score,
        file_path: file_path.to_string(),
        chunk_kind: ChunkKind::Symbol,
        symbol_name: Some(symbol_name.to_string()),
        symbol_kind: Some(symbol_kind.to_string()),
        summary: format!("{symbol_kind} {symbol_name} in {file_path}"),
        start_line: Some(lines.0),
        end_line: Some(lines.1),
        language: Some("rust".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_document_file_level() {
        let doc = sample_document("src/main.rs", ChunkKind::File);
        assert_eq!(doc.file_path, "src/main.rs");
        assert_eq!(doc.chunk_kind, ChunkKind::File);
        assert!(doc.symbol_name.is_none());
        assert_eq!(doc.vector.len(), 1024);
        assert!(doc.id.contains("src/main.rs"));
    }

    #[test]
    fn sample_symbol_document_populates_symbol_fields() {
        let doc = sample_symbol_document("src/lib.rs", "run", "function", 10, 25);
        assert_eq!(doc.chunk_kind, ChunkKind::Symbol);
        assert_eq!(doc.symbol_name.as_deref(), Some("run"));
        assert_eq!(doc.symbol_kind.as_deref(), Some("function"));
        assert_eq!(doc.start_line, Some(10));
        assert_eq!(doc.end_line, Some(25));
    }

    #[test]
    fn sample_result_file_level() {
        let r = sample_result("src/main.rs", 0.87, ChunkKind::File);
        assert_eq!(r.file_path, "src/main.rs");
        assert_eq!(r.score, 0.87);
        assert!(r.symbol_name.is_none());
    }

    #[test]
    fn sample_symbol_result_populates_all_fields() {
        let r = sample_symbol_result("src/lib.rs", "dispatch", "function", 0.91, (42, 78));
        assert_eq!(r.symbol_name.as_deref(), Some("dispatch"));
        assert_eq!(r.start_line, Some(42));
        assert_eq!(r.end_line, Some(78));
        assert_eq!(r.score, 0.91);
    }

    #[test]
    fn ids_are_deterministic() {
        let a = sample_document("src/main.rs", ChunkKind::File);
        let b = sample_document("src/main.rs", ChunkKind::File);
        assert_eq!(a.id, b.id);

        let c = sample_document("src/lib.rs", ChunkKind::File);
        assert_ne!(a.id, c.id);
    }
}
