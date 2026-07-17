---
title: Taps
description: Index data sources beyond code — Linear issues, Notion pages — into the same searchable index.
---

A **tap** is a data source wdpkr indexes. The default tap is `files`: it walks
your repository, chunks code with tree-sitter, and summarizes each symbol. But
the index isn't limited to code — any tap feeds the same summarize → embed →
search pipeline.

The motivating case: give an AI agent the *why* behind the code, not just the
*what*. Indexing your **Linear** issues — their descriptions and the discussion
in comments — lets an agent ask "why did we change the rate table?" and get the
decision, not just the diff. **Notion** pages add the specs and design docs that
sit behind the work.

Each non-`files` tap is indexed into its own namespace (`<namespace>--<tap>`) and
its results are tagged with a `source` field, so a single `wdpkr search` can span
code, issues, and docs while keeping them distinguishable.

Two ingest models, depending on the source:

- **Reconciled** (Linear): every run fetches the newest N items and that set
  *is* the desired index state — anything else is pruned.
- **Targeted, additive** (Notion): you name the exact documents to index, and
  nothing else is touched. Removal is explicit.

:::note[Decisions are not a tap]
[Decision recall](/guides/decisions/) also lives in a `<namespace>--decision`
namespace and shows up in search with `source: "decision"`, but it isn't a tap:
there's no external source to `index` from. Decisions are *authored* with
`wdpkr decision add` (which can pull provenance snapshots *from* taps like
Notion) and stored directly. `wdpkr index` never touches them.
:::

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
  - name: notion            # Notion pages (implementation specs)
    settings:
      # api_key: secret_...           # inline token (prefer NOTION_API_KEY)
      # api_key_env: NOTION_API_KEY   # env var holding the token (default)
      # notion_version: "2026-03-11"  # Notion-Version header (default)
      include_sections: true          # per-heading section children (default)
      decay:                          # per-tap search decay (opt-in)
        enabled: true
        half_life_days: 90            # score halves every 90 days unused
        floor: 0.4                    # never decays below 0.4× raw score
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

## The Notion tap

Indexes specific Notion pages — your implementation specs and design docs — on
demand. Unlike Linear, the Notion tap is **targeted and additive**: it fetches
only the page IDs you name and never prunes anything else. Removal is explicit.

Point it at pages by ID or URL with `--doc` (repeatable):

```bash
export NOTION_API_KEY=secret_...
wdpkr index --tap notion --doc <page-id-or-url> --doc <another>
```

Each page becomes one document keyed `notion://{page_id}`: a doc-level summary
vector plus — when `include_sections` is on — one **section child** per heading
(split on `heading_1/2/3`). That lets an agent match the exact section and use
the page ID to fetch the full document for context. Sub-pages (`child_page`
blocks) are **not** crawled; they're recorded as link references in the parent's
text. Index a sub-page explicitly with its own `--doc`.

| Setting | Default | Description |
| --- | --- | --- |
| `api_key` | — | Inline integration token. Takes precedence over `api_key_env`; keep the config file private |
| `api_key_env` | `NOTION_API_KEY` | Name of the env var holding the integration token |
| `notion_version` | `2026-03-11` | `Notion-Version` request header |
| `include_sections` | `true` | Emit per-heading section children alongside the doc-level summary |
| `decay` | — | Opt-in per-tap search decay (see below) |

Remove a stale spec explicitly with the `notion` namespace:

```bash
wdpkr delete --tap notion "notion://<page-id>*"
```

## Searching across taps

By default `wdpkr search` queries every configured tap and merges results by
score. Use `--tap` to narrow (repeatable):

```bash
wdpkr search "why did we change the rate table" --tap linear --pretty
wdpkr search "rate table lookup" --tap files          # code only
wdpkr search "auth spec" --tap notion --tap linear    # docs + issues
wdpkr search "rate table"                              # all taps, merged
```

Non-`files` results carry a `source` field and a scheme-prefixed `path`:

```json
{
  "path": "linear://ENG-123",
  "source": "linear",
  "score": 0.82,
  "summary": "Decision to switch the rate table to monthly buckets…"
}
```

(`--provider` is a deprecated alias for `--tap`.)

## Decay and reinforce

Docs go stale at a different rate than code. A tap can opt into **time decay** so
that documents nobody has touched in a while sink in the rankings — without ever
disappearing. Set a `decay` block under the tap's `settings`:

```yaml
- name: notion
  settings:
    decay:
      enabled: true
      half_life_days: 90   # default 90
      floor: 0.4           # default 0.4
```

At search time a result's score is multiplied by:

```
max(floor, 0.5 ^ (age_days / half_life_days))
```

where `age_days` is measured from the document's last index or reinforce time.
So a document's contribution halves every `half_life_days` it goes unused but
never drops below `floor × raw score` — stale specs rank lower yet stay
findable. Decay is a **ranking nudge only**; it never deletes anything.
`files` typically leaves decay off; `notion` and `linear` are good candidates
to turn it on.

When a search surfaces a document you actually relied on, tell wdpkr so it stops
decaying:

```bash
wdpkr reinforce notion://<page-id>
```

That bumps the document's `last_used_at` to now — a cheap metadata write, no
re-embedding — so the next search ranks it as fresh again. The id is the
result's `path`; the tap is inferred from the URI scheme (a bare path targets
the `files` namespace). Pass multiple ids to reinforce several at once. See
[`reinforce`](/reference/commands/#reinforce) for the command reference.
