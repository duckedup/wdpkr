# indexer

Orchestrates the indexing pipeline: walk → chunk → summarize → embed → upsert.

## Purpose

The `megagrep index` engine. Reads the high-water mark (HWM) from store
metadata, computes the git diff, processes changed files concurrently,
advances the HWM on success.

## Public surface

- `pub struct IndexRun { config, chunker, summarizer, embedder, store }`
- `pub async fn IndexRun::run(args: IndexArgs) -> anyhow::Result<IndexReport>`
- `pub struct IndexReport { files_processed, files_skipped, chunks_upserted, hwm_advanced_to, estimated_cost_usd, errors }`

## Files

- `mod.rs` — top-level `IndexRun`, run loop, HWM advancement logic
- `walk.rs` — repo walker; `.gitignore` + `.megagrepignore`-aware
  (delegates to the `ignore` crate)
- `ignore.rs` — `.megagrepignore` parsing (gitignore syntax)
- `git.rs` — current SHA, `git diff HWM..HEAD`, `git remote get-url origin`
  for namespace derivation
- `pipeline.rs` — `chunk → summarize → embed → upsert` per file with bounded
  concurrency (`futures::stream::buffer_unordered`)
- `cost.rs` — `--dry-run` estimation; `--max-cost` enforcement against
  estimated tokens × per-provider rates

## Plan

Per root `SPEC.md` § *Indexing*:

1. **Bootstrap**: HWM missing → walk all files in scope.
   **Steady state**: HWM present → diff `HWM..HEAD`, process changed +
   added files; delete vectors for removed files via `store.delete_by_file`.
2. **Pipeline ordering**: file-level summary first per file, then symbol
   summaries thread the file summary as context. Symbol summaries embedded
   in the same batch as the file summary where possible.
3. **Per-file failures** logged + skipped; transient HTTP errors retry with
   exponential backoff *inside the embedder/summarizer/store adapters*.
4. **HWM advancement**: only when `successes / in_scope ≥
   indexer.hwm_success_threshold` (default 0.95). Allows forward progress on
   flaky API days while preventing the HWM from leaping past wholesale
   failures.
5. **Idempotency**: all upserts overwrite by deterministic ID. Replaying a
   partial run produces the correct end state.
6. **Embedder mismatch check**: read `NamespaceMetadata.embedder` at
   startup; if it differs from the resolved `EmbedConfig`, refuse incremental
   indexing — require `--full` or a config change. Hard error with a clear
   message (root SPEC § *CI-first configuration*).
7. **CLI flags**:
   - `--full` — ignore HWM, full reindex
   - `--dry-run` — count + estimate, no API calls
   - `--from <sha>` — override HWM as diff base
   - `--max-cost USD` — abort if estimated remaining cost exceeds cap
   - `--concurrency N` — bound parallel API calls

## Open questions

- Per-provider cost rate table — hardcoded with override env vars
  (`MEGAGREP_VOYAGE_PRICE_PER_MTOKEN` etc.) is the leading proposal.
  Provider rates change rarely; hardcoded values stay close to source of
  truth.
- Whether to emit a structured progress stream (JSON lines on stdout) for
  CI logs vs. just human-readable stderr. CI consumers do not currently
  parse it; leaning human-readable for now.
