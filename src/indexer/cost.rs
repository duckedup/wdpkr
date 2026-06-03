//! Cost estimation for `--dry-run` mode.
//!
//! Walks the repo, chunks every file (tree-sitter, local, free), builds
//! the actual prompt strings that would be sent to the summarizer, and
//! estimates token counts from those. No API calls, no credentials required.

use std::path::Path;

use anyhow::Result;
use owo_colors::{OwoColorize, Stream, Style};

use super::walk;
use crate::chunk::{Chunker, detect_language};
use crate::summarize::prompts::{self, SYSTEM_PROMPT};
use crate::summarize::rollup::estimate_tokens;
use crate::summarize::{FileSummaryInput, SymbolSummaryInput};
use crate::tap::SourceItem;

#[derive(Debug, Clone)]
pub struct DryRunReport {
    pub files_total: usize,
    pub files_with_symbols: usize,
    pub total_symbols: usize,
    pub total_file_chunks: usize,
    /// Linear issues contributing to the estimate (0 when the linear tap is
    /// not configured or has no credentials).
    pub linear_issues: usize,
    pub estimated_summarizer_input_tokens: usize,
    pub estimated_summarizer_output_tokens: usize,
    pub estimated_embed_tokens: usize,
    pub estimated_vectors: usize,
    pub estimated_cost_usd: f64,
}

/// Summarizer-input estimate for one document: the system prompt + file-level
/// prompt, plus the system prompt + prompt for each symbol. Shared by the file
/// walk and the Linear tap so both go through one estimation path.
fn estimate_summary_input_tokens(
    file_input: &FileSummaryInput,
    symbols: &[SymbolSummaryInput],
    system_prompt_tokens: usize,
) -> usize {
    let mut total = system_prompt_tokens + estimate_tokens(&prompts::file_user_message(file_input));
    for sym in symbols {
        total += system_prompt_tokens + estimate_tokens(&prompts::symbol_user_message(sym));
    }
    total
}

/// The Linear tap's contribution to a dry-run estimate: one file-level summary
/// per issue (comments are folded into the issue's content, so there are no
/// symbol sub-calls).
#[derive(Debug, Clone, Default)]
pub struct LinearEstimate {
    pub issues: usize,
    pub summarizer_input_tokens: usize,
}

/// Estimate the summarizer input for a set of Linear issue [`SourceItem`]s.
pub fn estimate_linear(items: &[SourceItem]) -> LinearEstimate {
    let system_prompt_tokens = estimate_tokens(SYSTEM_PROMPT);
    let mut input = 0;
    for item in items {
        let file_input = FileSummaryInput {
            file_path: item.source_path.clone(),
            content: item.content.clone(),
            imports: vec![],
            language: item.language.clone().unwrap_or_else(|| "text".to_string()),
        };
        input += estimate_summary_input_tokens(&file_input, &[], system_prompt_tokens);
    }
    LinearEstimate {
        issues: items.len(),
        summarizer_input_tokens: input,
    }
}

#[derive(Debug, Clone)]
pub struct ProviderRates {
    pub summarizer_input_per_1m: f64,
    pub summarizer_output_per_1m: f64,
    pub embed_per_1m: f64,
}

impl ProviderRates {
    pub fn for_models(summarizer_model: &str, embed_model: &str) -> Self {
        let (sum_in, sum_out) = match summarizer_model {
            m if m.contains("haiku") => (0.25, 1.25),
            m if m.contains("sonnet") => (3.0, 15.0),
            m if m.contains("opus") => (15.0, 75.0),
            _ => (0.25, 1.25),
        };
        let embed = match embed_model {
            "voyage-code-3" | "voyage-3-large" | "voyage-3" => 0.06,
            "voyage-3-lite" => 0.02,
            "text-embedding-3-large" => 0.13,
            "text-embedding-3-small" => 0.02,
            _ => 0.06,
        };
        Self {
            summarizer_input_per_1m: sum_in,
            summarizer_output_per_1m: sum_out,
            embed_per_1m: embed,
        }
    }
}

