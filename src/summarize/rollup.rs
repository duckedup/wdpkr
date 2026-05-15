//! Big-file roll-up: conditional summarization strategy for files
//! exceeding the token threshold.
//!
//! Normal flow: file summary first → symbols get file summary as context.
//! Roll-up flow: symbols first (with minimal context) → file summary
//! synthesized from symbol summaries.

use anyhow::Result;

use super::prompts;
use super::{FileSummaryInput, Summarizer, SymbolSummaryInput};
use crate::chunk::SymbolChunk;

/// Default token threshold. Files above this use the roll-up path.
/// Tunable per SPEC — the chars/4 estimation is rough but sufficient
/// for a threshold decision.
pub const DEFAULT_TOKEN_THRESHOLD: usize = 50_000;

/// Result of summarizing a file and all its symbols.
pub struct FileSummaryResult {
    pub file_summary: String,
    pub symbol_summaries: Vec<SymbolSummaryResult>,
}

pub struct SymbolSummaryResult {
    pub name: String,
    pub summary: String,
}

/// Estimate token count from text length. `chars / 4` per SPEC —
/// not accurate enough for billing, sufficient for threshold decisions.
pub fn estimate_tokens(text: &str) -> usize {
    text.len() / 4
}

/// Summarize a file and its symbols, automatically choosing the normal
/// or roll-up path based on the token threshold.
pub async fn summarize_file_and_symbols(
    summarizer: &dyn Summarizer,
    file_input: &FileSummaryInput,
    symbols: &[SymbolChunk],
    token_threshold: usize,
) -> Result<FileSummaryResult> {
    let estimated = estimate_tokens(&file_input.content);

    if estimated <= token_threshold {
        normal_flow(summarizer, file_input, symbols).await
    } else {
        rollup_flow(summarizer, file_input, symbols).await
    }
}

/// Normal path: file summary first, then symbols with file context.
async fn normal_flow(
    summarizer: &dyn Summarizer,
    file_input: &FileSummaryInput,
    symbols: &[SymbolChunk],
) -> Result<FileSummaryResult> {
    let file_summary = summarizer.summarize_file(file_input).await?;

    let mut symbol_summaries = Vec::with_capacity(symbols.len());
    for sym in symbols {
        let input = SymbolSummaryInput {
            symbol_name: sym.name.clone(),
            symbol_kind: sym.kind.clone(),
            body: sym.body.clone(),
            signature: sym.signature.clone(),
            doc_comment: sym.doc_comment.clone(),
            file_path: file_input.file_path.clone(),
            file_summary: file_summary.clone(),
        };
        let summary = summarizer.summarize_symbol(&input).await?;
        symbol_summaries.push(SymbolSummaryResult {
            name: sym.name.clone(),
            summary,
        });
    }

    Ok(FileSummaryResult {
        file_summary,
        symbol_summaries,
    })
}

