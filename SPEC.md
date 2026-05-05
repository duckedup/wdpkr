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
| Vector store| Stores embeddings, runs ANN search                     | Turbopuffer (trait designed for swap)    |
| Embedder    | Embeds natural-language summaries                      | Voyage `voyage-code-3` (trait designed for swap) |
| Summarizer  | Generates NL summaries of code chunks                  | Anthropic Claude Haiku                   |
| Chunker     | AST-aware code splitting into file + symbol chunks    | tree-sitter (per-language grammars)      |

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

### Granularity: hierarchical, AST-driven

Two layers of chunks per indexable file (where supported):

| Language               | File-level | Symbol-level | tree-sitter grammar          |
|------------------------|------------|--------------|------------------------------|
| Go                     | ✅          | ✅            | `tree-sitter-go`             |
| Rust                   | ✅          | ✅            | `tree-sitter-rust`           |
| TypeScript / JavaScript| ✅          | ✅            | `tree-sitter-typescript` / `tree-sitter-javascript` |
| Python                 | ✅          | ✅            | `tree-sitter-python`         |
| Java                   | ✅          | ✅            | `tree-sitter-java`           |
| C / C++                | ✅          | ✅            | `tree-sitter-cpp`            |
| C#                     | ✅          | ✅            | `tree-sitter-c-sharp`        |
| Svelte                 | ✅          | ❌            | —                            |
| Everything else        | ✅          | ❌            | —                            |

For unsupported languages: file-level summary still works without a grammar. Svelte's multi-section structure (script + template + style) doesn't fit the symbol model cleanly; v1 treats `.svelte` files as file-level only. Adding a new language requires only a tree-sitter grammar crate and a node-type map — the extraction logic is language-agnostic.

### AST extraction strategy

Symbol-level chunking is not "split by line count." It uses tree-sitter to parse the full file AST and extracts **semantically meaningful top-level nodes**. The key insight: IDEs already use ASTs to understand code structure — dependencies, symbol boundaries, docstrings. We should leverage the same parsed structure rather than inventing our own heuristics.

**Extractable node types per language:**

| Language       | Extracted node types                                                                                     |
|----------------|----------------------------------------------------------------------------------------------------------|
| Go             | `function_declaration`, `method_declaration`, `type_declaration`, `const_declaration`, `var_declaration`  |
| Rust           | `function_item`, `impl_item`, `struct_item`, `enum_item`, `trait_item`, `mod_item`, `const_item`         |
| TypeScript/JS  | `function_declaration`, `class_declaration`, `interface_declaration`, `type_alias_declaration`, `export_statement`, `arrow_function` (named), `method_definition` |
| Python         | `function_definition`, `class_definition`, `decorated_definition`                                         |
| Java           | `class_declaration`, `method_declaration`, `interface_declaration`, `enum_declaration`, `constructor_declaration` |
| C/C++          | `function_definition`, `class_specifier`, `struct_specifier`, `enum_specifier`, `namespace_definition`    |
| C#             | `class_declaration`, `method_declaration`, `interface_declaration`, `struct_declaration`, `enum_declaration` |

**Extraction walk:**

1. Parse the file into a tree-sitter `Tree`.
2. Walk the root node's children (top-level declarations only — not recursing into function bodies).
3. For each child matching a splittable node type, extract it as a symbol chunk with:
   - `name`: the identifier node's text (e.g., function name)
   - `kind`: normalized kind (`function`, `method`, `type`, `struct`, `enum`, `trait`, `interface`, `const`)
   - `lines`: `(start_row, end_row)` from tree-sitter's byte-offset positions
   - `body`: the full text of the node, including any leading doc comment
   - `signature`: for functions/methods, just the signature line(s) — used in summaries for cross-referencing
4. For nodes that are containers (e.g., `impl_item` in Rust, `class_declaration` in Java), recurse one level to extract methods as separate symbol chunks, with the parent as context.

**Doc comment association:** tree-sitter grammars represent doc comments (`///`, `/** */`, `""" """`, `//`) as sibling nodes preceding the declaration they document. The extractor checks the previous sibling of each extracted node; if it's a comment node, it's included in the symbol chunk's body. This is free structured data that dramatically improves summary quality.

**Import/dependency extraction:** At file-level, the AST walk also extracts all import/require/use nodes. These are:
- Passed to the file-level summarizer as structured context (not just raw text — parsed into `{module, imported_names}` tuples)
- Stored as metadata on the file-level vector for potential dependency-graph queries in v2

**Oversized symbol handling:** If a single symbol exceeds the summarizer's context window (~50K tokens), the symbol is still treated as one logical chunk for summarization purposes — the summarizer receives the signature + doc comment + a truncated body with a note about truncation. The alternative (splitting a function into arbitrary pieces) produces worse summaries than summarizing a truncated-but-coherent unit.

**Fallback for parse failures:** If tree-sitter fails to parse a file (malformed syntax, grammar bug), the file falls back to file-level-only chunking. Parse failures are logged to stderr but do not block the indexing run. This matches claude-context's approach — AST failure should be transparent and non-fatal.

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

