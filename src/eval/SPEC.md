# eval

Retrieval-quality evaluation harness.

## Purpose

Loads golden-query cases from `eval/cases/`, runs each through
`search::SearchRun`, scores recall@5, recall@10, and MRR. Gates changes to
prompts, chunking, model choice, or any other retrieval-quality lever.

Per root `SPEC.md` § *Evaluation*: shipping the harness in v1 is mandatory —
without measurement, prompt and chunking changes can't be evaluated and
regressions go unnoticed.

## Public surface

- `pub struct EvalCase { id, query, relevant_paths }` — deserializable from
  `eval/cases/*.yaml`
- `pub struct CaseResult { case_id, recall_at_5, recall_at_10, mrr, top_paths }`
- `pub struct EvalSummary { mean_recall_at_5, mean_recall_at_10, mean_mrr, per_case }`
- `pub async fn run_eval(cases_dir: &Path) -> anyhow::Result<EvalSummary>`

## Files

- `mod.rs` — `run_eval` orchestrator; iterates cases, calls `SearchRun`,
  aggregates metrics
- `cases.rs` — case schema + loader
- `metrics.rs` — `recall_at_k`, `mrr` over a `Vec<SearchResult>` against the
  case's expected paths

## Plan

Per root `SPEC.md` § *Evaluation*:

1. ~10 seed cases sourced from real Linear tickets (root SPEC open Q #2).
2. Each case: `id`, `query` (ticket title or natural-language version),
   `relevant_paths` (load-bearing files when implemented).
3. Metrics: recall@5, recall@10, MRR — file-level only for v1.
4. Output: per-case scores + aggregate summary; suitable for CI gating.
5. Future (post-v1, root SPEC § *Future extensions*): synthetic-query
   generation, diff-based regression eval.

## Open questions

- **Exposure**: subcommand (`megagrep eval`) vs separate binary
  (`src/bin/eval.rs`) vs Rust integration test runner (`tests/eval.rs`).
  *Pending user decision — flagged in main conversation.*
- **Case file format**: YAML (matches config) vs TOML (Rust-native) vs JSON.
  Leaning YAML for editor-friendliness and consistency with the config file.
