# wdpkr

Conceptual codebase search for AI coding agents. Maintains a vector-search index of LLM-generated summaries and exposes a single `wdpkr search` command that returns tiered, file+symbol results as JSON.

**wdpkr is not a replacement for grep/ripgrep.** It's the conceptual layer on top — "where does the commission system live?" rather than "find the string `CommissionService`."

## How it works

```
wdpkr search "release commission payments to individual payees"
```

```json
{
  "query": "release commission payments to individual payees",
  "namespace": "my-repo",
  "indexed_at": "abc123",
  "results": [
    {
      "path": "src/finance/commission/release.rs",
      "score": 0.87,
      "summary": "Service for releasing commission payments...",
      "symbols": [
        {
          "name": "release_payment",
          "kind": "function",
          "lines": [42, 78],
          "summary": "Releases commission for a specified payee...",
          "score": 0.91
        }
      ]
    }
  ]
}
```

The agent reads the actual files for ground truth. wdpkr's job is to **point and describe**, not to ship source into the context window.

## Quick start

```bash
# Install
cargo install --path .

# Initialize a repo (writes CLAUDE.md section, .wdpkrignore, CI workflow)
wdpkr init

# Set up credentials
export ANTHROPIC_API_KEY=...   # for summarization (Claude Haiku)
export VOYAGE_API_KEY=...      # for embedding (voyage-code-3)
export TURBOPUFFER_API_KEY=... # for vector storage

# Index the codebase
wdpkr index --full

# Search
wdpkr search "release commission payments"
wdpkr search "how is rate limiting implemented" --pretty
wdpkr search "auth flow" --scope src/auth/ -k 10
```

## Commands

| Command | Purpose |
|---|---|
| `wdpkr index [--full]` | Index the codebase (full or incremental from HWM) |
| `wdpkr search "<query>"` | Semantic search, returns tiered JSON |
| `wdpkr config list` | Show all config values and their sources |
| `wdpkr config get <key>` | Get a single config value |
| `wdpkr config set <key> <val>` | Set a config value in the config file |
| `wdpkr config init` | Write default config file |
| `wdpkr init` | Initialize wdpkr for a repo (CLAUDE.md, .wdpkrignore, CI workflow) |

## Architecture

```
  Indexing (CI, on merge to main)         Searching (local, agent-invoked)
  ─────────────────────────────           ──────────────────────────────
  repo files                              natural-language query
       │                                       │
       ▼                                       ▼
  ┌─────────┐                             ┌──────────┐
  │ Chunker │  tree-sitter AST            │ Embedder │  same model as index
  └────┬────┘                             └────┬─────┘
       ▼                                       ▼
  ┌──────────────┐                        ┌──────────────┐
  │ Summarizer   │  Claude Haiku          │ Vector Store │  cosine similarity
  └──────┬───────┘                        └──────┬───────┘
         ▼                                       ▼
  ┌──────────┐                            group by file
  │ Embedder │  Voyage code-3             attach top symbols
  └────┬─────┘                            return tiered JSON
       ▼
  ┌──────────────┐
  │ Vector Store │  Turbopuffer
  └──────────────┘
```

### Key decisions

- **Embed summaries, not code.** Off-the-shelf embedders are mediocre on conceptual queries against raw code (vocabulary gap). LLM-generated summaries close that gap.
- **AST-driven chunking.** Tree-sitter parses files into semantically meaningful symbols (functions, types, traits) rather than arbitrary line splits. 8 languages: Rust, Go, TypeScript, JavaScript, Python, Java, C/C++, C#.
- **Pluggable backends.** Traits for VectorStore, Embedder, Summarizer, and Chunker — single implementations shipped, designed for swap.
- **CLI, not MCP.** Any agent that can shell out can use it. Auth stays in env vars. JSON to stdout, errors to stderr.
- **One namespace per repo.** Updated in place on each merge to main via a high-water-mark diff. No historical search, no commit-keyed namespaces.

## Stack

| Concern | Choice |
|---|---|
| Language | Rust (cold start + tree-sitter integration) |
| Async | tokio (current_thread for search, multi_thread for indexer) |
| CLI | clap (derive) |
| Vector store | Turbopuffer (trait-swappable) |
| Embedder | Voyage `voyage-code-3` / Ollama / OpenAI (trait-swappable) |
| Summarizer | Anthropic Claude Haiku (trait-swappable) |
| Chunker | tree-sitter (8 languages) |
| Config | `~/.config/wdpkr/config.yaml` + env vars + CLI flags |

## Configuration

Four-layer resolution: `defaults → config file → env vars → CLI flags`.

```bash
wdpkr config init    # Write default config
wdpkr config list    # Show values + where each came from
```

Key env vars:
```
TURBOPUFFER_API_KEY     # vector storage (always required)
VOYAGE_API_KEY          # embedding (default provider)
ANTHROPIC_API_KEY       # summarization (always required)
WDPKR_NAMESPACE      # override auto-derived namespace
WDPKR_EMBED_PROVIDER # voyage | ollama | openai
```

## Roadmap

### Done

- [x] **Config module** — four-layer resolution, source attribution, `config list/get/set/init/edit/path`
- [x] **CLI foundation** — clap subcommands (search, index, config, init), command-aware tokio runtime
- [x] **Search vertical** — VectorStore + Embedder traits, mock implementations with real cosine similarity, search orchestration, JSON + `--pretty` output, end-to-end integration tests
- [x] **Chunker** — tree-sitter AST walker, per-language node-type maps (8 languages), doc-comment association, import extraction
- [x] **Summarizer** — Anthropic adapter with retry, prompt templates (file + symbol level), big-file roll-up
- [x] **Real embedder adapters** — Voyage, Ollama, OpenAI with bounded retry
- [x] **VectorStore adapter** — Turbopuffer with attribute-filtered search
- [x] **Indexer pipeline** — git diff, repo walker, chunk → summarize → embed → upsert, HWM tracking
- [x] **Init command** — CLAUDE.md/AGENTS.md section, .wdpkrignore, CI workflow generation
- [x] **Integration tests** — full index → search round-trip against temp git repos with mocks
- [x] **CI** — Forgejo workflow (fmt, clippy, test, release build)

### Remaining

- [ ] **Eval harness** — recall@k / MRR scoring against golden queries
- [ ] **Distribution** — CI release binaries, `cargo install` from crates.io

## Development

```bash
just test          # run all tests (318 currently)
just ci            # fmt-check + clippy + test
just run search "query"   # run from source
just release       # optimized binary
just release-all   # cross-platform (linux x86_64/arm64, macOS arm64)
```

## License

MIT OR Apache-2.0
