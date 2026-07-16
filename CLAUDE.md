# Project Instructions for AI Agents

This file provides instructions and context for AI coding agents working on this project.

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:ca08a54f -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for persistent knowledge — do NOT use MEMORY.md files

## Session Completion

**When ending a work session**, you MUST complete ALL steps below. Work is NOT complete until `git push` succeeds.

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** - Create issues for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **PUSH TO REMOTE** - This is MANDATORY:
   ```bash
   git pull --rebase
   bd dolt push
   git push
   git status  # MUST show "up to date with origin"
   ```
5. **Clean up** - Clear stashes, prune remote branches
6. **Verify** - All changes committed AND pushed
7. **Hand off** - Provide context for next session

**CRITICAL RULES:**
- Work is NOT complete until `git push` succeeds
- NEVER stop before pushing - that leaves work stranded locally
- NEVER say "ready to push when you are" - YOU must push
- If push fails, resolve and retry until it succeeds
<!-- END BEADS INTEGRATION -->

## Build & Test

```bash
just test          # run all tests (~300)
just ci            # fmt-check + clippy (-D warnings) + test
just lint          # clippy only
just miri          # undefined behavior check via Miri (requires nightly)
just fmt           # format code
just build         # debug build
just release       # optimized release build
just run <args>    # run from source (e.g. `just run search "query"`)
```

Rust 1.96+ required (pinned via `rust-toolchain.toml`). Edition 2024.

### Miri (Undefined Behavior Checker)

`just miri` runs the test suite under [Miri](https://github.com/rust-lang/miri/) to detect undefined behavior, memory leaks, and pointer provenance issues. Miri interprets MIR (Rust's mid-level IR) and cannot execute FFI calls.

**When to add `#[cfg_attr(miri, ignore)]`** to a test:
- Uses `#[tokio::test]` — tokio's runtime requires kqueue/epoll (OS-level FFI)
- Calls tree-sitter (`Parser::new()`, `TreeSitterChunker`) — C library FFI
- Spawns processes (`Command::new()`) — requires `fork()` syscall
- Creates reqwest `Client` directly (not via mocks) — system TLS FFI

**Do NOT ignore** tests that use mock implementations (`MockEmbedder`, `MockVectorStore`, `MockSummarizer`) or the pure-Rust nidus store's conversion helpers — these run under Miri. (nidus's store *methods*, however, go through a tokio runtime, so those tests still need the tokio ignore above.)

Miri runs `cargo miri test`: wdpkr is pure Rust (the local store is the
pure-Rust `nidus` crate — no FFI, no bundled C/C++), so Miri builds the whole
crate. Tests needing OS-level FFI carry `#[cfg_attr(miri, ignore)]`. Miri runs in
CI as a separate job. If nightly breaks Miri temporarily, the CI job will fail but
won't block the main `check` job.

## Architecture Overview

wdpkr is a CLI tool that maintains a vector-search index of LLM-generated code summaries. Two commands:

- `wdpkr index [--full]` — walks repo, chunks with tree-sitter, summarizes via Anthropic Haiku, embeds via Voyage, upserts to the configured store (Turbopuffer or local nidus)
- `wdpkr search "<query>"` — embeds query, searches the configured store, returns tiered file+symbol JSON

```
src/
├── cli/          # Clap parsing + subcommand dispatch
├── config/       # 4-layer resolution: defaults → file → env → CLI flags
├── chunk/        # tree-sitter AST chunking (8 languages)
├── ai_providers/ # All model-backend adapters: voyage/openai/ollama (embed) + anthropic (summarize) + capability registry
├── http/         # Shared reqwest retry: RetryPolicy + send_with_retry (used by ai_providers + store)
├── summarize/    # Summarizer trait + prompt templates + big-file rollup + build_summarizer factory
├── embed/        # Embedder trait + build_embedder factory
├── store/        # VectorStore trait + Turbopuffer + nidus (local) adapters
├── search/       # Search orchestration + JSON/pretty output
├── indexer/      # Full pipeline: git diff → walk → chunk → summarize → embed → upsert
└── testing/      # Mocks (store, embedder, summarizer) + fixtures
```

Provider adapters live in one place (`ai_providers/`); the `embed` and `summarize` modules own their traits and a factory that consults `ai_providers::PROVIDERS` (a capability registry — `Embed`/`Summarize`) before dispatching. Voyage is embed-only by design. All HTTP adapters (AI providers and the Turbopuffer store) share `http::send_with_retry`: a reqwest client, bounded exponential-backoff retry on transient send errors and retryable statuses, configurable base URL for testing. The nidus store is the exception — a local, file-backed backend built on the pure-Rust [`nidus`](https://crates.io/crates/nidus) crate (no FFI, no bundled C/C++), wrapping a synchronous `Nidus` handle in `Arc<Mutex<_>>` + `spawn_blocking`.

## Conventions & Patterns

- **Trait-first design**: VectorStore, Embedder, Summarizer, Chunker are all traits with mock + real implementations
- **Config via `env_or` pattern**: `env_or_resolved(KEY, file_or_resolved(file_value, default))` — every field has a known env var, file key, and hardcoded default
- **Tests are mock-based**: no live API calls in the test suite. Integration tests create temp git repos with fixture source files
- **Commit style**: emoji prefix + short description (e.g. `🔍 search orchestration`)
- **Issue tracking**: `bd` (beads) — run `bd ready` for available work
- **Branch workflow**: one branch per issue or bundled epic, push for PR review
- **Error handling**: `anyhow` at binary boundary, traits return `anyhow::Result`
- **Async runtime**: `tokio` — `current_thread` for search (fast cold start), `multi_thread` for index

### wdpkr

This repo has a semantic codebase index via `wdpkr`. Use it to **locate feature areas by concept** — "where does commission logic live," "how is rate limiting implemented," "what does the PDF pipeline look like." Parse the JSON output; `path` and `summary` fields tell you where to look, then read the actual files.

#### Options

| Flag | Description |
|------|-------------|
| `--scope <path>` | Limit to subtree (repeatable: `--scope src/finance --scope src/annuity`) |
| `--filter <glob>` | Glob on result paths (repeatable, OR logic: `--filter "*.go" --filter "*schedule*"`) |
| `--tap <name>` | Limit to tap sources (repeatable: `files`, `linear`, `notion`). Default: all configured taps. (`--provider` is a deprecated alias) |
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