**Core principle:** embed the **summaries**, not the raw code. Because the inputs to the embedder are natural-language prose (LLM-generated summaries), code-specialized embedders are not strictly necessary — but models trained on code + natural language (like Voyage) still have an edge because our summaries contain identifiers, type names, and domain terms that sit between pure prose and pure code.

Same embedder must be used for indexing and querying. Switching embedders requires a full reindex.

#### Embedder trait

```rust
#[async_trait]
pub trait Embedder: Send + Sync {
    /// Embed a single text string, returning a dense vector.
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// Embed a batch of texts. Implementations should handle chunking
    /// against their provider's batch-size limits internally.
    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;

    /// The dimensionality of vectors this embedder produces.
    /// For providers with fixed dimensions (Voyage, OpenAI known models),
    /// this is a constant. For Ollama, detected via a probe embed on init.
    fn dimension(&self) -> usize;

    /// Max input tokens the model accepts. Text exceeding this is
    /// truncated (with a warning) before embedding.
    fn max_input_tokens(&self) -> usize;

    /// Provider name for logging and diagnostics (e.g., "voyage", "ollama").
    fn provider_name(&self) -> &str;

    /// Model identifier for logging and config validation.
    fn model_name(&self) -> &str;
}
```

#### Supported providers

| Provider  | Default model           | Dimensions | Max tokens | Use case                        |
|-----------|------------------------|------------|------------|----------------------------------|
| **Voyage**  | `voyage-code-3`      | 1024       | 16,000     | Default. Code-trained model; best fit for code summaries. |
| **Ollama**  | `nomic-embed-text`   | 768 (auto) | 8,192      | Local/offline. No API costs. Good for eval iteration. |
| **OpenAI**  | `text-embedding-3-large` | 3072   | 8,191      | Widely available. Fallback if Voyage unavailable. |

**Why `voyage-code-3` as default:** Our summaries are natural language, but they're *code-adjacent* natural language — dense with function names, type identifiers, module paths, and domain terms that sit between pure prose and pure code. `voyage-code-3` is explicitly trained on code and technical content, giving it a vocabulary advantage over general-purpose embedders on exactly this kind of input. It also benchmarks at the top of MTEB code retrieval tasks. The 16K token context is generous for summaries. OpenAI's `text-embedding-3-large` is a solid fallback but has no code-specific training signal.

**Why Ollama support matters:** during development and eval iteration, you're making hundreds of embed calls to test prompt changes. Paying per-call for that iteration loop is wasteful. A local `nomic-embed-text` running on Ollama gives fast, free iteration at the cost of some quality — acceptable for the dev loop, not for production indexing.

#### Provider configuration

```yaml
# ~/.config/megagrep/config.yaml
embedder:
  provider: voyage          # "voyage" | "ollama" | "openai"
  model: voyage-code-3      # override default model for the provider
  batch_size: 64            # texts per API call (default: 64)
```

```bash
# Env vars (override config file)
MEGAGREP_EMBED_PROVIDER=voyage
MEGAGREP_EMBED_MODEL=voyage-code-3
VOYAGE_API_KEY=...          # provider-specific credential
OLLAMA_HOST=http://localhost:11434  # Ollama endpoint
OPENAI_API_KEY=...          # if using OpenAI provider
```

**Dimension detection:** For Voyage and OpenAI, dimensions are looked up from a hardcoded map of known models (cheap, no API call). For Ollama (where models are user-installed and dimensions vary), the adapter sends a single probe embedding on initialization to detect the dimension. This detected dimension is used when creating the vector store namespace.

**Batch embedding:** The `embed_batch` implementation handles provider-specific batch limits internally. Voyage supports up to 128 texts per call; OpenAI up to 2048; Ollama processes sequentially. The caller doesn't need to know — it passes all texts and the adapter chunks appropriately.

**Text truncation:** If input text exceeds `max_input_tokens`, it's truncated to fit with a warning logged to stderr. This is a safety net, not normal operation — summaries should be sized to fit within the embedder's context window by the summarizer prompt.

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

