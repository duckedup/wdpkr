# megagrep

Conceptual codebase search for AI coding agents. Maintains a vector-search index of LLM-generated summaries and exposes a single `megagrep search` command that returns tiered, file+symbol results as JSON.

**megagrep is not a replacement for grep/ripgrep.** It's the conceptual layer on top — "where does the commission system live?" rather than "find the string `CommissionService`."

## How it works

```
megagrep search "release commission payments to individual payees"
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

The agent reads the actual files for ground truth. megagrep's job is to **point and describe**, not to ship source into the context window.

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
- **AST-driven chunking.** Tree-sitter parses files into semantically meaningful symbols (functions, types, traits) rather than arbitrary line splits.
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
| Config | `~/.config/megagrep/config.yaml` + env vars + CLI flags |

## Roadmap

### Done

- [x] **Config module** — four-layer resolution (defaults → file → env → CLI flags), source attribution, `config list/get/set/init/edit/path` commands, hard error on malformed config
- [x] **CLI foundation** — clap subcommands (search, index, config, init), command-aware tokio runtime
- [x] **Search vertical** — VectorStore + Embedder traits, mock implementations with real cosine similarity, search orchestration (embed → search → group → tiered JSON), output formatting (JSON + `--pretty`), end-to-end integration tests
- [x] **CI** — Forgejo workflow (fmt, clippy, test, release build)

### Next

- [ ] **Real embedder adapters** — Voyage, Ollama, OpenAI
- [ ] **VectorStore adapter** — Turbopuffer
- [ ] **Chunker** — tree-sitter AST walker, per-language node-type maps
- [ ] **Summarizer** — Anthropic adapter, prompt templates
- [ ] **Indexer pipeline** — git diff, repo walker, chunk → summarize → embed → upsert
- [ ] **Eval harness** — golden-query cases, recall@k, MRR
- [ ] **Init command** — CLAUDE.md, .megagrepignore, CI workflow generation
- [ ] **Distribution** — release binaries, cargo install

## Development

```bash
just test          # run all tests (173 currently)
just ci            # fmt-check + clippy + test
just run search "query"   # run from source
just release       # optimized binary
just release-all   # cross-platform (linux x86_64/arm64, macOS arm64)
```

## License

MIT OR Apache-2.0
