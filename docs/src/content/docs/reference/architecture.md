---
title: Architecture
description: How the wdpkr codebase is organized and the conventions that hold it together.
---

wdpkr is a Rust CLI (Edition 2024, Rust 1.95+) that maintains a vector-search
index of LLM-generated code summaries. This page maps the source tree and the
patterns that recur across it.

## Module map

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

## Design conventions

- **Trait-first design.** `VectorStore`, `Embedder`, `Summarizer`, and
  `Chunker` are each a trait with both a mock and a real implementation. The
  pipeline depends on the traits, never the concrete providers — that's what
  makes backends swappable.
- **The `env_or` config pattern.** Every field resolves through
  `env_or_resolved(KEY, file_or_resolved(file_value, default))` — so each
  setting has a known environment variable, a file key, and a hardcoded
  default. See [Configuration](/guides/configuration/).
- **Shared adapter shape.** All external API adapters use the same pattern: a
  `reqwest` HTTP client, bounded exponential-backoff retry on 429/5xx, and a
  configurable base URL so tests never hit the network.
- **Errors.** `anyhow` at the binary boundary; traits return `anyhow::Result`.
- **Async runtime.** `tokio` — `current_thread` for search (fast cold start),
  `multi_thread` for indexing (parallel summarize/embed).

## Testing

The test suite is mock-based — no live API calls. Integration tests create
temporary git repos with fixture source files and exercise the real pipeline
against `MockEmbedder`, `MockVectorStore`, and `MockSummarizer`.

The suite also runs under [Miri](https://github.com/rust-lang/miri/) to catch
undefined behavior. Tests that cross an FFI boundary — tree-sitter, spawned
processes, system TLS, or the tokio reactor — are marked
`#[cfg_attr(miri, ignore)]`; pure-Rust tests using the mocks run under Miri
unchanged.

## Building from source

```bash
just test       # run all tests
just ci         # fmt-check + clippy (-D warnings) + test
just build      # debug build
just release    # optimized release build
just run <args> # run from source
```

The toolchain is pinned via `rust-toolchain.toml`.