- Designed to be triggered by merge to main (or on a schedule) via GitHub Actions or equivalent. The triggering mechanism is up to the consumer — megagrep's only contract is: `megagrep index` runs in a git checkout with env vars set.
- Requires `fetch-depth: 0` (full history) — the indexer needs `git diff HWM..HEAD` which fails on shallow clones.
- `concurrency: { group: megagrep-${repo}, cancel-in-progress: true }` — concurrent indexer runs against the same namespace are unsafe. The CI workflow should ensure only one runs at a time, killing stale runs when a new merge lands.
- The config system (see [Configuration](#configuration)) is fully env-var-driven — CI never needs a config file. Credentials go in secrets; provider/tuning overrides go in workflow-level `env:` blocks.
- `megagrep init` generates a starter workflow file (`.github/workflows/megagrep.yml`) so teams don't write this boilerplate by hand.

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

`git config`-style management with dotted keys mapped to the `FileConfig` struct hierarchy (e.g., `embedder.model`, `indexer.concurrency`, `store.provider`).

```
megagrep config init                  # write default ~/.config/megagrep/config.yaml
megagrep config get <key>
megagrep config set <key> <value>
megagrep config list                  # show effective config and source of each value
megagrep config edit                  # open in $EDITOR
megagrep config path                  # print resolved config file location
```

`config list` is the key debugging affordance — it shows the resolved value for every field and where it came from:

```
$ megagrep config list
embedder.provider    = voyage          [default]
embedder.model       = voyage-code-3   [env: MEGAGREP_EMBED_MODEL]
indexer.concurrency  = 16              [file: ~/.config/megagrep/config.yaml]
indexer.max_cost     = 50              [default]
store.provider       = turbopuffer     [default]
...
```

This makes it trivial to debug "why is it using this model?" in both local and CI contexts.

---

## Configuration

### Locations

- **Primary:** `~/.config/megagrep/config.yaml`, file mode 0600.
- **Repo-resident:** `.megagrepignore` is the only repo-level config file. (No per-repo `megagrep.yaml`; the user-level config plus auto-derivation handles the rest.)
- **Env vars:** override config file values. Primarily for secrets.

This pattern matches `gh`, `gcloud`, and `aws` CLI conventions: user config in `~/.config/<tool>/`, env vars as overrides. Familiar territory.

### Resolution order

```
defaults  →  config file  →  env vars  →  CLI flags
```

Later sources override earlier ones. This four-layer chain matters because **different contexts use different layers:**

- **Local dev:** config file (`~/.config/megagrep/config.yaml`) for stable settings, env vars for credentials.
- **CI / GitHub Actions:** env vars for everything. No config file exists in CI runners, and that's fine — the system must be fully configurable via env vars alone.
- **One-off overrides:** CLI flags (e.g., `--concurrency 16` for a large reindex, `--max-cost 100` for bootstrap).

### Config implementation: `env_or` pattern

Following the Dayforward codebase convention (`dayforward/src/config/src`), config resolution uses a single generic function that checks an env var and falls back to a default. The "default" at each call site is the config-file value if one was loaded, otherwise the hardcoded default. This keeps resolution explicit — every field's env var name and fallback are visible in one place.

#### Core resolution function

```rust
// src/config/mod.rs

/// Read an environment variable, parse it, or fall back to `default`.
/// Silent fallback — parse failures use the default without error.
/// This matches the Dayforward `env_or` pattern.
pub(crate) fn env_or<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
```

For the config-file layer, we load `~/.config/megagrep/config.yaml` into an `Option<FileConfig>` (all fields optional via serde). Each `from_env()` method uses the file value as the fallback when present:

```rust
/// Helper: use the config-file value if present, otherwise the hardcoded default.
fn file_or<T>(file_val: Option<T>, default: T) -> T {
    file_val.unwrap_or(default)
}
```

The full resolution for any field is then:

```rust
// env var wins → config file value → hardcoded default
let concurrency = env_or("MEGAGREP_CONCURRENCY", file_or(file.concurrency, 8));
```

#### Module layout

```
src/config/
├── mod.rs          # Config struct, env_or, file loading, from_env()
├── store.rs        # StoreConfig — vector store provider
├── embed.rs        # EmbedConfig — embedding provider
├── summarizer.rs   # SummarizerConfig — LLM summarizer
├── indexer.rs       # IndexerConfig — indexing behavior
```

Each module owns its env var names, defaults, and `from_env()` constructor. The top-level `Config` aggregates them.

#### Top-level config

```rust
// src/config/mod.rs

mod store;
mod embed;
mod summarizer;
mod indexer;

pub use store::StoreConfig;
pub use embed::EmbedConfig;
pub use summarizer::SummarizerConfig;
pub use indexer::IndexerConfig;

pub struct Config {
    pub store: StoreConfig,
    pub embed: EmbedConfig,
    pub summarizer: SummarizerConfig,
    pub indexer: IndexerConfig,
}

impl Config {
    pub fn new() -> Self {
        let file = FileConfig::load(); // None if file missing or malformed
        Self {
            store: StoreConfig::from_env(&file),
            embed: EmbedConfig::from_env(&file),
            summarizer: SummarizerConfig::from_env(&file),
            indexer: IndexerConfig::from_env(&file),
        }
    }
}
```

#### `StoreConfig`

```rust
// src/config/store.rs

pub struct StoreConfig {
    pub provider: String,       // "turbopuffer"
    pub api_key: String,        // credential — env-only, never in config file
}

impl StoreConfig {
    pub fn from_env(file: &Option<FileConfig>) -> Self {
        let f = file.as_ref().and_then(|f| f.store.as_ref());
        Self {
            provider: env_or(
                "MEGAGREP_STORE_PROVIDER",
                file_or(f.map(|s| s.provider.clone()), "turbopuffer".into()),
            ),
            api_key: env_or("TURBOPUFFER_API_KEY", String::new()),
        }
    }
}
```

#### `EmbedConfig`

```rust
// src/config/embed.rs

pub struct EmbedConfig {
    pub provider: String,       // "voyage" | "ollama" | "openai"
    pub model: String,          // provider-specific model name
    pub batch_size: usize,      // texts per API call

    // Provider-specific credentials / endpoints
    pub voyage_api_key: String,
    pub openai_api_key: String,
    pub ollama_host: String,
}

impl EmbedConfig {
    pub fn from_env(file: &Option<FileConfig>) -> Self {
        let f = file.as_ref().and_then(|f| f.embedder.as_ref());

        let provider = env_or(
            "MEGAGREP_EMBED_PROVIDER",
            file_or(f.map(|e| e.provider.clone()), "voyage".into()),
        );

        // Default model depends on the resolved provider
        let default_model = match provider.as_str() {
            "voyage" => "voyage-code-3",
            "ollama" => "nomic-embed-text",
            "openai" => "text-embedding-3-large",
            _ => "voyage-code-3",
        };

        Self {
            provider: provider.clone(),
            model: env_or(
                "MEGAGREP_EMBED_MODEL",
                file_or(f.and_then(|e| e.model.clone()), default_model.into()),
            ),
            batch_size: env_or(
                "MEGAGREP_EMBED_BATCH_SIZE",
                file_or(f.and_then(|e| e.batch_size), 64),
            ),
            voyage_api_key: env_or("VOYAGE_API_KEY", String::new()),
            openai_api_key: env_or("OPENAI_API_KEY", String::new()),
            ollama_host: env_or(
                "OLLAMA_HOST",
                file_or(f.and_then(|e| e.ollama_host.clone()), "http://localhost:11434".into()),
            ),
        }
    }

    /// Validate that the selected provider's required credential is set.
    /// Called at startup — fail fast, not on first API call.
    pub fn validate(&self) -> Result<()> {
        match self.provider.as_str() {
            "voyage" if self.voyage_api_key.is_empty() => {
                bail!("VOYAGE_API_KEY is required when embedder.provider=voyage")
            }
            "openai" if self.openai_api_key.is_empty() => {
                bail!("OPENAI_API_KEY is required when embedder.provider=openai")
            }
            _ => Ok(()),
        }
    }
}
```

**Key design choice:** the default model is derived from the resolved provider, not hardcoded independently. If someone sets `MEGAGREP_EMBED_PROVIDER=ollama` but doesn't set `MEGAGREP_EMBED_MODEL`, they get `nomic-embed-text` — not `voyage-code-3` for a provider that doesn't serve it.

#### `SummarizerConfig`

```rust
// src/config/summarizer.rs

pub struct SummarizerConfig {
    pub provider: String,       // "anthropic"
    pub model: String,          // Anthropic model name
    pub api_key: String,        // credential — env-only
}

impl SummarizerConfig {
    pub fn from_env(file: &Option<FileConfig>) -> Self {
        let f = file.as_ref().and_then(|f| f.summarizer.as_ref());
        Self {
            provider: env_or(
                "MEGAGREP_SUMMARIZER_PROVIDER",
                file_or(f.map(|s| s.provider.clone()), "anthropic".into()),
            ),
            model: env_or(
                "MEGAGREP_SUMMARIZER_MODEL",
                file_or(
                    f.and_then(|s| s.model.clone()),
                    "claude-haiku-4-5-20251001".into(),
                ),
            ),
            api_key: env_or("ANTHROPIC_API_KEY", String::new()),
        }
    }
}
```

#### `IndexerConfig`

```rust
// src/config/indexer.rs

pub struct IndexerConfig {
    pub namespace: String,            // auto-derived if empty
    pub default_branch: String,
    pub concurrency: usize,
    pub max_cost: f64,                // USD
    pub hwm_success_threshold: f64,   // 0.0–1.0
}

impl IndexerConfig {
    pub fn from_env(file: &Option<FileConfig>) -> Self {
        let f = file.as_ref().and_then(|f| f.indexer.as_ref());
        Self {
            namespace: env_or(
                "MEGAGREP_NAMESPACE",
                file_or(f.and_then(|i| i.namespace.clone()), String::new()),
            ),
            default_branch: env_or(
                "MEGAGREP_DEFAULT_BRANCH",
                file_or(f.and_then(|i| i.default_branch.clone()), "main".into()),
            ),
            concurrency: env_or(
                "MEGAGREP_CONCURRENCY",
                file_or(f.and_then(|i| i.concurrency), 8),
            ),
            max_cost: env_or(
                "MEGAGREP_MAX_COST",
                file_or(f.and_then(|i| i.max_cost), 50.0),
            ),
            hwm_success_threshold: env_or(
                "MEGAGREP_HWM_SUCCESS_THRESHOLD",
                file_or(f.and_then(|i| i.hwm_success_threshold), 0.95),
            ),
        }
    }
}
```

#### Config file schema (serde target)

The config file is deserialized into a struct with all optional fields. Missing keys are `None`, which `file_or` treats as "use the hardcoded default."

```rust
// src/config/mod.rs

#[derive(Deserialize, Default)]
pub struct FileConfig {
    pub store: Option<FileStoreConfig>,
    pub embedder: Option<FileEmbedConfig>,
    pub summarizer: Option<FileSummarizerConfig>,
    pub indexer: Option<FileIndexerConfig>,
}

#[derive(Deserialize)]
pub struct FileStoreConfig {
    pub provider: Option<String>,
}

#[derive(Deserialize)]
pub struct FileEmbedConfig {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub batch_size: Option<usize>,
    pub ollama_host: Option<String>,
}

#[derive(Deserialize)]
pub struct FileSummarizerConfig {
    pub provider: Option<String>,
    pub model: Option<String>,
}

#[derive(Deserialize)]
pub struct FileIndexerConfig {
    pub namespace: Option<String>,
    pub default_branch: Option<String>,
    pub concurrency: Option<usize>,
    pub max_cost: Option<f64>,
    pub hwm_success_threshold: Option<f64>,
}

impl FileConfig {
    pub fn load() -> Option<Self> {
        let path = dirs::config_dir()?.join("megagrep/config.yaml");
        let content = std::fs::read_to_string(&path).ok()?;
        serde_yaml::from_str(&content).ok()
    }
}
```

#### Corresponding `config.yaml`

```yaml
# ~/.config/megagrep/config.yaml — for local dev use.
# Every field is optional. Env vars override all values here.
# Credentials (API keys) should be set via env vars, not this file.

store:
  provider: turbopuffer

embedder:
  provider: voyage
  model: voyage-code-3
  batch_size: 64
  # ollama_host: http://localhost:11434

summarizer:
  provider: anthropic
  model: claude-haiku-4-5-20251001

indexer:
  # namespace: ""             # auto-derived from git remote if empty
  default_branch: main
  concurrency: 8
  max_cost: 50
  hwm_success_threshold: 0.95
```

#### Testing pattern

Following Dayforward convention, each config module has tests verifying both defaults and env overrides. Uses `serial_test` to isolate env var mutations:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn embed_config_defaults() {
        // Clear all relevant env vars
        std::env::remove_var("MEGAGREP_EMBED_PROVIDER");
        std::env::remove_var("MEGAGREP_EMBED_MODEL");
        std::env::remove_var("MEGAGREP_EMBED_BATCH_SIZE");

        let cfg = EmbedConfig::from_env(&None);
        assert_eq!(cfg.provider, "voyage");
        assert_eq!(cfg.model, "voyage-code-3");
        assert_eq!(cfg.batch_size, 64);
    }

    #[test]
    #[serial]
    fn embed_config_env_overrides() {
        std::env::set_var("MEGAGREP_EMBED_PROVIDER", "ollama");
        // Model should auto-derive from provider when not set
        std::env::remove_var("MEGAGREP_EMBED_MODEL");

        let cfg = EmbedConfig::from_env(&None);
        assert_eq!(cfg.provider, "ollama");
        assert_eq!(cfg.model, "nomic-embed-text");

        // Cleanup
        std::env::remove_var("MEGAGREP_EMBED_PROVIDER");
    }
}
```

### Environment variable quick reference

Complete list for CI/CD use. Every value is configurable via env var alone — no config file needed.

**Credentials (provider-standard names):**

| Env var                | Required when              |
|------------------------|----------------------------|
| `TURBOPUFFER_API_KEY`  | Always (v1 store)          |
| `VOYAGE_API_KEY`       | `embed.provider=voyage`    |
| `OPENAI_API_KEY`       | `embed.provider=openai`    |
| `ANTHROPIC_API_KEY`    | Always (v1 summarizer)     |
| `OLLAMA_HOST`          | `embed.provider=ollama` (default: `http://localhost:11434`) |

**Megagrep settings:**

| Env var                          | Default                     |
|----------------------------------|-----------------------------|
| `MEGAGREP_STORE_PROVIDER`        | `turbopuffer`               |
| `MEGAGREP_EMBED_PROVIDER`        | `voyage`                    |
| `MEGAGREP_EMBED_MODEL`           | per-provider (see `EmbedConfig`) |
| `MEGAGREP_EMBED_BATCH_SIZE`      | `64`                        |
| `MEGAGREP_SUMMARIZER_PROVIDER`   | `anthropic`                 |
| `MEGAGREP_SUMMARIZER_MODEL`      | `claude-haiku-4-5-20251001` |
| `MEGAGREP_NAMESPACE`             | auto-derived from git remote|
| `MEGAGREP_DEFAULT_BRANCH`        | `main`                      |
| `MEGAGREP_CONCURRENCY`           | `8`                         |
| `MEGAGREP_MAX_COST`              | `50`                        |
| `MEGAGREP_HWM_SUCCESS_THRESHOLD` | `0.95`                      |

CLI flags (`--concurrency`, `--max-cost`, etc.) override env vars for that invocation.

### CI-first configuration

The env var surface is designed so that **CI never needs a config file.** A GitHub Actions workflow sets env vars from secrets and workflow-level `env:` blocks; that's the entire config story. The config file exists for developer convenience on their local machine — CI should never depend on it.

**Critical invariant: embedder must match between indexing and search.** If CI indexes with `voyage-code-3` and a developer's local config uses `openai`, query embeddings will be in a different vector space than the stored embeddings. The `NamespaceMetadata.embedder` field in the vector store catches this — search will fail with a clear error message ("index was built with voyage/voyage-code-3, but search is configured for openai/text-embedding-3-large; run `megagrep index --full` to reindex or change your embedder config"). This is a hard error, not a warning.

In practice, this means local developers need either:
- The same embedder provider + API key as CI (recommended — just set `VOYAGE_API_KEY` locally), or
- A local config file that matches CI's embedder choice (the defaults handle this if CI also uses defaults)

### Pluggable backends

Four interfaces with v1 implementations:

```
src/store/        # VectorStore trait     → turbopuffer adapter
src/embed/        # Embedder trait        → voyage, ollama, openai adapters
src/summarize/    # Summarizer trait      → anthropic adapter
src/chunk/        # Chunker trait         → tree-sitter adapter
```

**Design principle:** define the abstractions from day one. Ship one implementation where one is sufficient (store, summarizer, chunker) — the second backend is what surfaces leaky interface assumptions, so building it without a real driver is guesswork. For the embedder, we ship three from the start because the use cases are genuinely distinct (production quality vs. local iteration vs. widely-available fallback) and having multiple implementations validates the trait immediately.

#### VectorStore trait

The vector store abstraction is the most important interface to get right — it constrains what search capabilities the system can expose.

```rust
#[async_trait]
pub trait VectorStore: Send + Sync {
    // ── Namespace lifecycle ──────────────────────────────────────────

    /// Create a namespace (collection/index) for a repository.
    /// `dimension` is the embedding vector size, determined by the embedder.
    async fn create_namespace(&self, ns: &Namespace, dimension: usize) -> Result<()>;

    /// Delete a namespace and all its vectors.
    async fn delete_namespace(&self, ns: &Namespace) -> Result<()>;

    /// Check whether a namespace exists.
    async fn namespace_exists(&self, ns: &Namespace) -> Result<bool>;

    // ── Metadata ─────────────────────────────────────────────────────

    /// Read opaque metadata attached to the namespace (e.g., HWM SHA).
    async fn get_metadata(&self, ns: &Namespace) -> Result<NamespaceMetadata>;

    /// Write metadata. Used to persist the high-water mark after indexing.
    async fn set_metadata(&self, ns: &Namespace, meta: &NamespaceMetadata) -> Result<()>;

    // ── Write ────────────────────────────────────────────────────────

    /// Upsert a batch of documents. Idempotent — re-upserting the same
    /// ID with different content overwrites. Implementations should handle
    /// batching against provider limits internally.
    async fn upsert(&self, ns: &Namespace, docs: &[VectorDocument]) -> Result<UpsertStats>;

    /// Delete documents by ID. Used when files are deleted or re-chunked.
    async fn delete_by_ids(&self, ns: &Namespace, ids: &[&str]) -> Result<()>;

    /// Delete all documents whose `file_path` matches the given path.
    /// More ergonomic than tracking IDs for file-level deletes.
    async fn delete_by_file(&self, ns: &Namespace, file_path: &str) -> Result<()>;

    // ── Search ───────────────────────────────────────────────────────

    /// Nearest-neighbor search over the namespace.
    async fn search(
        &self,
        ns: &Namespace,
        query_vector: &[f32],
        opts: &SearchOptions,
    ) -> Result<Vec<SearchResult>>;
}
```

**Supporting types:**

```rust
/// Identifies a namespace. Derived from the git remote URL (normalized + hashed)
/// or overridden in config.
pub struct Namespace(pub String);

/// Metadata stored alongside the namespace — not in the vectors themselves.
pub struct NamespaceMetadata {
    /// Last successfully indexed commit SHA.
    pub hwm_sha: Option<String>,
    /// Embedder provider + model used to create this namespace.
    /// Changing embedder requires a full reindex; this field catches the mismatch.
    pub embedder: Option<String>,
    /// Arbitrary key-value pairs for future use.
    pub extra: HashMap<String, String>,
}

/// A document to be stored in the vector store.
pub struct VectorDocument {
    /// Deterministic ID: hash of (file_path, chunk_kind, symbol_name, content_hash).
    pub id: String,
    /// The dense embedding vector.
    pub vector: Vec<f32>,
    /// The summary text that was embedded (stored for debugging, not searched).
    pub summary: String,

    // ── Filterable attributes ──
    pub file_path: String,
    pub chunk_kind: ChunkKind,       // File | Symbol
    pub symbol_name: Option<String>, // None for file-level chunks
    pub symbol_kind: Option<String>, // "function", "struct", "trait", etc.
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
    pub language: Option<String>,
}

pub enum ChunkKind { File, Symbol }

pub struct SearchOptions {
    /// Max results to return.
    pub top_k: usize,
    /// Filter to a subtree (e.g., "internal/finance/").
    pub path_prefix: Option<String>,
    /// Filter by chunk kind.
    pub chunk_kind: Option<ChunkKind>,
    /// Filter by language.
    pub language: Option<String>,
    /// Minimum similarity score (0.0–1.0). Results below this are dropped.
    pub min_score: Option<f32>,
}

pub struct SearchResult {
    pub id: String,
    pub score: f32,
    pub file_path: String,
    pub chunk_kind: ChunkKind,
    pub symbol_name: Option<String>,
    pub symbol_kind: Option<String>,
    pub summary: String,
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
    pub language: Option<String>,
}

pub struct UpsertStats {
    pub upserted: usize,
    pub skipped: usize,  // already up-to-date (same ID + content hash)
}
```

**Why this API shape:**

- **`delete_by_file`** is a first-class operation because the indexer's unit of change detection is the file (via `git diff`). Without it, the indexer would need to track individual chunk IDs per file — an unnecessary bookkeeping burden. Turbopuffer supports attribute filtering on delete; other backends may need to query-then-delete.
- **`NamespaceMetadata`** stores the HWM and the embedder identity. The embedder field is critical: if a user switches from Voyage to OpenAI, the stored vectors are incompatible. The indexer checks this on startup and refuses incremental indexing if there's a mismatch (requires `--full`).
- **`SearchOptions` filtering** uses server-side attribute filters (not client-side post-filtering). Turbopuffer supports this natively. Any backend that can't filter server-side should implement it as a post-filter with an over-fetched `top_k`.
- **No hybrid search in the trait.** BM25 + vector hybrid search is a v2 quality lever. Adding it to the trait now would constrain backend selection (not all stores support BM25 natively). The trait is pure dense vector search; hybrid can be added as an optional extension trait later.
- **No `list_documents` / `get_by_id` / `count`.** These are admin/debug operations, not part of the hot path. They can be added to a separate `VectorStoreAdmin` trait if needed.

#### Embedder trait

Defined in the [Embedding](#embedding) section above. Three v1 implementations: Voyage (default), Ollama (local), OpenAI (fallback).

#### Summarizer trait

```rust
#[async_trait]
pub trait Summarizer: Send + Sync {
    /// Summarize a file given its content and metadata context.
    async fn summarize_file(&self, input: &FileSummaryInput) -> Result<String>;

    /// Summarize a symbol given its body and the parent file summary.
    async fn summarize_symbol(&self, input: &SymbolSummaryInput) -> Result<String>;

    /// Model name for cost tracking and logging.
    fn model_name(&self) -> &str;
}

pub struct FileSummaryInput {
    pub file_path: String,
    pub content: String,
    pub imports: Vec<Import>,      // Parsed from AST, not raw text
    pub language: String,
}

pub struct SymbolSummaryInput {
    pub symbol_name: String,
    pub symbol_kind: String,
    pub body: String,
    pub signature: Option<String>,
    pub doc_comment: Option<String>,
    pub file_path: String,
    pub file_summary: String,      // Parent file summary, for context
}
```

Single v1 implementation: Anthropic Claude Haiku.

#### Chunker trait

```rust
pub trait Chunker: Send + Sync {
    /// Extract chunks from a file. Returns a file-level chunk and
    /// zero or more symbol-level chunks.
    fn chunk(&self, file_path: &str, content: &str, language: &str) -> Result<FileChunks>;
}

pub struct FileChunks {
    pub file_path: String,
    pub language: String,
    pub imports: Vec<Import>,
    pub file_content: String,          // Full content for file-level summary
    pub symbols: Vec<SymbolChunk>,     // Empty if no grammar or parse failure
}

pub struct SymbolChunk {
    pub name: String,
    pub kind: String,                  // "function", "method", "struct", etc.
    pub body: String,                  // Full text including doc comment
    pub signature: Option<String>,     // First line(s) of declaration
    pub doc_comment: Option<String>,   // Extracted doc comment text
    pub start_line: u32,
    pub end_line: u32,
}

pub struct Import {
    pub module: String,                // "internal/finance/commission"
    pub names: Vec<String>,           // ["ReleaseService", "PayeeRecord"]
}
```

Single v1 implementation: tree-sitter adapter (described in the [AST extraction strategy](#ast-extraction-strategy) section).

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
| Tree-sitter           | `tree-sitter` + `tree-sitter-{go,rust,typescript,javascript,python,java,cpp,c-sharp}` |
| Config file parsing   | `serde_yaml` + `dirs` (XDG path resolution) |
| Token counting        | `tiktoken-rs`                          |
| Bounded parallelism   | `futures::stream::buffer_unordered`    |
| Test isolation        | `serial_test` (env var isolation in config tests) |
| Errors                | `thiserror` (libs) + `anyhow` (binary boundary) |

No official Rust SDKs from turbopuffer, Voyage, OpenAI, or Anthropic — all four are HTTP APIs cleanly hit with `reqwest`. For Ollama, the HTTP API at `localhost:11434` is equally straightforward. This is a feature, not a limitation: the `VectorStore` / `Embedder` / `Summarizer` traits stay honest about what they need, and we avoid pulling in SDK crates that may impose their own async runtime or error types.

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
| Embedder batch size              | 64           | Provider rate limits + throughput |
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
- **No raw-code embeddings.** We embed LLM summaries instead. Even with `voyage-code-3` (a code-trained model) as our default embedder, the summary-based approach closes the vocabulary gap between user-story queries and code identifiers more effectively than embedding raw code directly. The model is good at embedding code-adjacent prose; that doesn't mean raw code is the right input. Revisit if eval shows a dual-embedding approach (summaries + raw code in separate vectors) produces meaningfully better recall.
- **No hybrid BM25 + vector search.** Turbopuffer supports it natively, and claude-context uses it by default (BM25 + dense with RRF reranking). We deliberately omit it from the `VectorStore` trait in v1 — not all backends support BM25, and adding it constrains backend portability. Pure dense vector search for now; hybrid is a v2 candidate behind an optional extension trait if eval shows lexical matching helps.
- **No re-ranking layer.** Single-stage vector retrieval. Re-ranking with a cross-encoder is a v2 quality lever.
- **No reverse-import context for file summaries.** Highest-quality file-level signal but requires a full dep-graph pre-pass. Deferred to v2.
- **No symbol-level chunking for Svelte.** File-level only. The `tree-sitter-svelte` grammar is meaningfully less mature than the others, and Svelte components are file-scoped anyway. Revisit if v1 usage shows real demand.
- **No raw code in search results.** Paths and summaries only. The agent reads files directly for ground truth.
- **No pagination on search results.** Small `k`, refine the query if more is needed.
- **No stdin support for queries.** Positional arg only. Trivial to add later.
- **No `--rank-by` knob.** Ranking is fixed: file-summary score for file ordering, symbol score for within-file symbol ordering.
- **No additional vector store backends in v1.** The `VectorStore` trait is designed for swap; only the turbopuffer adapter is implemented. Add backends (Qdrant, Milvus, local FAISS) when there's a real second user driving the requirement.
- **No additional summarizer providers in v1.** Anthropic Haiku only. The `Summarizer` trait supports swap, but we're not building adapters for providers we don't use. (Embedder has three providers because the use cases are genuinely distinct — production, local dev, and fallback.)
- **No MCP server integration.** The CLI shell-out is the integration point. Auth stays in env vars on the developer's machine.
- **No per-repo `megagrep.yaml`.** All user config lives in `~/.config/megagrep/`. `.megagrepignore` is the only repo-resident config file.
- **No Claude Code plugin packaging in v1.** `megagrep init` writes a `CLAUDE.md` section, which is sufficient for internal use. A proper plugin is the distribution story for external adoption — v2.

---

## References

- **claude-context** (Zilliz): https://github.com/zilliztech/claude-context — MCP-based codebase indexer using Milvus/Zilliz Cloud. Studied for interface design, AST chunking patterns, and embedding provider abstraction. Key differences from megagrep: embeds raw code (not summaries), uses MCP (not CLI), Milvus-specific (not backend-agnostic), no LLM summarization layer. Their hybrid search (BM25 + dense with RRF) is a strong v2 candidate for us.
- Cursor + Turbopuffer customer story: https://turbopuffer.com/customers/cursor — context for the architectural pattern, the 23.5% benchmark improvement, and `copy_from_namespace`.
- Cursor's semantic search benchmark write-up: https://cursor.com/blog/semsearch — referenced from the Turbopuffer page.
- Cursor's secure codebase indexing: https://cursor.com/blog/secure-codebase-indexing — Merkle-tree fingerprinting approach we considered and rejected for v1.
- Turbopuffer documentation: https://turbopuffer.com/docs — namespace model, hybrid search, write semantics.
- **Voyage AI embeddings:** https://docs.voyageai.com/docs/embeddings — `voyage-code-3` documentation, benchmarks, and API reference.
- tree-sitter: https://tree-sitter.github.io — incremental parsing library used for AST-based symbol extraction. Rust crate is the canonical implementation.
- XDG Base Directory Specification: https://specifications.freedesktop.org/basedir-spec/basedir-spec-latest.html — for `~/.config/megagrep/` placement.
- CLI conventions reference: `gh`, `gcloud`, `aws` — config-in-`~/.config`-with-env-overrides pattern.

---

## Open questions for review

Items that warrant feedback before implementation begins:

1. The summarizer prompts themselves — file-level and symbol-level — need to be drafted and iterated against the eval set. Quality depends heavily on these and they're not yet written.
2. The seed eval cases (10 to start) — needs human selection from real Linear tickets, with file annotations. This is the bootstrapping work that gates measuring everything else.
3. Bootstrap cost on the largest Dayforward repo — to be confirmed via `megagrep index --dry-run` on real codebase before committing to the first full index. With Voyage pricing (`voyage-code-3` at ~$0.06/1M tokens) this should be significantly cheaper than OpenAI.
4. Whether the v1 language list (Go, Rust, TS/JS, Python, Java, C/C++, C#, Svelte) covers Dayforward's stack — the expanded list adds Python, Java, C/C++, and C# over the original plan.
5. **`voyage-code-3` vs `voyage-3-large` for summary embeddings** — `voyage-code-3` is trained on code and should handle our code-adjacent summaries well, but `voyage-3-large` may outperform on pure natural-language queries. Needs eval comparison before locking in.
6. **Turbopuffer attribute filter support** — the `VectorStore` trait assumes server-side filtering by `file_path`, `chunk_kind`, `language`, etc. Verify that turbopuffer's attribute filtering covers these use cases efficiently, or adjust the trait to make filtering optional.
7. **AST node-type coverage validation** — the splittable node types per language (listed in "AST extraction strategy") are based on common patterns. Before shipping, validate against real Dayforward code that the extraction captures the right granularity — not too coarse (whole modules) or too fine (individual fields).

---

*This spec is a starting point, not a contract. The decisions captured here reflect the planning conversation that produced them; some will be refined or reversed as implementation surfaces issues, and that's fine. The goal is to make the reasoning behind each decision explicit, so any change is a deliberate one rather than drift.*
