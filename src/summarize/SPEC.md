# summarize

Summarizer trait + Anthropic adapter + prompt templates.

## Purpose

Generates the natural-language summaries that we then embed. The system's
entire quality bet rides on this module's prompts — any change here is
eval-gated.

## Public surface

The trait is finalized in root `SPEC.md` § *Summarizer trait*.

- `#[async_trait] pub trait Summarizer: Send + Sync`
  - `summarize_file(&self, input: &FileSummaryInput)`
  - `summarize_symbol(&self, input: &SymbolSummaryInput)`
  - `model_name`
- `pub struct FileSummaryInput { file_path, content, imports, language }`
- `pub struct SymbolSummaryInput { symbol_name, kind, body, signature, doc_comment, file_path, file_summary }`
- `pub struct AnthropicSummarizer`
- `pub fn build_summarizer(cfg: &SummarizerConfig) -> anyhow::Result<Box<dyn Summarizer>>`

## Files

- `mod.rs` — trait + factory + input types
- `anthropic.rs` — Anthropic Messages API adapter (HTTP via `reqwest`)
- `prompts.rs` — file-level + symbol-level prompt templates

## Plan

1. Trait + input types + factory.
2. Anthropic adapter using `claude-haiku-4-5-20251001` as the default model.
3. **Pipeline ordering invariant** (root SPEC § *Summarization*): file-level
   summaries must be produced first; symbol-level summaries thread the file
   summary as context. Symbols summarized in isolation produce weak
   embeddings.
4. Big-file handling (root SPEC § *Big-file handling*): for files exceeding
   ~50K tokens, the file-level summary is rolled up from the symbol summaries
   instead of passing the whole file to the model.
5. Oversized symbols (root SPEC § *Oversized symbol handling*): truncate body,
   keep `signature` + `doc_comment`, mark truncation in the prompt.

## Open questions

- The actual prompt wording — root SPEC open Q #1. Drafts go in `prompts.rs`;
  final wording is eval-driven.
- Token counter for the big-file threshold: `tiktoken-rs` works for OpenAI,
  not Claude. A `chars / 4` heuristic is probably good enough — the threshold
  is itself a tuning knob.
