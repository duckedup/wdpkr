# Project Instructions for AI Agents

This file provides instructions and context for AI coding agents working on this project.

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:7510c1e2 -->
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

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/SYNC_CONCEPTS.md for details and anti-patterns.

## Session Completion

**When ending a work session**, you MUST complete ALL steps below. Work is NOT complete until `git push` succeeds.

**MANDATORY WORKFLOW:**

1. **File issues for remaining work** - Create issues for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **PUSH TO REMOTE** - This is MANDATORY:
   ```bash
   git pull --rebase
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

Rust 1.95+ required (pinned via `rust-toolchain.toml`). Edition 2024.

### Miri (Undefined Behavior Checker)

`just miri` runs the test suite under [Miri](https://github.com/rust-lang/miri/) to detect undefined behavior, memory leaks, and pointer provenance issues. Miri interprets MIR (Rust's mid-level IR) and cannot execute FFI calls.

**When to add `#[cfg_attr(miri, ignore)]`** to a test:
- Uses `#[tokio::test]` — tokio's runtime requires kqueue/epoll (OS-level FFI)
- Calls tree-sitter (`Parser::new()`, `TreeSitterChunker`) — C library FFI
- Spawns processes (`Command::new()`) — requires `fork()` syscall
- Creates reqwest `Client` directly (not via mocks) — system TLS FFI

**Do NOT ignore** tests that use mock implementations (`MockEmbedder`, `MockVectorStore`, `MockSummarizer`) — these are pure Rust and should run under Miri.

Miri runs in CI as a separate job. If nightly breaks Miri temporarily, the CI job will fail but won't block the main `check` job.

## Architecture Overview

wdpkr is a CLI tool that maintains a vector-search index of LLM-generated code summaries. Two commands:

- `wdpkr index [--full]` — walks repo, chunks with tree-sitter, summarizes via Anthropic Haiku, embeds via Voyage, upserts to Turbopuffer
- `wdpkr search "<query>"` — embeds query, searches Turbopuffer, returns tiered file+symbol JSON

```
src/
├── cli/          # Clap parsing + subcommand dispatch
├── config/       # 4-layer resolution: defaults → file → env → CLI flags
├── chunk/        # tree-sitter AST chunking (8 languages)
├── summarize/    # Anthropic adapter + prompt templates + big-file rollup
├── embed/        # Voyage / Ollama / OpenAI adapters
├── store/        # VectorStore trait + Turbopuffer adapter
├── search/       # Search orchestration + JSON/pretty output
├── indexer/      # Full pipeline: git diff → walk → chunk → summarize → embed → upsert
└── testing/      # Mocks (store, embedder, summarizer) + fixtures
```

All external API adapters share the same pattern: reqwest HTTP client, bounded exponential-backoff retry on 429/5xx, configurable base URL for testing.

## Conventions & Patterns

- **Trait-first design**: VectorStore, Embedder, Summarizer, Chunker are all traits with mock + real implementations
- **Config via `env_or` pattern**: `env_or_resolved(KEY, file_or_resolved(file_value, default))` — every field has a known env var, file key, and hardcoded default
- **Tests are mock-based**: no live API calls in the test suite. Integration tests create temp git repos with fixture source files
- **Commit style**: emoji prefix + short description (e.g. `🔍 search orchestration`)
- **Issue tracking**: `bd` (beads) — run `bd ready` for available work
- **Branch workflow**: one branch per issue or bundled epic, push for PR review
- **Error handling**: `anyhow` at binary boundary, traits return `anyhow::Result`
- **Async runtime**: `tokio` — `current_thread` for search (fast cold start), `multi_thread` for index
