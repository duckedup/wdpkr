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
| [`reinforce`](#reinforce) | Mark documents as freshly used so decay stops sinking them |
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
| `--tap <name>` | all | Limit search to these tap sources (e.g. `files`, `linear`, `notion`); repeatable. `--provider` is a deprecated alias |
| `--terse` | off | Compact output: paths + one-sentence summaries |
| `--pretty` | off | Human-readable output instead of JSON |

Results from a non-`files` tap carry a `source` field (e.g. `"source": "linear"`)
and a scheme-prefixed `path` (e.g. `linear://ENG-123`). See
[Taps](/guides/taps/) for indexing data sources beyond code, and
[Decay and reinforce](/guides/taps/#decay-and-reinforce) for how per-tap age
affects ranking.

## `index`

Walk the repo, chunk with tree-sitter, summarize, embed, and upsert to the
vector store. Incremental by default — only changed files are reprocessed.

```bash
wdpkr index              # incremental
wdpkr index --full       # rebuild everything
wdpkr index --dry-run    # estimate cost, no API calls
wdpkr index --tap linear # index only the configured Linear tap
wdpkr index --tap notion --doc <page-id-or-url>  # index specific Notion pages
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
| `--doc <id-or-url>` | — | Target document(s) for a targeted tap (the Notion tap); repeatable. Additive — only the named documents are (re)indexed |

## `reinforce`

Mark one or more documents as freshly used so per-tap [decay](/guides/taps/#decay-and-reinforce)
stops sinking them in the rankings. Bumps each document's `last_used_at` to now
— a cheap metadata write, no re-embedding.

```bash
wdpkr reinforce notion://<page-id>
wdpkr reinforce notion://<page-id> linear://ENG-123   # several at once
```

| Argument | Description |
| --- | --- |
| `<id>...` | One or more document ids (a result's `path`). The tap is inferred from the URI scheme; a bare path targets the `files` namespace |

Reinforce the specs an agent actually relied on so the next search ranks them
higher. Only meaningful for taps with decay enabled.

## `delete`

Remove indexed vectors whose paths match a glob.

```bash
wdpkr delete "src/legacy/**"                  # from the files namespace
wdpkr delete --tap notion "notion://<id>*"    # from a tap's namespace
```

| Flag / Argument | Default | Description |
| --- | --- | --- |
| `<pattern>` | *(required)* | Glob matching paths to remove. For non-`files` taps the paths are tap URIs (e.g. `notion://<id>*`) |
| `--tap <name>` | files | Delete from this tap's namespace instead of the base (files) namespace |

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
