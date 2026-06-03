# wdpkr

Semantic code search for AI agents. Taps through your codebase to find exactly where things live.

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

- **Embed summaries, not code.** Off-the-shelf embedders are mediocre on conceptual queries against raw code. LLM-generated summaries close that gap.
- **AST-driven chunking.** Tree-sitter parses files into semantically meaningful symbols (functions, types, traits) rather than arbitrary line splits. Supports Rust, Go, TypeScript/TSX, JavaScript, Python, Java, C/C++, C#.
- **Pluggable backends.** Traits for VectorStore, Embedder, Summarizer, and Chunker — swap providers without changing the pipeline.
- **CLI, not MCP.** Any agent that can shell out can use it. JSON to stdout, errors to stderr.

## Install

```bash
cargo install wdpkr
```

Or from source:

```bash
git clone https://github.com/duckedup/wdpkr.git
cd wdpkr
cargo install --path .
```

## Quick start

```bash
# Initialize a repo (writes CLAUDE.md section, .wdpkrignore, CI workflow)
wdpkr init

# Configure providers and API keys
wdpkr config init

# Index the codebase
wdpkr index --full

# Search
wdpkr search "release commission payments"
wdpkr search "how is rate limiting implemented" --pretty
wdpkr search "auth flow" --scope src/auth/ -k 10
```

## Configuration

Four-layer resolution: `defaults → config file → env vars → CLI flags`.

```bash
wdpkr config init    # Interactive setup — choose providers, enter API keys
wdpkr config list    # Show effective values + where each came from
```

### Providers

wdpkr uses three external services. Each is trait-swappable — the defaults are production-ready but you can bring your own.

| Role | Default | Alternatives | API key |
|---|---|---|---|
| **Summarizer** | Anthropic Claude Haiku | — | `ANTHROPIC_API_KEY` |
| **Embedder** | Voyage `voyage-code-3` | OpenAI, Ollama (local) | `VOYAGE_API_KEY` |
| **Vector store** | Turbopuffer | DuckDB (local) | `TURBOPUFFER_API_KEY` |

### Environment variables

```
ANTHROPIC_API_KEY          # summarization (required)
TURBOPUFFER_API_KEY        # vector storage (required for the default store)
VOYAGE_API_KEY             # embedding (required for default provider)
WDPKR_EMBED_PROVIDER       # voyage | ollama | openai
WDPKR_STORE_PROVIDER       # turbopuffer | duckdb
WDPKR_DUCKDB_PATH          # DuckDB database file (store.provider=duckdb)
WDPKR_NAMESPACE            # override auto-derived namespace
```

All settings can also be set in `~/.config/wdpkr/config.yaml` via `wdpkr config set`.

### Local vector store (DuckDB)

For a fully local setup with no hosted vector database, use the DuckDB backend — a
single embedded file, no API key:

```bash
wdpkr config set store.provider duckdb
wdpkr config set store.duckdb.path ~/.local/share/wdpkr/wdpkr.duckdb   # optional; this is the default
```

Pair it with the Ollama embedder (`WDPKR_EMBED_PROVIDER=ollama`) to keep embeddings
local too. Provider-specific store settings are nested per backend in the config file:

```yaml
store:
  provider: duckdb
  turbopuffer:
    api_key: ...           # or TURBOPUFFER_API_KEY
  duckdb:
    path: ~/.local/share/wdpkr/wdpkr.duckdb   # or WDPKR_DUCKDB_PATH
```

Search is exact (brute-force cosine) and requires no DuckDB extension. The DuckDB
backend is compiled in by default; build with `--no-default-features` to exclude it.

### Data sources (taps)

wdpkr can index more than code. A **tap** is a data source; the default is `files`
(your repo). Configure a `taps:` list to add others — e.g. **Linear** issues, so an
agent can retrieve *why* a decision was made, not just the code that resulted:

```yaml
taps:
  - name: files
  - name: linear            # newest issues + comment threads
    settings:
      amount: 100
      order_by: updatedAt
```

```bash
export LINEAR_API_KEY=lin_api_...
wdpkr index --tap linear                                   # index just Linear
wdpkr search "why did we change the rate table" --provider linear
```

Non-`files` results carry a `source` field (`"source": "linear"`) and a
scheme-prefixed `path` (`linear://ENG-123`). `--provider` scopes a search to chosen
sources. See the [Taps guide](https://wdpkr.duckedup.org/guides/taps/) for the
full reference.

## Commands

| Command | Purpose |
|---|---|
| `wdpkr search "<query>"` | Semantic search — returns tiered JSON |
| `wdpkr search "<q>" --provider linear` | Scope search to specific tap sources |
| `wdpkr index [--full]` | Index the codebase (full or incremental) |
| `wdpkr index --tap linear` | Index only a configured tap (e.g. Linear) |
| `wdpkr index --dry-run` | Estimate tokens and cost without API calls |
| `wdpkr init` | Set up wdpkr for a repo (CLAUDE.md, .wdpkrignore, CI workflow) |
| `wdpkr config init` | Interactive config setup |
| `wdpkr config list` | Show all config values and their sources |
| `wdpkr config get <key>` | Get a single config value |
| `wdpkr config set <key> <val>` | Set a config value |

## Evaluation

wdpkr ships a retrieval eval harness. Cases live in `eval/cases/*.json` (query +
expected files/symbols); run them against a live index with:

```bash
wdpkr eval                          # default suite
wdpkr eval eval/cases/wdpkr-deep.json --tag keyword   # filter by tag
```

Each case reports **recall@k**, **MRR** (rank of the first relevant file),
**symbol recall**, and a **compression ratio** (result tokens ÷ tokens of the
files you'd otherwise read). A `By tag` breakdown slices the suite by query style.

### Results — docstring mode, local DuckDB

The numbers below are the `wdpkr-deep` suite (32 cases) run against wdpkr's own
codebase, indexed in **`--docstring` mode** (embeds code documentation +
signatures, **no LLM summaries**) with **`voyage-code-3`** into a local
**DuckDB** store.

| Metric | Value |
|---|---|
| Recall@5 (file) | **0.95** |
| MRR (file) | **0.81** |
| Symbol recall | 0.60 |
| Symbol MRR | 0.27 |
| Compression ratio | 0.25 |

By query style:

| Style | n | Recall@5 | MRR |
|---|---|---|---|
| keyword | 6 | 1.00 | 0.87 |
| where-is | 2 | 1.00 | 1.00 |
| natural-language | 20 | 0.97 | 0.84 |
| concept | 3 | 1.00 | 0.67 |

**Takeaways:** file-level retrieval is strong and top-heavy — the right file is
returned ~95% of the time and is usually rank 1–2, even with no LLM in the
indexing path. Keyword and "where is X" queries rank best; broad conceptual
phrasings rank lower. Symbol-level precision is the weakest spot (Symbol MRR
0.27): the correct *file* often ranks well before the correct *symbol* surfaces.

## Documentation

The docs site lives in [`docs/`](docs/) — Astro + Starlight, published to
[wdpkr.duckedup.org](https://wdpkr.duckedup.org) on merge to `main`. It uses
[Bun](https://bun.sh).

```bash
just docs          # dev server with live reload (http://localhost:4321)
just docs-build    # production build → docs/dist/
just docs-preview  # preview the production build
```

Or directly:

```bash
cd docs
bun install
bun run dev
```