const ESTIMATED_SUMMARY_OUTPUT_TOKENS: usize = 100;
const PLACEHOLDER_FILE_SUMMARY: &str =
    "Module implementing domain logic for data processing and validation with error handling.";

pub fn dry_run(chunker: &dyn Chunker, root: &Path) -> Result<DryRunReport> {
    let files = walk::walk_files(root)?;
    let system_prompt_tokens = estimate_tokens(SYSTEM_PROMPT);

    let mut files_total = 0;
    let mut files_with_symbols = 0;
    let mut total_symbols = 0;
    let mut summarizer_input_tokens = 0;
    let mut total_chunks = 0;

    for file_path in &files {
        let rel_path = file_path
            .strip_prefix(root)
            .unwrap_or(file_path)
            .to_string_lossy();

        let content = match std::fs::read_to_string(file_path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let language = detect_language(&rel_path).unwrap_or("unknown");
        let chunks = match chunker.chunk(&rel_path, &content, language) {
            Ok(c) => c,
            Err(_) => continue,
        };

        files_total += 1;
        total_chunks += 1;

        let file_input = FileSummaryInput {
            file_path: rel_path.to_string(),
            content: content.clone(),
            imports: chunks.imports.clone(),
            language: language.to_string(),
        };

        let sym_inputs: Vec<SymbolSummaryInput> = chunks
            .symbols
            .iter()
            .map(|sym| SymbolSummaryInput {
                symbol_name: sym.name.clone(),
                symbol_kind: sym.kind.clone(),
                body: sym.body.clone(),
                signature: sym.signature.clone(),
                doc_comment: sym.doc_comment.clone(),
                file_path: rel_path.to_string(),
                file_summary: PLACEHOLDER_FILE_SUMMARY.to_string(),
            })
            .collect();

        summarizer_input_tokens +=
            estimate_summary_input_tokens(&file_input, &sym_inputs, system_prompt_tokens);

        if !chunks.symbols.is_empty() {
            files_with_symbols += 1;
            total_symbols += chunks.symbols.len();
            total_chunks += chunks.symbols.len();
        }
    }

    let summarizer_output_tokens = total_chunks * ESTIMATED_SUMMARY_OUTPUT_TOKENS;
    let embed_tokens = total_chunks * ESTIMATED_SUMMARY_OUTPUT_TOKENS;
    let estimated_vectors = total_chunks;

    Ok(DryRunReport {
        files_total,
        files_with_symbols,
        total_symbols,
        total_file_chunks: total_chunks,
        linear_issues: 0,
        estimated_summarizer_input_tokens: summarizer_input_tokens,
        estimated_summarizer_output_tokens: summarizer_output_tokens,
        estimated_embed_tokens: embed_tokens,
        estimated_vectors,
        estimated_cost_usd: 0.0,
    })
}

impl DryRunReport {
    /// Fold a Linear estimate into the totals (one summary + one embed + one
    /// vector per issue). Call before [`with_cost`](Self::with_cost).
    pub fn merge_linear(&mut self, est: LinearEstimate) {
        self.linear_issues = est.issues;
        self.estimated_summarizer_input_tokens += est.summarizer_input_tokens;
        self.estimated_summarizer_output_tokens += est.issues * ESTIMATED_SUMMARY_OUTPUT_TOKENS;
        self.estimated_embed_tokens += est.issues * ESTIMATED_SUMMARY_OUTPUT_TOKENS;
        self.estimated_vectors += est.issues;
    }

    pub fn with_cost(mut self, rates: &ProviderRates) -> Self {
        let sum_input_cost = self.estimated_summarizer_input_tokens as f64 / 1_000_000.0
            * rates.summarizer_input_per_1m;
        let sum_output_cost = self.estimated_summarizer_output_tokens as f64 / 1_000_000.0
            * rates.summarizer_output_per_1m;
        let embed_cost = self.estimated_embed_tokens as f64 / 1_000_000.0 * rates.embed_per_1m;
        self.estimated_cost_usd = sum_input_cost + sum_output_cost + embed_cost;
        self
    }

    pub fn display(&self, summarizer_model: &str, embed_model: &str) {
        println!("Dry run report:");
        println!(
            "  Files:              {}",
            self.files_total
                .if_supports_color(Stream::Stdout, |s| s.cyan())
        );
        println!(
            "  Files with symbols: {}",
            self.files_with_symbols
                .if_supports_color(Stream::Stdout, |s| s.cyan())
        );
        if self.linear_issues > 0 {
            println!(
                "  Linear issues:      {}",
                self.linear_issues
                    .if_supports_color(Stream::Stdout, |s| s.cyan())
            );
        }
        println!(
            "  Total symbols:      {}",
            self.total_symbols
                .if_supports_color(Stream::Stdout, |s| s.cyan())
        );
        println!(
            "  Total chunks:       {} (file + symbol)",
            self.total_file_chunks
                .if_supports_color(Stream::Stdout, |s| s.cyan())
        );
        println!(
            "  Estimated vectors:  {}",
            self.estimated_vectors
                .if_supports_color(Stream::Stdout, |s| s.cyan())
        );
        println!();
        println!("  Summarizer ({summarizer_model}):");
        let input_tok = format_tokens(self.estimated_summarizer_input_tokens);
        let output_tok = format_tokens(self.estimated_summarizer_output_tokens);
        println!(
            "    Input tokens:  ~{}",
            input_tok.if_supports_color(Stream::Stdout, |s| s.yellow())
        );
        println!(
            "    Output tokens: ~{}",
            output_tok.if_supports_color(Stream::Stdout, |s| s.yellow())
        );
        println!("  Embedder ({embed_model}):");
        let embed_tok = format_tokens(self.estimated_embed_tokens);
        println!(
            "    Tokens: ~{}",
            embed_tok.if_supports_color(Stream::Stdout, |s| s.yellow())
        );
        println!();
        let cost = format!("${:.4}", self.estimated_cost_usd);
        let cost_style = Style::new().green().bold();
        println!(
            "  Estimated cost: {}",
            cost.if_supports_color(Stream::Stdout, |s| s.style(cost_style))
        );
    }
}

fn format_tokens(n: usize) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::tree_sitter::TreeSitterChunker;

    #[test]
    fn provider_rates_haiku_voyage() {
        let rates = ProviderRates::for_models("claude-haiku-4-5-20251001", "voyage-code-3");
        assert_eq!(rates.summarizer_input_per_1m, 0.25);
        assert_eq!(rates.summarizer_output_per_1m, 1.25);
        assert_eq!(rates.embed_per_1m, 0.06);
    }

    #[test]
    fn provider_rates_sonnet_openai() {
        let rates = ProviderRates::for_models("claude-sonnet-4-6", "text-embedding-3-large");
        assert_eq!(rates.summarizer_input_per_1m, 3.0);
        assert_eq!(rates.embed_per_1m, 0.13);
    }

    #[test]
    fn cost_calculation() {
        let report = DryRunReport {
            files_total: 10,
            files_with_symbols: 8,
            total_symbols: 50,
            total_file_chunks: 60,
            linear_issues: 0,
            estimated_summarizer_input_tokens: 500_000,
            estimated_summarizer_output_tokens: 6_000,
            estimated_embed_tokens: 6_000,
            estimated_vectors: 60,
            estimated_cost_usd: 0.0,
        };
        let rates = ProviderRates::for_models("claude-haiku-4-5-20251001", "voyage-code-3");
        let with_cost = report.with_cost(&rates);

        // Summarizer: 500k/1M * 0.25 + 6k/1M * 1.25 = 0.125 + 0.0075 = 0.1325
        // Embedder: 6k/1M * 0.06 = 0.00036
        // Total ≈ 0.133
        assert!(
            with_cost.estimated_cost_usd > 0.1 && with_cost.estimated_cost_usd < 0.2,
            "cost: {}",
            with_cost.estimated_cost_usd
        );
    }

    #[test]
    fn estimate_linear_counts_and_tokens() {
        let items = vec![
            SourceItem {
                source_path: "linear://ENG-1".into(),
                content: "ENG-1 Fix login\n\nUsers cannot log in after refresh.".into(),
                content_hash: "h1".into(),
                language: None,
                module_doc: None,
                children: vec![],
            },
            SourceItem {
                source_path: "linear://ENG-2".into(),
                content: "ENG-2 Rate table\n\nWhy we chose monthly buckets.".into(),
                content_hash: "h2".into(),
                language: None,
                module_doc: None,
                children: vec![],
            },
        ];
        let est = estimate_linear(&items);
        assert_eq!(est.issues, 2);
        assert!(est.summarizer_input_tokens > 0);
    }

    #[test]
    fn merge_linear_folds_into_totals() {
        let mut report = DryRunReport {
            files_total: 1,
            files_with_symbols: 0,
            total_symbols: 0,
            total_file_chunks: 1,
            linear_issues: 0,
            estimated_summarizer_input_tokens: 1_000,
            estimated_summarizer_output_tokens: 100,
            estimated_embed_tokens: 100,
            estimated_vectors: 1,
            estimated_cost_usd: 0.0,
        };
        report.merge_linear(LinearEstimate {
            issues: 3,
            summarizer_input_tokens: 5_000,
        });
        assert_eq!(report.linear_issues, 3);
        assert_eq!(report.estimated_summarizer_input_tokens, 6_000);
        // 3 issues × ESTIMATED_SUMMARY_OUTPUT_TOKENS added to output + embed.
        assert_eq!(
            report.estimated_summarizer_output_tokens,
            100 + 3 * ESTIMATED_SUMMARY_OUTPUT_TOKENS
        );
        assert_eq!(
            report.estimated_embed_tokens,
            100 + 3 * ESTIMATED_SUMMARY_OUTPUT_TOKENS
        );
        assert_eq!(report.estimated_vectors, 1 + 3);
    }

    #[test]
    fn format_tokens_display() {
        assert_eq!(format_tokens(500), "500");
        assert_eq!(format_tokens(1_500), "1.5k");
        assert_eq!(format_tokens(500_000), "500.0k");
        assert_eq!(format_tokens(1_500_000), "1.5M");
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn dry_run_on_current_repo() {
        let chunker = TreeSitterChunker::new();
        let report = dry_run(&chunker, Path::new(".")).unwrap();

        assert!(report.files_total > 0, "should find files in current repo");
        assert!(report.total_symbols > 0, "should find symbols");
        assert!(report.estimated_summarizer_input_tokens > 0);
        assert!(report.estimated_vectors > 0);
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn dry_run_includes_prompt_overhead() {
        let chunker = TreeSitterChunker::new();
        let report = dry_run(&chunker, Path::new(".")).unwrap();

        // With prompt overhead, input tokens should be significantly more than
        // just the raw code. The system prompt alone is ~170 tokens per call,
        // and we have at least (files + symbols) calls.
        let min_overhead =
            (report.files_total + report.total_symbols) * estimate_tokens(SYSTEM_PROMPT);
        assert!(
            report.estimated_summarizer_input_tokens > min_overhead,
            "input tokens ({}) should exceed system prompt overhead alone ({})",
            report.estimated_summarizer_input_tokens,
            min_overhead
        );
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn dry_run_cost_on_current_repo() {
        let chunker = TreeSitterChunker::new();
        let report = dry_run(&chunker, Path::new(".")).unwrap();
        let rates = ProviderRates::for_models("claude-haiku-4-5-20251001", "voyage-code-3");
        let with_cost = report.with_cost(&rates);

        assert!(
            with_cost.estimated_cost_usd > 0.0,
            "cost should be positive for a real repo"
        );
        assert!(
            with_cost.estimated_cost_usd < 10.0,
            "cost should be reasonable for this small repo: ${}",
            with_cost.estimated_cost_usd
        );
    }
}
