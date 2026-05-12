//! Summarizer abstraction.
//!
//! Defines the [`Summarizer`] trait for generating natural-language
//! summaries of code chunks. The system's entire quality bet rides on
//! these summaries — they're what gets embedded and searched against.
//!
//! Two levels: file-level summaries first, then symbol-level summaries
//! that thread the file summary as context. This ordering constraint is
//! enforced by the indexer pipeline, not the trait itself.
//!
//! Implementation tracks root `SPEC.md` § Summarizer trait.

pub mod prompts;

use anyhow::Result;
use async_trait::async_trait;

use crate::chunk::Import;

// ── Trait ─────────────────────────────────────────────────────────────────

#[async_trait]
pub trait Summarizer: Send + Sync {
    /// Summarize a file given its content and metadata context.
    async fn summarize_file(&self, input: &FileSummaryInput) -> Result<String>;

    /// Summarize a symbol given its body and the parent file summary.
    async fn summarize_symbol(&self, input: &SymbolSummaryInput) -> Result<String>;

    /// Model name for cost tracking and logging.
    fn model_name(&self) -> &str;
}

// ── Input types ───────────────────────────────────────────────────────────

/// Input to [`Summarizer::summarize_file`]. Carries all the context the
/// summarizer needs to produce a dense, search-target-friendly file summary.
#[derive(Debug, Clone)]
pub struct FileSummaryInput {
    /// File path — e.g. `internal/finance/commission/release.go` is itself
    /// domain context for free.
    pub file_path: String,
    /// Full file content.
    pub content: String,
    /// Structured imports — signal of cross-file relationships.
    pub imports: Vec<Import>,
    /// Language name (e.g. "rust", "go").
    pub language: String,
}

/// Input to [`Summarizer::summarize_symbol`]. Carries the symbol body plus
/// the parent file summary as context — symbols summarized in isolation
/// produce weak generic summaries that embed poorly.
#[derive(Debug, Clone)]
pub struct SymbolSummaryInput {
    pub symbol_name: String,
    pub symbol_kind: String,
    /// Full text of the symbol, including any leading doc comment.
    pub body: String,
    /// For functions/methods: just the signature line(s).
    pub signature: Option<String>,
    /// Extracted doc comment text.
    pub doc_comment: Option<String>,
    /// File this symbol belongs to.
    pub file_path: String,
    /// The parent file-level summary — the key context that makes
    /// symbol-level summaries specific rather than generic.
    pub file_summary: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn _assert_send<T: Send>() {}
    fn _assert_sync<T: Sync>() {}

    #[test]
    fn trait_is_object_safe() {
        fn _takes_summarizer(_: &dyn Summarizer) {}
    }

    #[test]
    fn file_summary_input_construction() {
        let input = FileSummaryInput {
            file_path: "src/main.rs".into(),
            content: "fn main() {}".into(),
            imports: vec![Import {
                module: "std::io".into(),
                names: vec!["Read".into()],
            }],
            language: "rust".into(),
        };
        assert_eq!(input.file_path, "src/main.rs");
        assert_eq!(input.imports.len(), 1);
        assert_eq!(input.imports[0].module, "std::io");
    }

    #[test]
    fn symbol_summary_input_construction() {
        let input = SymbolSummaryInput {
            symbol_name: "process_payment".into(),
            symbol_kind: "function".into(),
            body: "pub fn process_payment() -> Result<()> { Ok(()) }".into(),
            signature: Some("pub fn process_payment() -> Result<()>".into()),
            doc_comment: Some("Processes a payment.".into()),
            file_path: "src/finance/payments.rs".into(),
            file_summary: "Payment processing and release logic.".into(),
        };
        assert_eq!(input.symbol_name, "process_payment");
        assert_eq!(input.file_summary, "Payment processing and release logic.");
        assert!(input.signature.is_some());
        assert!(input.doc_comment.is_some());
    }

    #[test]
    fn symbol_input_without_optional_fields() {
        let input = SymbolSummaryInput {
            symbol_name: "MAX_RETRIES".into(),
            symbol_kind: "const".into(),
            body: "const MAX_RETRIES: u32 = 3;".into(),
            signature: None,
            doc_comment: None,
            file_path: "src/config.rs".into(),
            file_summary: "Configuration constants.".into(),
        };
        assert!(input.signature.is_none());
        assert!(input.doc_comment.is_none());
    }
}
