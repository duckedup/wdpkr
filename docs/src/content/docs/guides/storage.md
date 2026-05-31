---
title: Storage
description: The vector store backends wdpkr can use — hosted Turbopuffer or local file-backed DuckDB — and how to configure them.
---

The vector store holds wdpkr's embedded summaries and serves
cosine-similarity search. It's a trait (`VectorStore`) with two
implementations; pick one with `store.provider`.

| | **Turbopuffer** (default) | **DuckDB** |
| --- | --- | --- |
| Where | Hosted service | Local file on disk |
| Setup | API key | None |
| Best for | Shared/CI indexes, large repos | Local-only, offline, zero-dependency |
| `store.provider` | `turbopuffer` | `duckdb` |
| Credential | `TURBOPUFFER_API_KEY` | — |

Both rank by cosine similarity and return identical search results — only where
the vectors live differs.

## Turbopuffer

The default. A hosted vector database — vectors and metadata live in
Turbopuffer, keyed by a per-repo namespace. Good when the index is built in CI
and queried from many machines.

```bash
export TURBOPUFFER_API_KEY=...
wdpkr config set store.provider turbopuffer
# optional: store the key in the config file instead of the env var
wdpkr config set store.turbopuffer.api_key "$TURBOPUFFER_API_KEY"
```

| Setting | Env var | Default | Description |
| --- | --- | --- | --- |
| `store.provider` | `WDPKR_STORE_PROVIDER` | `turbopuffer` | Backend selector |
| `store.turbopuffer.api_key` | `TURBOPUFFER_API_KEY` | — | API key (prefer the env var) |

:::note
The nested `store.turbopuffer.api_key` is the supported key. The older flat
`store.turbopuffer_api_key` is still read for backwards compatibility, but
prefer the nested form.
:::

## DuckDB

A local, file-backed store — no external service and no API key. Everything
lives in a single DuckDB file, so the whole index is just a file you can copy,
back up, or delete. Good for local-only workflows and offline use.

```bash
wdpkr config set store.provider duckdb
# optional: choose where the file lives
wdpkr config set store.duckdb.path ~/.local/share/wdpkr/wdpkr.duckdb
```

| Setting | Env var | Default | Description |
| --- | --- | --- | --- |
| `store.provider` | `WDPKR_STORE_PROVIDER` | `turbopuffer` | Backend selector |
| `store.duckdb.path` | `WDPKR_DUCKDB_PATH` | `$XDG_DATA_HOME/wdpkr/wdpkr.duckdb` | Database file path |

The default path resolves to `~/.local/share/wdpkr/wdpkr.duckdb` (honoring
`$XDG_DATA_HOME`).

**How it works.** One DuckDB file holds every namespace — the namespace is a
column, not a separate file. Vectors are stored in a `FLOAT[dim]` array column
and ranked with the `vss` extension's `array_cosine_distance`, with
`score = 1 - distance` so results match the Turbopuffer adapter exactly.

:::caution
A DuckDB file is tied to a single **embedding dimension** (recorded in its
metadata). Opening it with a different dimension is a hard error — if you switch
embedding provider or model, re-run `wdpkr index --full` against a fresh path
(or delete the existing file first). Vectors from different models aren't
comparable anyway.
:::

The DuckDB backend is compiled in by default. Build with
`--no-default-features` to exclude it.

## Switching backends

Re-index after changing the store — the new backend starts empty:

```bash
wdpkr config set store.provider duckdb
wdpkr index --full
```

See [Configuration](/guides/configuration/) for how these settings resolve, and
[Providers](/guides/providers/) for the summarizer and embedder.