/// Roll-up path: symbols first (minimal context), then file summary
/// synthesized from the symbol summaries.
async fn rollup_flow(
    summarizer: &dyn Summarizer,
    file_input: &FileSummaryInput,
    symbols: &[SymbolChunk],
) -> Result<FileSummaryResult> {
    let placeholder_context = format!(
        "{} file with {} imports",
        file_input.language,
        file_input.imports.len()
    );

    let mut symbol_summaries = Vec::with_capacity(symbols.len());
    for sym in symbols {
        let input = SymbolSummaryInput {
            symbol_name: sym.name.clone(),
            symbol_kind: sym.kind.clone(),
            body: sym.body.clone(),
            signature: sym.signature.clone(),
            doc_comment: sym.doc_comment.clone(),
            file_path: file_input.file_path.clone(),
            file_summary: placeholder_context.clone(),
        };
        let summary = summarizer.summarize_symbol(&input).await?;
        symbol_summaries.push(SymbolSummaryResult {
            name: sym.name.clone(),
            summary,
        });
    }

    // Roll up: synthesize file summary from symbol summaries.
    let pairs: Vec<(&str, &str)> = symbol_summaries
        .iter()
        .map(|s| (s.name.as_str(), s.summary.as_str()))
        .collect();
    let rollup_msg =
        prompts::rollup_user_message(&file_input.file_path, &file_input.language, &pairs);
    let file_summary = summarizer
        .summarize_file(&FileSummaryInput {
            file_path: file_input.file_path.clone(),
            content: rollup_msg,
            imports: file_input.imports.clone(),
            language: file_input.language.clone(),
        })
        .await?;

    Ok(FileSummaryResult {
        file_summary,
        symbol_summaries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::Import;
    use crate::testing::mock_summarize::MockSummarizer;

    fn small_file() -> FileSummaryInput {
        FileSummaryInput {
            file_path: "src/small.rs".into(),
            content: "fn hello() {}".into(), // 14 chars → ~3 tokens
            imports: vec![],
            language: "rust".into(),
        }
    }

    fn big_file(size: usize) -> FileSummaryInput {
        FileSummaryInput {
            file_path: "src/giant.rs".into(),
            content: "x".repeat(size),
            imports: vec![Import {
                module: "std".into(),
                names: vec![],
            }],
            language: "rust".into(),
        }
    }

    fn sample_symbols() -> Vec<SymbolChunk> {
        vec![
            SymbolChunk {
                name: "process".into(),
                kind: "function".into(),
                body: "fn process() {}".into(),
                signature: Some("fn process()".into()),
                doc_comment: None,
                start_line: 1,
                end_line: 1,
            },
            SymbolChunk {
                name: "validate".into(),
                kind: "function".into(),
                body: "fn validate() {}".into(),
                signature: Some("fn validate()".into()),
                doc_comment: None,
                start_line: 3,
                end_line: 3,
            },
        ]
    }

    #[test]
    fn estimate_tokens_chars_div_4() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("a".repeat(200_000).as_str()), 50_000);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn small_file_uses_normal_flow() {
        let s = MockSummarizer::new();
        let result = summarize_file_and_symbols(&s, &small_file(), &sample_symbols(), 50_000)
            .await
            .unwrap();

        // Normal flow: file summary from file content, symbols get file summary as context
        assert!(result.file_summary.contains("src/small.rs"));
        assert_eq!(result.symbol_summaries.len(), 2);
        assert_eq!(result.symbol_summaries[0].name, "process");
        // Symbol summary should reference the file summary (normal flow threads it)
        assert!(
            result.symbol_summaries[0]
                .summary
                .contains("File summary of")
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn big_file_uses_rollup_flow() {
        let s = MockSummarizer::new();
        // 200K chars → 50K tokens, threshold at 10K → triggers rollup
        let result = summarize_file_and_symbols(&s, &big_file(200_000), &sample_symbols(), 10_000)
            .await
            .unwrap();

        // Roll-up: file summary is derived from rollup prompt (contains symbol summaries)
        assert!(result.file_summary.contains("src/giant.rs"));
        assert_eq!(result.symbol_summaries.len(), 2);
        // In rollup flow, symbols get placeholder context instead of real file summary
        assert!(
            result.symbol_summaries[0]
                .summary
                .contains("rust file with 1 imports")
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn exact_threshold_uses_normal_flow() {
        let s = MockSummarizer::new();
        // 40K chars → 10K tokens, threshold at 10K → exactly at threshold → normal
        let result = summarize_file_and_symbols(&s, &big_file(40_000), &sample_symbols(), 10_000)
            .await
            .unwrap();
        // Should be normal flow (<=, not <)
        assert!(result.file_summary.contains("src/giant.rs"));
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn empty_symbols_returns_file_summary_only() {
        let s = MockSummarizer::new();
        let result = summarize_file_and_symbols(&s, &small_file(), &[], 50_000)
            .await
            .unwrap();
        assert!(!result.file_summary.is_empty());
        assert!(result.symbol_summaries.is_empty());
    }

    #[test]
    fn rollup_prompt_includes_symbol_summaries() {
        let msg = prompts::rollup_user_message(
            "src/big.rs",
            "rust",
            &[
                ("process", "Processes items."),
                ("validate", "Validates input."),
            ],
        );
        assert!(msg.contains("src/big.rs"));
        assert!(msg.contains("process: Processes items."));
        assert!(msg.contains("validate: Validates input."));
        assert!(msg.contains("too large"));
    }
}
