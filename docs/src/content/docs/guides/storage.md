---
title: Storage
description: The vector store backends wdpkr can use — hosted Turbopuffer or local pure-Rust nidus — and how to configure them.
---

The vector store holds wdpkr's embedded summaries and serves
cosine-similarity search. It's a trait (`VectorStore`) with two
implementations; pick one with `store.provider`.

| | **Turbopuffer** (default) | **nidus** |
| --- | --- | --- |
| Where | Hosted service | Local directory on disk |
| Setup | API key | None |
| Best for | Shared/CI indexes, large repos | Local-only, offline, zero-dependency |
| `store.provider` | `turbopuffer` | `nidus` |
| Credential | `TURBOPUFFER_API_KEY` | — |
| Dependencies | — | Pure Rust, **no FFI / no C toolchain** |

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

## nidus

A local, file-backed store — no external service and no API key. It's built on
[nidus](https://crates.io/crates/nidus), a small **pure-Rust** embeddable vector
store, so wdpkr compiles and links with **no FFI and no C/C++ toolchain** — which
is what lets it be vendored into other products cleanly. The whole index is just
a directory you can copy, back up, or delete. Good for local-only workflows and
offline use.

```bash
wdpkr config set store.provider nidus
# optional: choose where the store directory lives
wdpkr config set store.nidus.path ~/.local/share/wdpkr/nidus
```

| Setting | Env var | Default | Description |
| --- | --- | --- | --- |
| `store.provider` | `WDPKR_STORE_PROVIDER` | `turbopuffer` | Backend selector |
| `store.nidus.path` | `WDPKR_NIDUS_PATH` | `$XDG_DATA_HOME/wdpkr/nidus` | Store directory path |

The default path resolves to `~/.local/share/wdpkr/nidus` (honoring
`$XDG_DATA_HOME`).

**How it works.** One nidus directory holds every namespace — each wdpkr
namespace maps to a nidus collection. Search is exact brute-force cosine
similarity; nidus returns the cosine score directly, so `score` matches the
Turbopuffer adapter exactly and `min_score` behaves identically across backends.

:::caution
A nidus store is tied to a single **embedding dimension** (fixed when the
directory is created). Opening it with a different dimension is a hard error — if
you switch embedding provider or model, re-run `wdpkr index --full` against a
fresh path (or delete the existing directory first). Vectors from different
models aren't comparable anyway.
:::

## Switching backends

Re-index after changing the store — the new backend starts empty:

```bash
wdpkr config set store.provider nidus
wdpkr index --full
```

See [Configuration](/guides/configuration/) for how these settings resolve, and
[Providers](/guides/providers/) for the summarizer and embedder.
