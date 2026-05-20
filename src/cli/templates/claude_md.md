
### wdpkr

This repo has a semantic codebase index via `wdpkr`. Use it to **locate feature areas by concept** — "where does commission logic live," "how is rate limiting implemented," "what does the PDF pipeline look like." Parse the JSON output; `path` and `summary` fields tell you where to look, then read the actual files.

#### Options

| Flag | Description |
|------|-------------|
| `--scope <path>` | Limit to subtree (repeatable: `--scope src/finance --scope src/annuity`) |
| `--filter <glob>` | Glob on result paths (repeatable, OR logic: `--filter "*.go" --filter "*schedule*"`) |
| `--terse` | Paths + one-sentence summaries, no symbols — minimal context cost |
| `--no-symbols` | File-level results only, omit symbol nesting |
| `-k, --top-k <N>` | Max file results (default 5). Use `-k 2` for precise hits |
| `--symbols-per-file <N>` | Max symbols per file (default 3) |
| `--pretty` | Human-readable colored output instead of JSON |

#### Call graph data

Symbol-level results include `calls` and `called_by` fields when the index has been built with call-graph support. Use these to assess blast radius before making changes:

- `"calls": ["src/finance/rates.rs:lookup_rate_table"]` — this symbol calls `lookup_rate_table` in `src/finance/rates.rs`
- `"called_by": ["src/api/handler.rs:process_request"]` — `process_request` depends on this symbol

A `null` value means the symbol hasn't been indexed with call-graph data yet (run `wdpkr index --skip-summaries` to rebuild). An empty array `[]` means the symbol genuinely has no callers or callees.

When changing a symbol, check its `called_by` to find all dependents — read those files to verify your change doesn't break callers. When exploring unfamiliar code, check `calls` to understand what a function depends on before diving into its implementation.

#### When to use

- **Conceptual questions** where you don't know what to grep for: "where does X live," "how is Y implemented"
- **Orientation** before touching an unfamiliar area — get the lay of the land first
- Combine `--scope` with `--filter` and `--terse` for fast, precise lookups:
  `wdpkr search "rate table" --scope src/finance --filter "*.go" --terse -k 3`

#### When NOT to use

- You have a concrete symbol or string to find — use `rg`/grep instead
- You already know which file to read — read it directly
- You need exact text matches or regex — wdpkr is semantic, not lexical

#### Best practices

- **Scope aggressively.** If you know the layer, `--scope` is more valuable than refining the query. Unscoped searches return results across all layers (UI, backend, infra), wasting result slots on irrelevant files.
- **Use `--terse` by default** for simple lookups. Full summaries and symbol trees are useful for deep exploration but waste context tokens when you just need to find the right file.
- **Combine `--scope` with `--filter`** to narrow both the search space and the result set. `--scope` limits the vector query (efficient); `--filter` prunes results by filename pattern (flexible).
- **Switch to `rg` after wdpkr points you somewhere.** Don't chain wdpkr queries to refine — once you have a file or symbol name, grep is faster.
- **Run scoped queries in parallel** when a question spans layers — e.g., one `--scope src/graphql` and one `--scope src/finance`.
