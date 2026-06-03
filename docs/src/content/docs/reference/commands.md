---
title: CLI commands
description: Every wdpkr command and flag, with examples.
---

wdpkr is a single binary with a handful of subcommands. All output is JSON on
stdout; errors go to stderr.

```
wdpkr <command> [options]
```

| Command | Purpose |
| --- | --- |
| [`search`](#search) | Conceptual search — returns tiered JSON |
| [`index`](#index) | Build or refresh the index (full or incremental) |
| [`delete`](#delete) | Remove vectors from the index by path glob |
| [`init`](#init) | Set up wdpkr for a repository |
| [`config`](#config) | Manage configuration |
| [`eval`](#eval) | Measure search quality and compression |

## `search`

Embed a natural-language query, search the vector store, and return matching
files with their top symbols.

```bash
wdpkr search "release commission payments"
wdpkr search "how is rate limiting implemented" --pretty
wdpkr search "auth flow" --scope src/auth/ -k 10
```

| Flag | Default | Description |
| --- | --- | --- |
| `<query>` | *(required)* | Natural-language query |
| `-k`, `--top-k` | `5` | Max file-level results |
| `--symbols-per-file` | `3` | Max symbols returned per file |
| `--no-symbols` | off | File-level results only, omit symbol nesting |
| `--scope <path>` | — | Limit search to subtree(s); repeatable |
| `--filter <glob>` | — | Glob filter on result paths; repeatable (OR logic) |
| `--provider <name>` | all | Limit search to these tap sources (e.g. `files`, `linear`); repeatable |
| `--terse` | off | Compact output: paths + one-sentence summaries |
| `--pretty` | off | Human-readable output instead of JSON |

Results from a non-`files` tap carry a `source` field (e.g. `"source": "linear"`)
and a scheme-prefixed `path` (e.g. `linear://ENG-123`). See
[Taps](/guides/taps/) for indexing data sources beyond code.

## `index`

Walk the repo, chunk with tree-sitter, summarize, embed, and upsert to the
vector store. Incremental by default — only changed files are reprocessed.

```bash
wdpkr index              # incremental
wdpkr index --full       # rebuild everything
wdpkr index --dry-run    # estimate cost, no API calls
wdpkr index --tap linear # index only the configured Linear tap
```

| Flag | Default | Description |
| --- | --- | --- |
| `--full` | off | Ignore the high-water mark and reindex everything |
| `--dry-run` | off | Estimate cost without API calls or writes |
| `--concurrency <n>` | `4` | Bound parallel file processing |
| `--from <sha>` | — | Override the starting SHA for the diff (manual recovery) |
| `--max-cost <usd>` | — | Abort if estimated remaining cost exceeds this cap |
| `--skip-summaries` | off | Re-chunk and rebuild call-graph edges with zero API calls, reusing existing vectors |
| `--tap <name>` | all | Run only this configured tap |

## `delete`

Remove indexed vectors whose file paths match a glob.

```bash
wdpkr delete "src/legacy/**"
```

| Argument | Description |
| --- | --- |
| `<pattern>` | Glob matching file paths to remove from the index |

## `init`

Set wdpkr up in a repository — writes a `CLAUDE.md` section, a `.wdpkrignore`,
and a CI workflow that reindexes on merge to `main`.

```bash
wdpkr init
```

## `config`

Manage the configuration that the other commands resolve from. See
[Configuration](/guides/configuration/) for the full key reference.

```bash
wdpkr config init                       # write a default config file
wdpkr config list                       # show all values and their sources
wdpkr config get embedder.model         # read one value
wdpkr config set indexer.concurrency 16 # set one value
wdpkr config path                       # print the resolved config file path
wdpkr config edit                       # open the config file in $EDITOR
```

| Subcommand | Purpose |
| --- | --- |
| `init` | Write a default config file to `~/.config/wdpkr/config.yaml` |
| `get <key>` | Get a config value by dotted key |
| `set <key> <value>` | Set a config value by dotted key |
| `list` | Show all config values and their sources |
| `path` | Print the resolved config file path |
| `edit` | Open the config file in `$EDITOR` |

## `eval`

Run the evaluation suite to measure search quality and how much context wdpkr
saves versus reading source directly.

```bash
wdpkr eval
```
