---
title: Getting started
description: Install wdpkr, configure providers, index your repo, and run your first semantic search.
---

wdpkr maintains a vector-search index of LLM-generated code summaries, so an
agent can ask *where* something lives in conceptual terms and get back the
right files and symbols — without pulling source into its context window.

This page takes you from zero to your first search.

## Install

```bash
cargo install wdpkr
```

Or build from source:

```bash
git clone https://github.com/duckedup/wdpkr.git
cd wdpkr
cargo install --path .
```

:::note
wdpkr requires Rust 1.96+ (Edition 2024). The toolchain is pinned via
`rust-toolchain.toml` when building from source.
:::

## Set up a repo

Run `init` from the root of the repository you want to index. It writes a
`CLAUDE.md` section, a `.wdpkrignore`, and a CI workflow so the index rebuilds
on merge to `main`.

```bash
wdpkr init
```

## Configure providers

wdpkr talks to three external services — a summarizer, an embedder, and a
vector store. Walk through them interactively:

```bash
wdpkr config init
```

At minimum you'll need the API keys for your chosen providers. The defaults
are production-ready:

| Role | Default | API key |
| --- | --- | --- |
| Summarizer | Anthropic Claude Haiku | `ANTHROPIC_API_KEY` |
| Embedder | Voyage `voyage-code-3` | `VOYAGE_API_KEY` |
| Vector store | Turbopuffer | `TURBOPUFFER_API_KEY` |

See [Providers](/guides/providers/) for the alternatives and how to swap them.

## Index the codebase

The first run is a full index:

```bash
wdpkr index --full
```

Want to know the cost before you spend it? Estimate tokens with no API calls:

```bash
wdpkr index --dry-run
```

Subsequent runs are incremental — wdpkr diffs against the last indexed commit
and only re-summarizes what changed.

## Search

```bash
wdpkr search "release commission payments"
wdpkr search "how is rate limiting implemented" --pretty
wdpkr search "auth flow" --scope src/auth/ -k 10
```

By default wdpkr emits JSON to stdout — ideal for an agent to parse. Add
`--pretty` for a human-readable view.

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

## Next steps

- [How it works](/guides/how-it-works/) — the pipelines behind index and search
- [Configuration](/guides/configuration/) — every setting and where it resolves from
- [CLI commands](/reference/commands/) — the full command reference
