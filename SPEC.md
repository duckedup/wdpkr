# megagrep

> **Status:** Initial specification, draft for review.
> **Audience:** Engineers reviewing the design; coding agents that will work on or alongside this project.
> **Scope:** This document captures the architectural decisions made during initial planning. It is the starting point, not the final word — defaults and open questions are flagged as such.

## What it is

`megagrep` is a CLI tool that maintains a vector-search index of a codebase and exposes a single semantic search interface for AI coding agents. Its purpose is to reduce token usage and improve agent performance on tasks that require *orientation* in an unfamiliar codebase — finding where a feature lives, understanding what already exists in a domain area, identifying the right entry points before proposing changes.

It is explicitly **not** a replacement for `grep`/`ripgrep`. It is the conceptual layer on top of a codebase. Exact symbol or text lookup remains the job of grep and the IDE.

## Why this is worth building

Cursor published [internal benchmark results](https://turbopuffer.com/customers/cursor) showing that semantic search **plus** grep produced 23.5% better agent accuracy than grep alone on their Cursor Context Bench, with a 2.6% improvement in code retention on large codebases and a 2.2% reduction in dissatisfied user requests. Their setup uses Turbopuffer with a custom code-trained embedding model.

We can't replicate Cursor's custom embedding model. The architectural workaround — **embed LLM-generated natural-language summaries of code rather than the code itself** — closes the vocabulary gap between conceptual queries (e.g., a Linear ticket) and the code's actual identifiers, and makes off-the-shelf embedders viable.

## Use case (concrete)

An engineer is implementing a Linear ticket: *"As a Finance user, I want to release commission payments to individual payees or all at once, and manually correct commission amounts before release."*

Their AI coding agent runs `megagrep search "release commission payments to individual payees, manually correct amounts before release"` and gets back a tiered JSON response: the files where the commission system lives, the most relevant symbols within each file, and dense natural-language summaries describing what each does. The agent uses this to orient, then reads the actual files (via its existing file-read tools) for ground truth before proposing changes.

## Goals & Non-goals

### Goals (v1)

- Conceptual codebase search via vector retrieval over LLM-generated summaries
- CI-triggered indexing on every merge to main
- Local CLI usable from any agent harness (Claude Code, Cursor, Aider, manual)
- Pluggable backends (vector store, embedder, summarizer, chunker) — designed in, single implementations shipped
- Fast cold start (the agent calls this many times per session)
- Evaluation harness for measuring retrieval quality

### Non-goals (v1) — see also "Explicit punts" at end

- Not a grep replacement
- Not authoritative about current code state — the agent reads files for ground truth
- Not retaining historical commit indexes — single namespace per repo, updated in place
- Not implementing every backend the abstractions allow for

---

## Architecture

### Components

| Component   | Role                                                  | v1 Implementation                       |
|-------------|-------------------------------------------------------|------------------------------------------|
| Indexer     | Walks repo, summarizes, embeds, upserts vectors       | `megagrep index` subcommand              |
| Searcher    | Embeds query, queries store, returns tiered JSON      | `megagrep search` subcommand             |
| Config      | User-facing config management                          | `megagrep config` subcommand             |
| Vector store| Stores embeddings, runs ANN search                     | Turbopuffer (interface designed for swap)|
| Embedder    | Embeds natural-language summaries                      | OpenAI `text-embedding-3-large`          |
| Summarizer  | Generates NL summaries of code chunks                  | Anthropic Claude Haiku                   |
| Chunker     | Splits files into file-level + symbol-level chunks    | tree-sitter (per-language grammars)      |

### High-level data flow

**Indexing (CI on merge to main):**
1. CI runs `megagrep index`.
2. Indexer reads the high-water mark (HWM) — the SHA last successfully indexed — from turbopuffer namespace metadata.
3. Computes `git diff HWM..HEAD` for changed/added/deleted files.
4. For each changed file: chunk → summarize (file-level first, then symbol-level with file summary as context) → embed summaries → upsert to turbopuffer.
5. For each deleted file: delete its vectors.
6. On success, update HWM to HEAD as the final write.

**Searching (local, agent-invoked):**
1. Agent runs `megagrep search "<query>"`.
2. Searcher embeds the query.
3. Issues a vector query against the namespace, returns top-K file matches.
4. For each top file, returns the top symbol matches within it (auto-tiered server-side).
5. Returns JSON with paths, summaries, line ranges, scores.

### Namespace model

**One namespace per repo, updated in place.** No commit-keyed namespaces, no retention policy.

- Namespace name auto-derived from `git remote get-url origin` (normalized + hashed); overrideable in config.
- HWM stored as namespace metadata: a SHA fits in the kilobyte-scale metadata limit easily.
- The "search target" is conceptually "the current state of main." Agents on feature branches accept that summaries may be slightly stale relative to their working tree; source of truth remains the actual files, which the agent reads.

This is a deliberate simplification from "namespace per commit." The cost of statelessness (no historical search, can't reproduce a search against an old commit) was judged not worth the complexity for the stated use case.

---

## Indexing

### Scope (what gets indexed)

- **Inclusion rule:** any file not excluded by `.gitignore` or `.megagrepignore`.
- `.gitignore` covers vendored deps, generated code, build artifacts, etc.
- `.megagrepignore` covers committed-but-noisy files: lockfiles (`go.sum`, `package-lock.json`), generated protobuf/gRPC code, snapshot test outputs, large fixtures. None are gitignored, none are useful for conceptual search, all would dilute retrieval if indexed.

### Granularity: hierarchical

Two layers of chunks per indexable file (where supported):

| Language          | File-level | Symbol-level |
|-------------------|------------|--------------|
| Go                | ✅          | ✅            |
| Rust              | ✅          | ✅            |
| TypeScript / JavaScript | ✅    | ✅            |
| Svelte            | ✅          | ❌            |
| Everything else   | ✅          | ❌            |

Symbol-level chunking uses tree-sitter, with grammars per language. Svelte's multi-section structure (script + template + style) doesn't fit the symbol model cleanly; v1 treats `.svelte` files as file-level only. Same fallback for any unsupported language: file-level summary still works fine without a tree-sitter grammar.

### Summarization

The core quality bet of the system: we embed **LLM-generated natural-language summaries**, not raw code. Rationale: off-the-shelf code embedders are mediocre on conceptual queries (the vocabulary gap between user-story English and code identifiers); LLM summaries close that gap by translating code into the same register as queries.

**File-level prompt input:**
- File content
- File path (e.g., `internal/finance/commission/release.go` is itself domain context for free)
- File's imports (signal of cross-file relationships)

**Symbol-level prompt input:**
- Symbol body (function, type, etc.)
- The file-level summary, threaded in as context

This implies a pipeline ordering constraint: **file-level summaries are produced first, then symbol-level summaries** consume them as context. Symbols summarized in isolation produce weak generic summaries that embed poorly.

**Prompt style:** dense, search-target-friendly prose. Optimized for matching against user-story-shaped queries, not for explaining code to a developer. Prompts will be iterated with eval cases before locking in.

**Big-file handling:** for files exceeding a threshold (~50K tokens, default-tunable), roll up the file-level summary from the symbol summaries rather than passing the whole file to the summarizer in one call.

### Embedding

- Embed the **summaries**, not the raw code.
- Because the inputs to the embedder are natural language prose, code-specialized embedders are not necessary. `text-embedding-3-large` handles this well.
- Same embedder for indexing and querying.

### Change detection: high-water mark

The indexer maintains a single SHA in turbopuffer namespace metadata: the last-successfully-indexed commit on main.

- **Steady state:** read HWM, diff `HWM..HEAD`, process changed files, advance HWM.
- **Bootstrap (first run on a repo):** HWM missing → walk the entire repo, summarize and embed everything in scope, set HWM.
- **Manual reindex:** `--full` flag ignores the HWM and rebuilds from scratch.
- **Recovery:** `--from <sha>` overrides the HWM as the diff base. Expected to be rare.

### Failure handling

- Per-file failures (timeout, rate limit, malformed input) are logged to stderr and skipped — they don't kill the run.
- Transient errors retry with exponential backoff.
- The HWM advances only if a meaningful proportion of in-scope files succeeded (default threshold ~95%); this allows forward progress on flaky API days while preventing the HWM from advancing past wholesale failures.
- All upserts are idempotent — replaying a partially-failed run is safe and produces the correct end state.

### Cost controls

- `--dry-run`: walks the repo, counts files and chunks that would be processed, estimates cost, makes no API calls. Required pre-flight for bootstrap on large repos.
- `--max-cost USD`: hard cap; indexer aborts cleanly if estimated remaining cost would exceed the cap. Default ~$50 (tunable).
- `--concurrency N`: bounds parallel API calls. Default ~8.

### CI integration

- Triggered by merge to main via GitHub Actions (or equivalent).
- `concurrency: { group: megagrep-${repo}, cancel-in-progress: true }` — when a new merge lands, kill any running indexer for stale state. We don't want to write embeddings derived from a SHA that's already been superseded.
- Auth: `TURBOPUFFER_API_KEY`, `OPENAI_API_KEY`, `ANTHROPIC_API_KEY` injected as secrets.

---

## Searching

### Tool surface: CLI, not MCP

Megagrep is a CLI tool the agent invokes via shell, not an MCP server. Reasoning:

- Auth stays local — env vars on the developer's machine, secrets in CI. The agent never handles credentials.
- Universal — any agent that can shell out can use it (Claude Code, Cursor, Aider, scripts, manual).
- Lower implementation barrier than MCP, no protocol overhead, fast cold start.
- Output is JSON to stdout. Errors and diagnostics go to stderr.

### Subcommands

```
megagrep config
megagrep search
megagrep index
```

### `megagrep search`

```
megagrep search <query> [flags]

  -k, --top-k N              max file-level results (default: 5)
      --symbols-per-file N   max symbols per file (default: 3)
      --no-symbols           file-level only, omit symbol nesting
      --scope <path>         limit search to a subtree (e.g., "internal/finance/")
      --pretty               human-readable output (default: JSON)
```

**Auto-tiered output:** a single call returns file-level results with the top symbol matches nested inside each file. The agent does not have to make two round trips to orient + drill.

**No raw code in results.** The agent reads the actual file via its existing file-read tools. Megagrep's job is to point and describe, not to ship source into the context window.

**Default JSON shape:**

```json
{
  "query": "release commission payments to individual payees",
  "namespace": "dayforward-platform",
  "indexed_at": "abc123def456",
  "results": [
    {
      "path": "internal/finance/commission/release.go",
      "score": 0.87,
      "summary": "Service for releasing commission payments to payees, with support for individual or batch release; allows manual amount overrides before final release.",
      "symbols": [
        {
          "name": "ReleasePayeeCommission",
          "kind": "function",
          "lines": [42, 78],
          "summary": "Releases commission for a specified payee with optional amount override.",
          "score": 0.91
        }
      ]
    }
  ]
}
```

- `indexed_at` is the HWM. Tells the agent how stale the index is and lets it decide whether to verify with grep.
- `kind` on symbols (function, method, struct, type) gives the agent quick filtering signals.
- File results are ordered by file-summary score; symbols ordered by symbol score within each file.

### Exit codes

| Code | Meaning                              | Agent behavior            |
|------|--------------------------------------|---------------------------|
| 0    | Success                              | Use results               |
| 1    | Configuration error                  | Surface to user, no retry |
| 2    | Backend error (transient)            | Retry                     |
| 3    | Index missing or empty               | Surface to user, fall back to grep |

The exit-code distinction matters: an agent seeing exit 3 should *not* retry; it's a setup problem. Exit 2 is transient. The agent's behavior is unambiguous from the exit code alone.

### `megagrep index`

```
megagrep index [flags]

  --full              ignore high-water mark, full reindex
  --dry-run           estimate cost, no API calls, no writes
  --concurrency N     bound parallel API calls (default ~8)
  --from <sha>        override starting SHA (manual recovery)
  --max-cost USD      hard cost cap
```

### `megagrep config`

`git config`-style management with dotted keys (`store.provider`, `embedder.model`, `summarizer.model`, `default_branch`).

```
megagrep config init                  # write default ~/.config/megagrep/config.yaml
megagrep config get <key>
megagrep config set <key> <value>
megagrep config list                  # show effective config and source of each value
megagrep config edit                  # open in $EDITOR
megagrep config path                  # print resolved config file location
```

`config list` showing the resolution chain (which value came from which source — env, file, default) is the debugging affordance.

---

## Configuration

### Locations

- **Primary:** `~/.config/megagrep/config.yaml`, file mode 0600.
- **Repo-resident:** `.megagrepignore` is the only repo-level config file. (No per-repo `megagrep.yaml`; the user-level config plus auto-derivation handles the rest.)
- **Env vars:** override config file values. Primarily for secrets.

This pattern matches `gh`, `gcloud`, and `aws` CLI conventions: user config in `~/.config/<tool>/`, env vars as overrides. Familiar territory.

### Resolution order

```
defaults  →  config file  →  env vars
```

Later sources override earlier ones.

### Pluggable backends

Four interfaces, each with a single v1 implementation:

```
internal/store/        # Vector store interface;       turbopuffer adapter
internal/embed/        # Embedder interface;           OpenAI adapter
internal/summarize/    # Summarizer interface;         Anthropic adapter
internal/chunk/        # Chunker interface;            tree-sitter adapter
```

**Design principle:** define the abstractions from day one, but only ship one implementation of each. The second backend is what surfaces the leaky parts of the first interface; doing it without a real driver is a guessing game and a maintenance burden with no validating users.

---

## Agent integration

`megagrep init` writes (or appends) a section to the repo's `CLAUDE.md` file (or equivalent for other agents) telling the agent when and when not to use megagrep. The instruction text matters more than people think.

**Reference instruction text (subject to iteration):**

> ### megagrep
>
> This repo has a semantic codebase index available via `megagrep`. Use it when you need to **orient yourself on a feature area or conceptual question** before making changes — e.g., "where does the commission system live," "how is rate limiting implemented," "what does our PDF generation pipeline look like."
>
> Run `megagrep search "<query>"` and parse the JSON output. The `path` and `summary` fields tell you where to look; read the actual files for ground truth.
>
> **Don't use megagrep for:** exact symbol or text lookup (use `rg`/grep), reading file contents (read files directly), or lookups where you already know the file. megagrep is the conceptual layer; grep is still the right tool when you know what string you're searching for.

Megagrep's clean exit codes let the agent fall back gracefully: exit 3 (index missing) is an unambiguous signal to drop back to grep.

### `megagrep init`

Interactive — user can skip individual steps:

1. Write `CLAUDE.md` instruction section (or equivalent agent integration file).
2. Write `.megagrepignore` with sensible defaults.
3. Write `.github/workflows/megagrep.yml` (the CI workflow scaffold — checkout, install megagrep, run `megagrep index`, with the right `concurrency:` group). This is identical for every repo and writing it by hand is annoying; auto-generating it saves users the tedious bit.

---

## Implementation

### Language: Rust

Honest reasoning (the perf claim is more nuanced than it first appears):

- The searcher's hot path is I/O bound (parse args → embed query → query turbopuffer → format JSON). Rust vs. Go is approximately a wash on the actual hot path. Where Rust meaningfully wins is **cold start and binary size** — for a tool the agent invokes 10-30 times per session, this matters in aggregate.
- The indexer is bottlenecked on embedding/summarization API latency, not local compute. Both languages handle the parallel-HTTP pattern fine.
- Real wins for Rust on this project: tree-sitter integration is genuinely first-class in Rust; single-binary distribution + fast cold start matter on the hot path; the type system pulls weight for a tool with many fallible operations against three external APIs.
- Honest cost: slower to write than Go for the same outcome. For a side project with learning value, an acceptable trade.

### Stack

| Concern               | Crate                                  |
|-----------------------|----------------------------------------|
| Async runtime         | `tokio` (multi-threaded for indexer; `current_thread` for searcher) |
| HTTP                  | `reqwest` with `rustls-tls`            |
| JSON                  | `serde` + `serde_json`                 |
| CLI parsing           | `clap` (derive macros)                 |
| Tree-sitter           | `tree-sitter` + `tree-sitter-{go,rust,typescript,javascript,svelte}` |
| Token counting        | `tiktoken-rs`                          |
| Bounded parallelism   | `futures::stream::buffer_unordered`    |
| Errors                | `thiserror` (libs) + `anyhow` (binary boundary) |

No official Rust SDKs from turbopuffer, OpenAI, or Anthropic — all three are HTTP APIs cleanly hit with `reqwest`. This is also good for the abstraction layer: the `Store` / `Embedder` / `Summarizer` traits stay honest about what they need.

---

## Evaluation

### Why this is in v1

The system's entire value proposition is retrieval quality on conceptual queries. Without measurement: prompt changes can't be evaluated, the file-vs-symbol balance can't be tuned, regressions go unnoticed for weeks. The Cursor team has [written about](https://cursor.com/blog/semsearch) their internal benchmark (Cursor Context Bench) for the same reason — they couldn't iterate on retrieval quality without it.

### Approach

**Golden-query eval set:** 20–50 real Linear tickets from Dayforward's history, each annotated with the file paths that turned out to be relevant when implemented. The data is available — `git log --grep="LINEAR-XXX"` plus the ticket text gives both ends.

**Metrics:** recall@5, recall@10, MRR.

**Lifecycle:** lives in the repo, runs as a CI check or local script, gates changes to prompts, chunking, or model choices. Ship the *harness* in v1 with ~10 seed cases; grow the case set over time as queries surface where megagrep underperforms.

### Future extensions (post-v1)

- Synthetic queries from real code (ask the summarizer to generate "what user-story-shaped questions would lead to this file").
- Diff-based regression eval (run the eval pre/post for PRs that change chunking or prompts; surface delta).

---

## Distribution

- **GitHub Releases** with prebuilt binaries: linux x86_64, linux arm64, macOS arm64.
- **`cargo install megagrep`** for the cargo-using crowd.
- **CI install** via `curl | sh` against the release URL or a setup-action.
- **Homebrew formula:** deferred until external interest justifies the maintenance.

---

## Defaults to be tuned with evaluation

These are starting values, not architectural commitments:

| Default                          | Initial value | Tune with             |
|----------------------------------|--------------|------------------------|
| `--top-k`                        | 5            | Eval recall@K curves   |
| `--symbols-per-file`             | 3            | Eval + manual review   |
| `--concurrency`                  | 8            | Indexer wallclock perf |
| `--max-cost` default             | $50          | Real per-merge costs   |
| Big-file roll-up threshold       | ~50K tokens  | Haiku context window utilization |
| HWM advancement success threshold| ~95%         | Real failure patterns  |
| Summarizer prompts               | (TBD)        | Eval iteration         |

---

## Explicit punts (things deliberately NOT in v1)

This list is part of the spec because "we considered it and chose not to do it" is information future contributors need.

- **No grep replacement.** Megagrep is the conceptual layer on top of a codebase, not a replacement for `rg`/`grep`. The agent integration text actively encourages hybrid use.
- **No commit-keyed namespaces.** Single namespace per repo, updated in place. Considered and rejected: the cost of statelessness (no historical search) wasn't worth the operational complexity.
- **No `copy_from_namespace` usage.** [Cursor uses this](https://turbopuffer.com/customers/cursor) for cross-namespace embedding reuse and for fast onboarding; with our single-namespace-per-repo model it's irrelevant.
- **No Merkle-tree fingerprinting.** [Cursor's secure-indexing approach](https://cursor.com/blog/secure-codebase-indexing) handles "find any namespace similar enough to this working tree" via per-file content hashing. The HWM-based ancestor approach handles ~95% of practical cases at a fraction of the complexity.
- **No raw-code embeddings.** We embed LLM summaries instead. Decision driven by not having a code-trained embedding model; revisit if Voyage code-3 or similar shows substantially better behavior in eval.
- **No hybrid BM25 + vector search.** Turbopuffer supports it natively, but exposing it through the abstraction interface adds complexity for v1. Pure vector for now; hybrid is a v2 candidate if eval shows lexical matching helps.
- **No re-ranking layer.** Single-stage vector retrieval. Re-ranking with a cross-encoder is a v2 quality lever.
- **No reverse-import context for file summaries.** Highest-quality file-level signal but requires a full dep-graph pre-pass. Deferred to v2.
- **No symbol-level chunking for Svelte.** File-level only. The `tree-sitter-svelte` grammar is meaningfully less mature than the others, and Svelte components are file-scoped anyway. Revisit if v1 usage shows real demand.
- **No raw code in search results.** Paths and summaries only. The agent reads files directly for ground truth.
- **No pagination on search results.** Small `k`, refine the query if more is needed.
- **No stdin support for queries.** Positional arg only. Trivial to add later.
- **No `--rank-by` knob.** Ranking is fixed: file-summary score for file ordering, symbol score for within-file symbol ordering.
- **No additional vector store backends in v1.** Interfaces are designed for swap; only turbopuffer is implemented. Add backends when there's a real second user driving the requirement.
- **No additional embedder/summarizer providers in v1.** Same rationale.
- **No MCP server integration.** The CLI shell-out is the integration point. Auth stays in env vars on the developer's machine.
- **No per-repo `megagrep.yaml`.** All user config lives in `~/.config/megagrep/`. `.megagrepignore` is the only repo-resident config file.
- **No Claude Code plugin packaging in v1.** `megagrep init` writes a `CLAUDE.md` section, which is sufficient for internal use. A proper plugin is the distribution story for external adoption — v2.

---

## References

- Cursor + Turbopuffer customer story: https://turbopuffer.com/customers/cursor — context for the architectural pattern, the 23.5% benchmark improvement, and `copy_from_namespace`.
- Cursor's semantic search benchmark write-up: https://cursor.com/blog/semsearch — referenced from the Turbopuffer page.
- Cursor's secure codebase indexing: https://cursor.com/blog/secure-codebase-indexing — Merkle-tree fingerprinting approach we considered and rejected for v1.
- Turbopuffer documentation: https://turbopuffer.com/docs — namespace model, hybrid search, write semantics.
- tree-sitter: https://tree-sitter.github.io — incremental parsing library used for symbol-level chunking. Originally created by Max Brunsfeld.
- XDG Base Directory Specification: https://specifications.freedesktop.org/basedir-spec/basedir-spec-latest.html — for `~/.config/megagrep/` placement.
- CLI conventions reference: `gh`, `gcloud`, `aws` — config-in-`~/.config`-with-env-overrides pattern.

---

## Open questions for review

Items that warrant feedback before implementation begins:

1. The summarizer prompts themselves — file-level and symbol-level — need to be drafted and iterated against the eval set. Quality depends heavily on these and they're not yet written.
2. The seed eval cases (10 to start) — needs human selection from real Linear tickets, with file annotations. This is the bootstrapping work that gates measuring everything else.
3. Bootstrap cost on the largest Dayforward repo — to be confirmed via `megagrep index --dry-run` on real codebase before committing to the first full index.
4. Whether the v1 language list (Go, Rust, TS/JS, Svelte) is complete for Dayforward — confirmed at planning time; verify nothing was missed.

---

*This spec is a starting point, not a contract. The decisions captured here reflect the planning conversation that produced them; some will be refined or reversed as implementation surfaces issues, and that's fine. The goal is to make the reasoning behind each decision explicit, so any change is a deliberate one rather than drift.*
