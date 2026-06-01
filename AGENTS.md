# AGENTS.md — Multi-agent coordination for wdpkr

This file describes how AI agents should interact with this codebase. It supplements `CLAUDE.md` (which covers project-specific conventions) with agent-coordination patterns.

## Who uses this codebase

wdpkr is built BY AI agents (Claude Code) and FOR AI agents (any agent that can shell out). The primary consumer of `wdpkr search` output is an AI coding agent that needs to orient itself in an unfamiliar codebase.

## Non-Interactive Shell Commands

**ALWAYS use non-interactive flags** with file operations to avoid hanging on confirmation prompts.

```bash
cp -f source dest           # NOT: cp source dest
mv -f source dest           # NOT: mv source dest
rm -f file                  # NOT: rm file
rm -rf directory            # NOT: rm -r directory
```

## Agent workflow

### Before starting work

```bash
bd prime              # Load beads workflow context
bd ready              # See what's available
bd show <id>          # Read issue details + dependencies
```

### Working on an issue

1. `git checkout main && git pull`
2. `git checkout -b <issue-id>`
3. Implement with tests — run `just ci` before committing
4. `git commit` with emoji-prefix message + `Co-Authored-By` trailer
5. `git push -u origin <branch>`
6. Flag for human review

### Quality gates

Every PR must pass `just ci` which runs:
- `cargo fmt --all -- --check` — formatting
- `cargo clippy --all-targets --all-features -- -D warnings` — linting
- `cargo test --all-features` — all tests (unit + integration)

### Test philosophy

- **Mock external APIs** — use `src/testing/mock_*.rs` for VectorStore, Embedder, Summarizer
- **Real tree-sitter** — chunking tests use the actual parser, not mocks
- **Temp git repos** — integration tests create fixture repos in `/tmp`
- **No live API calls in tests** — zero cost, deterministic, CI-safe

## Search output contract

Agents consuming `wdpkr search` output should parse the JSON and use:
- `results[].path` — file to read
- `results[].summary` — what the file does (for context, not ground truth)
- `results[].symbols[].name` — specific function/type to look at
- `results[].symbols[].lines` — line range to read
- `indexed_at` — HWM SHA, tells the agent how stale the index is

**Always read the actual file for ground truth.** wdpkr points and describes; it does not substitute for reading code.

## Key design constraints

- **Embed summaries, not code** — the vocabulary gap between user stories and code identifiers is closed by LLM-generated summaries, not by raw-code embedding
- **Single namespace per repo** — no commit-keyed namespaces, no historical search
- **CLI, not MCP** — any agent that can shell out can use wdpkr; auth stays in env vars
- **Pluggable backends** — every external dependency (vector store, embedder, summarizer, chunker) is behind a trait

## File layout reference

| Directory | What it contains |
|---|---|
| `src/cli/` | Clap parsing, subcommand dispatch, `templates/` for init |
| `src/config/` | 4-layer config resolution, `env_or` pattern |
| `src/chunk/` | Chunker trait, tree-sitter walker, per-language node maps |
| `src/summarize/` | Summarizer trait, Anthropic adapter, prompts, rollup |
| `src/embed/` | Embedder trait, Voyage/Ollama/OpenAI adapters |
| `src/store/` | VectorStore trait, Turbopuffer adapter |
| `src/search/` | SearchRun orchestration, JSON + pretty output |
| `src/indexer/` | IndexRun, git utils, repo walker, per-file pipeline |
| `src/testing/` | MockVectorStore, MockEmbedder, MockSummarizer, fixtures |
| `tests/` | Integration tests (search_e2e, index_search_e2e) |
| `eval/` | Golden-query eval cases (future) |

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
