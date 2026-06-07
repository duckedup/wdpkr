---
title: Evaluation
description: How wdpkr measures retrieval quality, the metrics it reports, and current baseline results.
---

wdpkr ships a retrieval eval harness so changes to chunking, embedding, or
indexing can be measured rather than guessed at. This page explains what it
measures, how to run it, and the current baseline numbers.

Because wdpkr's job is to **point and describe** — return the right file (and
symbol) near the top, not ship source into the context window — the metrics
focus on *ranked retrieval quality* and *context compression*.

## Running it

```bash
wdpkr eval                                   # default suite (eval/cases/wdpkr.json)
wdpkr eval eval/cases/wdpkr-deep.json        # a specific suite
wdpkr eval eval/cases/wdpkr-deep.json --tag keyword   # only cases with a tag
wdpkr eval --json                            # full machine-readable results
```

The harness embeds each case's query, runs a real search against the configured
index, and grades the results. It uses whatever index/embedder/store your config
points at — so numbers reflect *your* setup (embed mode, model, store), not a
fixture.

## Case format

Cases live in `eval/cases/*.json`. Each case is a query plus the ground truth it
should retrieve:

```json
{
  "label": "callgraph-nl",
  "query": "How are caller and callee edges resolved between symbols?",
  "expected_files": ["src/indexer/mod.rs"],
  "expected_symbols": ["resolve_call_edges"],
  "top_k": 5,
  "tags": ["nl", "callgraph"]
}
```

- **`expected_files`** — file-level ground truth (relevance is graded against these).
- **`expected_symbols`** — optional symbol-level ground truth, graded independently.
- **`tags`** — free-form labels; the harness aggregates metrics per tag, and `--tag` filters.
- Leave both expectations empty to grade compression only (useful for broad "overview" queries).

## Metrics

| Metric | What it measures |
|---|---|
| **Recall@k** | Fraction of expected files returned in the top *k*. "Did we find it at all?" |
| **MRR** | Mean reciprocal rank of the *first* expected file — `1/rank`. Rewards putting a relevant file at the top; recall@k alone can't see rank. |
| **First-hit rank** | 1-based rank of the first expected file (per case, in `--json`). |
| **Symbol recall / Symbol MRR** | Same idea, graded against `expected_symbols` over the flattened, rank-ordered symbol list across result files. |
| **Precision@k** | Found expected ÷ returned. Useful but **noisy when gold sets are small** — a tight, all-relevant cluster scores low if only one file is listed as "expected." Read it alongside MRR, not alone. |
| **Compression ratio** | Output tokens (the JSON result) ÷ tokens of the source files you'd otherwise read. Lower is better — it's how much context wdpkr saves. |

The CLI prints a per-case table, a summary, and a `By tag` breakdown; `--json`
adds per-case `relevance`, `symbol_relevance`, and a `by_tag` array.

## Current results

The numbers below are the `wdpkr-deep` suite (32 cases) run against wdpkr's own
codebase, indexed in **`--docstring` mode** — embeddings come from code
documentation + signatures with **no LLM summaries** — using **`voyage-code-3`**
into a local **nidus** store.

| Metric | Value |
|---|---|
| Recall@5 (file) | **0.95** |
| MRR (file) | **0.81** |
| Symbol recall | 0.60 |
| Symbol MRR | 0.27 |
| Compression ratio | 0.25 |

First-hit-rank distribution (file level): rank 1 ×21 · rank 2 ×7 · rank 3 ×1 ·
rank 5 ×1 · missed ×2. When the right file is found, it's in the top two-thirds
of the time.

### By query style

| Style | n | Recall@5 | MRR |
|---|---|---|---|
| keyword | 6 | 1.00 | 0.87 |
| where-is | 2 | 1.00 | 1.00 |
| natural-language | 20 | 0.97 | 0.84 |
| concept | 3 | 1.00 | 0.67 |
| symbol | 1 | 0.00 | 0.00 |

## Interpreting the baseline

- **File-level retrieval is strong and top-heavy.** ~95% recall and the answer
  usually at rank 1–2 — with no LLM in the indexing path. Docstring mode is a
  viable, zero-summarization-cost setup on a well-documented codebase.
- **Phrasing matters.** Keyword and "where is X" queries rank best; broad
  conceptual phrasings rank lower. In docstring mode, literal-token queries land
  on the implementation file directly, while natural-language queries tend to
  surface *callers* and conceptually-adjacent files.
- **Symbol-level precision is the weakest spot** (Symbol MRR 0.27 vs file MRR
  0.81): the correct *file* often ranks well before the correct *symbol*
  surfaces among the top symbols-per-file.

### Known limitations & areas to improve

- **Symbol ranking** is the biggest gap — worth exploring more symbols-per-file,
  better per-symbol doc embeddings, or within-file symbol re-ranking.
- **Prose competes with code.** Doc/Markdown files embed strongly for conceptual
  queries and can crowd out source; a chunk-kind weighting could keep code on top
  for code-intent queries.
- **Private / thinly-documented symbols** can fail to surface their file in
  docstring mode (nothing descriptive to embed).
- **Gold sets are hand-curated and incomplete**, which caps precision; the metric
  suite leans on MRR and symbol grading to compensate.

> These numbers are a snapshot of one configuration. They will shift with the
> embed mode (docstring vs summary), embedder/model, vector store, and the gold
> sets in the suite — re-run `wdpkr eval` against your own index to measure your
> setup.
