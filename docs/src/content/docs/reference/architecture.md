---
title: Architecture
description: How the wdpkr codebase is organized and the conventions that hold it together.
---

wdpkr is a Rust CLI (Edition 2024, Rust 1.96+) that maintains a vector-search
index of LLM-generated code summaries. This page maps the source tree and the
patterns that recur across it.

## Two crates

The engine lives in a separate published crate,
[`wdpkr-core`](https://crates.io/crates/wdpkr-core): repo walking, chunking,
summarization, embedding, search, and the `VectorStore` trait. It is
storage-agnostic — it defines the store seam and ships no backend.

The `wdpkr` crate is the CLI over that engine, plus the concrete backends it
ships with. Core's modules are re-exported from `wdpkr`, so `wdpkr::chunk`,
`wdpkr::indexer`, and friends resolve to the engine's items.

```
wdpkr-core/src/
├── config/       # 4-layer resolution: defaults → file → env → CLI flags
├── chunk/        # tree-sitter AST chunking (8 languages)
├── ai_providers/ # voyage / openai / ollama (embed) + anthropic (summarize)
├── http/         # Shared reqwest retry: RetryPolicy + send_with_retry
├── summarize/    # Summarizer trait + prompts + big-file rollup
├── embed/        # Embedder trait + factory
├── store/        # VectorStore + StoreProvider traits + provider registry
├── search/       # Search orchestration + JSON/pretty output
├── indexer/      # Full pipeline: git diff → walk → chunk → summarize → embed → upsert
├── tap/          # Data sources: files, linear, notion
└── testing/      # Mocks (store, embedder, summarizer) + fixtures

wdpkr/src/
├── cli/          # Clap parsing + subcommand dispatch
├── config.rs     # Registers the backends, then delegates to core's resolution
└── store/        # The backends: Turbopuffer + nidus
```

## The store seam

Core defines `VectorStore` and `StoreProvider` and a process-global registry,
but implements neither — that is what keeps the engine backend-agnostic.
wdpkr registers Turbopuffer and nidus into that registry
(`store::register_backends`), and each backend declares the settings it needs
via `StoreProvider::settings`. Core then resolves those through the same
defaults → file → env chain as everything else, which is how
`wdpkr config list` prints `store.nidus.path` without core knowing what nidus
is.

Because settings resolve only for backends registered at the time, registration
must happen before config resolution — every entry point in `wdpkr::config`
does that first, which is why the CLI calls those rather than core's
constructors directly.

## Design conventions

- **Trait-first design.** `VectorStore`, `Embedder`, `Summarizer`, and
  `Chunker` are each a trait with both a mock and a real implementation. The
  pipeline depends on the traits, never the concrete providers — that's what
  makes backends swappable, and what lets the engine ship without any of them.
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
