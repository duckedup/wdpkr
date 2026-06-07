---
title: Configuration
description: How wdpkr resolves settings — defaults, config file, environment, and CLI flags — and every key you can set.
---

wdpkr resolves every setting through four layers, each overriding the one
before it:

```
defaults  →  config file  →  environment variables  →  CLI flags
```

Run `config list` at any time to see the effective value of every setting and
exactly where it came from:

```bash
wdpkr config list
```

## Interactive setup

The fastest way to get configured is the guided walkthrough — it picks
providers and stores your API keys:

```bash
wdpkr config init
```

To set or read individual values:

```bash
wdpkr config get embedder.model
wdpkr config set indexer.concurrency 16
```

## The config file

Local settings live in `~/.config/wdpkr/config.yaml`. Every field is optional,
and environment variables override anything set here.

```yaml
store:
  provider: turbopuffer
  # Provider-specific settings live in nested blocks:
  # turbopuffer:
  #   api_key: ...                # prefer the TURBOPUFFER_API_KEY env var
  # nidus:                         # local, file-backed, pure-Rust store (provider: nidus)
  #   path: ~/.local/share/wdpkr/nidus          # store directory; or WDPKR_NIDUS_PATH env var

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

:::caution
Keep API keys **out** of the config file — set them via environment variables
(or `config set`, which stores them per-provider). Credentials in a checked-in
or shared file are a liability.
:::

## Settings reference

### Store

| Key | Default | Description |
| --- | --- | --- |
| `store.provider` | `turbopuffer` | Vector store backend — `turbopuffer` or `nidus` |
| `store.turbopuffer.api_key` | — | Turbopuffer API key (prefer the env var) |
| `store.nidus.path` | `~/.local/share/wdpkr/nidus` | nidus store directory (local backend) |

See [Storage](/guides/storage/) for the trade-offs between backends. The flat
`store.turbopuffer_api_key` key is still read for backwards compatibility, but
prefer the nested `store.turbopuffer.api_key`.

### Embedder

| Key | Default | Description |
| --- | --- | --- |
| `embedder.provider` | `voyage` | `voyage`, `openai`, or `ollama` |
| `embedder.model` | `voyage-code-3` | Provider-derived if unset |
| `embedder.batch_size` | `64` | Embeddings per request |
| `embedder.ollama_host` | `http://localhost:11434` | Ollama endpoint (local provider) |
| `embedder.voyage_api_key` | — | Voyage API key (prefer the env var) |
| `embedder.openai_api_key` | — | OpenAI API key (prefer the env var) |

### Summarizer

| Key | Default | Description |
| --- | --- | --- |
| `summarizer.provider` | `anthropic` | Summarization provider |
| `summarizer.model` | `claude-haiku-4-5-20251001` | Summarization model |
| `summarizer.anthropic_api_key` | — | Anthropic API key (prefer the env var) |

### Indexer

| Key | Default | Description |
| --- | --- | --- |
| `indexer.namespace` | *(auto)* | Index namespace; derived from the git remote when empty |
| `indexer.default_branch` | `main` | Branch the index tracks |
| `indexer.git_remote` | *(auto)* | Remote used to derive the namespace |
| `indexer.concurrency` | `8` | Parallel summarize/embed workers |
| `indexer.max_cost` | `50` | Hard cap (USD) on a single indexing run |
| `indexer.hwm_success_threshold` | `0.95` | Min success rate to advance the indexed high-water mark |

### Taps

Data sources to index. Omit `taps` to index repository code only (the default
`files` tap). Each entry has a `name` and optional per-tap `settings`. See
[Taps](/guides/taps/) for the full Linear tap reference.

```yaml
taps:
  - name: files
  - name: linear
    settings:
      amount: 100
      order_by: updatedAt
      include_comments: true
```

| Key | Default | Description |
| --- | --- | --- |
| `taps[].name` | `files` | Tap name — `files` or `linear` |
| `taps[].settings` | — | Per-tap settings (e.g. the Linear tap's `amount`, `order_by`, `team`, `include_comments`, `api_key_env`) |

Set the Linear API key via the `LINEAR_API_KEY` environment variable, not the
config file.

## Environment variables

Every setting has a matching environment variable, which overrides the config
file:

```
# Credentials
ANTHROPIC_API_KEY              # summarization (required)
TURBOPUFFER_API_KEY            # vector storage (required for default store)
VOYAGE_API_KEY                 # embedding (required for default provider)
OPENAI_API_KEY                 # embedding (OpenAI provider)
OLLAMA_HOST                    # embedding (Ollama provider)
LINEAR_API_KEY                 # Linear tap (when the linear tap is configured)

# Providers & models
WDPKR_STORE_PROVIDER           # turbopuffer | nidus
WDPKR_NIDUS_PATH               # nidus store directory (nidus store)
WDPKR_EMBED_PROVIDER           # voyage | openai | ollama
WDPKR_EMBED_MODEL
WDPKR_EMBED_BATCH_SIZE
WDPKR_SUMMARIZER_PROVIDER
WDPKR_SUMMARIZER_MODEL

# Indexing
WDPKR_NAMESPACE                # override the auto-derived namespace
WDPKR_DEFAULT_BRANCH
WDPKR_GIT_REMOTE
WDPKR_CONCURRENCY
WDPKR_MAX_COST
WDPKR_HWM_SUCCESS_THRESHOLD
```
