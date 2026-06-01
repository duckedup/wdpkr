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
pub mod rollup;

use anyhow::{Result, bail};
use async_trait::async_trait;

use crate::ai_providers::{self, Capability};
use crate::chunk::Import;
use crate::config::SummarizerConfig;

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

/// Construct a [`Summarizer`] from the resolved config. Consults the provider
/// registry to reject non-summarizer providers, then dispatches to the
/// concrete adapter in [`crate::ai_providers`].
pub fn build_summarizer(config: &SummarizerConfig) -> Result<Box<dyn Summarizer>> {
    if !ai_providers::supports(&config.provider, Capability::Summarize) {
        bail!(
            "provider '{}' is not a valid summarizer; available summarizers: {}",
            config.provider,
            ai_providers::names_with(Capability::Summarize).join(", ")
        );
    }
    match config.provider.as_str() {
        "anthropic" => Ok(Box::new(ai_providers::anthropic::AnthropicSummarizer::new(
            config,
        )?)),
        other => bail!("summarizer provider '{other}' is not yet implemented"),
    }
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

    // ── Factory ───────────────────────────────────────────────────────

    #[test]
    fn build_summarizer_non_summarizer_provider_errors() {
        // OpenAI is an embedder, not a summarizer — the registry rejects it.
        let config = SummarizerConfig {
            provider: "openai".into(),
            model: "gpt-4".into(),
            api_key: "key".into(),
        };
        assert!(build_summarizer(&config).is_err());
    }

    #[test]
    fn build_summarizer_anthropic_without_key_errors() {
        let config = SummarizerConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5-20251001".into(),
            api_key: String::new(),
        };
        match build_summarizer(&config) {
            Ok(_) => panic!("should fail without API key"),
            Err(e) => assert!(e.to_string().contains("ANTHROPIC_API_KEY")),
        }
    }

    #[test]
    fn build_summarizer_anthropic_with_key_succeeds() {
        let config = SummarizerConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5-20251001".into(),
            api_key: "test-key".into(),
        };
        let s = build_summarizer(&config).unwrap();
        assert_eq!(s.model_name(), "claude-haiku-4-5-20251001");
    }
}
