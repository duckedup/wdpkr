//! Deterministic [`Summarizer`] for tests and the eval harness.
//!
//! Returns predictable summaries derived from the input fields so tests
//! can verify pipeline behavior without hitting an LLM.

use anyhow::Result;
use async_trait::async_trait;

use crate::summarize::{FileSummaryInput, Summarizer, SymbolSummaryInput};

pub struct MockSummarizer;

impl MockSummarizer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MockSummarizer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Summarizer for MockSummarizer {
    async fn summarize_file(&self, input: &FileSummaryInput) -> Result<String> {
        Ok(format!(
            "File summary of {} ({} file with {} imports).",
            input.file_path,
            input.language,
            input.imports.len()
        ))
    }

    async fn summarize_symbol(&self, input: &SymbolSummaryInput) -> Result<String> {
        Ok(format!(
            "{} {} in {} — part of: {}",
            input.symbol_kind, input.symbol_name, input.file_path, input.file_summary
        ))
    }

    fn model_name(&self) -> &str {
        "mock-summarizer-v1"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::Import;

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn file_summary_is_deterministic() {
        let s = MockSummarizer::new();
        let input = FileSummaryInput {
            file_path: "src/main.rs".into(),
            content: "fn main() {}".into(),
            imports: vec![Import {
                module: "std".into(),
                names: vec![],
            }],
            language: "rust".into(),
        };
        let a = s.summarize_file(&input).await.unwrap();
        let b = s.summarize_file(&input).await.unwrap();
        assert_eq!(a, b);
        assert!(a.contains("src/main.rs"));
        assert!(a.contains("rust"));
        assert!(a.contains("1 imports"));
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn symbol_summary_includes_context() {
        let s = MockSummarizer::new();
        let input = SymbolSummaryInput {
            symbol_name: "release_payment".into(),
            symbol_kind: "function".into(),
            body: "pub fn release_payment() {}".into(),
            signature: None,
            doc_comment: None,
            file_path: "src/payments.rs".into(),
            file_summary: "Payment processing service.".into(),
        };
        let summary = s.summarize_symbol(&input).await.unwrap();
        assert!(summary.contains("release_payment"));
        assert!(summary.contains("function"));
        assert!(summary.contains("Payment processing service."));
    }

    #[test]
    fn model_name() {
        assert_eq!(MockSummarizer::new().model_name(), "mock-summarizer-v1");
    }
}
