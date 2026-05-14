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
