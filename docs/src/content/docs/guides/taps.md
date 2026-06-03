---
title: Taps
description: Index data sources beyond code — like Linear issues — into the same searchable index.
---

A **tap** is a data source wdpkr indexes. The default tap is `files`: it walks
your repository, chunks code with tree-sitter, and summarizes each symbol. But
the index isn't limited to code — any tap feeds the same summarize → embed →
search pipeline.

The motivating case: give an AI agent the *why* behind the code, not just the
*what*. Indexing your **Linear** issues — their descriptions and the discussion
in comments — lets an agent ask "why did we change the rate table?" and get the
decision, not just the diff.

Each non-`files` tap is indexed into its own namespace (`<namespace>--<tap>`) and
its results are tagged with a `source` field, so a single `wdpkr search` can span
code and issues while keeping them distinguishable.

## Configuring taps

Taps live under a `taps:` list in `~/.config/wdpkr/config.yaml`. Omit the list
entirely and you get the default `files` tap. List taps explicitly to add more:

```yaml
taps:
  - name: files             # repository code
  - name: linear            # Linear issues
    settings:
      amount: 100           # ingest the newest N issues (default 50)
      order_by: updatedAt   # 'updatedAt' (default) or 'createdAt'
      # team: ENG           # optional team-key filter
      include_comments: true
      # api_key_env: LINEAR_API_KEY   # env var holding the key (default)
```

## The Linear tap

Fetches the newest issues from the Linear GraphQL API and indexes one document
per issue: identifier, title, state, priority, labels, project, assignee, URL,
the description, and — when `include_comments` is on — the full comment thread,
all captured by a single summary.

Set your key in the environment, not the config file:

```bash
export LINEAR_API_KEY=lin_api_...
wdpkr index --tap linear
```

| Setting | Default | Description |
| --- | --- | --- |
| `amount` | `50` | How many of the newest issues to ingest |
| `order_by` | `updatedAt` | `updatedAt` or `createdAt` — both return most-recent-first |
| `team` | — | Optional team-key filter (e.g. `ENG`) |
| `include_comments` | `true` | Fold each issue's comment thread into its document |
| `api_key_env` | `LINEAR_API_KEY` | Name of the env var holding the API key |

### The index mirrors the newest issues

Every run fetches the newest `amount` issues — that complete set *is* the desired
index state. Anything previously indexed that isn't in that set is pruned:
**archived**, **deleted (trashed)**, **permanently removed**, and issues that
**aged out** of the newest-`amount` window are all deleted from the index. (A
failed fetch deletes nothing, so a transient API error can't wipe the index.)

### Cost

`wdpkr index --dry-run` includes the Linear tap: it does a free metadata read
(no LLM calls) and reports a **Linear issues: N** line plus the estimated
summarization cost — one summary per issue. With no `LINEAR_API_KEY` set, the
Linear estimate is skipped and the code estimate still runs.

## Searching across taps

By default `wdpkr search` queries every configured tap and merges results by
score. Use `--provider` to narrow:

```bash
wdpkr search "why did we change the rate table" --provider linear --pretty
wdpkr search "rate table lookup" --provider files     # code only
wdpkr search "rate table"                              # code + Linear, merged
```

Linear results look like:

```json
{
  "path": "linear://ENG-123",
  "source": "linear",
  "score": 0.82,
  "summary": "Decision to switch the rate table to monthly buckets…"
}
```

## What's next

The tap system is source-agnostic. Notion (via `notion-mg`) is a natural next
source for documents that change less often than issues — same pattern: a
namespace suffix, a `source` tag, and a `--provider` to scope search.
