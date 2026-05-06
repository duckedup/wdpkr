# search

The `megagrep search` engine.

## Purpose

Embed query → vector search → assemble tiered file+symbols JSON response.
Optimized for cold start: the agent invokes this many times per session.

## Public surface

- `pub struct SearchRun { config, embedder, store }`
- `pub async fn SearchRun::run(args: SearchArgs) -> anyhow::Result<SearchReport>`
- `pub struct SearchReport` — serializes to the JSON shape in root
  `SPEC.md` § *Default JSON shape*

## Files

- `mod.rs` — orchestration: embed query, call `store.search`, group results
  by file, attach top symbols, surface `indexed_at` (HWM) and `namespace`
- `output.rs` — JSON serialization + `--pretty` rendering (probably
  `serde_json` plus a small ANSI helper)

## Plan

Per root `SPEC.md` § *Searching*:

1. Resolve `Config`, build embedder + store from it.
2. Embed query.
3. Call `store.search` with:
   - `top_k = args.top_k * (args.symbols_per_file + 1) * over_fetch_factor`
   - `path_prefix = args.scope`
4. Group results: files ranked by file-summary score; symbols ranked
   within each file by symbol score.
5. Cap to `args.top_k` files × `args.symbols_per_file` symbols.
6. Always include `indexed_at` (HWM SHA) and `namespace` in the response.
7. **Embedder mismatch check**: if `NamespaceMetadata.embedder` differs from
   resolved `EmbedConfig`, exit with code 1 and a clear message. Same
   invariant as the indexer.
8. **Exit codes** per root SPEC § *Exit codes*:
   - `0` success, `2` backend transient (retryable), `3` index missing/empty.

## Open questions

- Over-fetch factor — `3×` is the starting heuristic; tune via eval.
- `--scope` semantics — current SPEC says path *prefix*; full glob support
  (`internal/finance/*`) is a possible v1.1 if eval shows users want it.
- Whether to emit `--pretty` to stdout or stderr. Stdout is conventional
  for `--pretty`; the JSON contract is for the agent path only.
