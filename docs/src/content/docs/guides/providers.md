---
title: Providers
description: The three external services wdpkr depends on, the defaults, and how to swap them.
---

wdpkr leans on three external roles: a **summarizer**, an **embedder**, and a
**vector store**. Each is a trait with a real implementation behind it, so any
of them can be swapped without touching the pipeline. The defaults are
production-ready.

| Role | Default | Alternatives | API key |
| --- | --- | --- | --- |
| Summarizer | Anthropic Claude Haiku | — | `ANTHROPIC_API_KEY` |
| Embedder | Voyage `voyage-code-3` | OpenAI, Ollama (local) | `VOYAGE_API_KEY` |
| Vector store | Turbopuffer | nidus (local, pure-Rust) | `TURBOPUFFER_API_KEY` |

All adapters share one shape: a `reqwest` HTTP client, bounded
exponential-backoff retry on 429/5xx, and a configurable base URL (which is how
the test suite drives them without live calls).

## Summarizer

Generates the natural-language summary for each chunk before it's embedded.
Defaults to Anthropic Claude Haiku — fast and cheap, which matters because
indexing summarizes every symbol in the repo.

```bash
wdpkr config set summarizer.model claude-haiku-4-5-20251001
```

## Embedder

Turns summaries (at index time) and queries (at search time) into vectors.
**The same model must be used for both** — that's what makes the vectors
comparable.

### Voyage (default)

```bash
export VOYAGE_API_KEY=...
wdpkr config set embedder.provider voyage
wdpkr config set embedder.model voyage-code-3
```

### OpenAI

```bash
export OPENAI_API_KEY=...
wdpkr config set embedder.provider openai
wdpkr config set embedder.model text-embedding-3-large
```

### Ollama (local, no API key)

Run embeddings entirely on your machine — nothing leaves the box:

```bash
wdpkr config set embedder.provider ollama
wdpkr config set embedder.ollama_host http://localhost:11434
wdpkr config set embedder.model mxbai-embed-large
```

:::tip
If you switch embedding providers or models, re-run `wdpkr index --full`.
Vectors from different models aren't comparable, so a mixed index returns
nonsense.
:::

## Vector store

Holds the embedded summaries and serves cosine-similarity search. Two backends,
selected by `store.provider`: hosted **Turbopuffer** (default) or local
pure-Rust **nidus** (no FFI / no C toolchain).

```bash
# hosted (default)
export TURBOPUFFER_API_KEY=...
wdpkr config set store.provider turbopuffer

# or local, no API key
wdpkr config set store.provider nidus
```

See [Storage](/guides/storage/) for the full comparison and configuration of
each backend.
