---
title: Decision recall
description: Record architectural decisions once and have them surface automatically in search — the "why" behind the code.
---

Code search tells an agent *what* the code does. **Decision recall** tells it
*why*. A **decision** is an authored, ADR-style memory object — "we round half
up because payouts were off by a cent" — that wdpkr embeds and surfaces in
search: both as a direct hit and, crucially, attached to the code it governs.

Search `"commission rounding"` and you get the code **and** the decision that
explains it:

```jsonc
{
  "path": "src/finance/commission.rs",
  "score": 0.83,
  "governed_by": [
    { "path": "decision://0007", "title": "Half-up rounding for commission", "status": "accepted" }
  ],
  "symbols": [ /* … */ ]
}
```

## How it's stored

Decisions are **not files on disk**. They live in the *same* vector store as your
code index, in a dedicated `<namespace>--decision` namespace, with their
structured metadata (areas, author, status, relationships) kept in a registry
inside that namespace. This means:

- Decisions travel with your index and survive `wdpkr index` — indexing code
  never touches them (they have no upstream source to re-fetch from).
- They're the store's authoritative copy: on a local [`nidus`](/guides/storage/)
  store they're on disk; on Turbopuffer they're in your namespace.
- Removing or editing a decision is a single command — no re-stamping of code.

Because a decision references code by **glob** (`areas`) rather than by a
snapshot, its `governed_by` links stay correct as the code and the decision set
evolve — resolution happens at search time.

## Recording a decision

```bash
wdpkr decision add "Half-up rounding for commission" \
  --context "Payouts were off by a cent on odd totals" \
  --decision "Round half up at 2 decimals, ties away from zero" \
  --consequences "Matches the finance spec; differs from banker's rounding" \
  --area 'src/finance/**'
```

`--area` is what powers recall: any code result whose path matches the glob gets
this decision attached. Areas are repeatable and use the same glob syntax as
`--filter` (`**` crosses directory boundaries).

The author is filled in from `git config user.name` (override with `--author`),
so every decision — and every override — traces back to a person.

### Pulling provenance from a tap

A decision can pull its source material straight from a configured
[tap](/guides/taps/). The referenced document's content is snapshotted into the
decision (keeping it self-contained) and its URI is recorded under `sources`:

```bash
wdpkr decision add "Rounding policy" \
  --area 'src/finance/**' \
  --tap notion --doc https://www.notion.so/Rounding-Spec-399cb3ca…
```

Pull works through any tap that supports targeted fetch (Notion today).
`--doc` is repeatable to pull several documents from the same `--tap`.

## Relationships: supersede and override

Decisions form a small graph so an agent can trace how thinking evolved.

- **Supersede** — a new decision *replaces* an old one. The old decision is
  marked `superseded` and drops out of active recall, but stays in the store and
  is reachable via the `superseded_by` backlink.

  ```bash
  wdpkr decision add "Banker's rounding for commission" \
    --area 'src/finance/**' --supersedes 7
  ```

- **Override** — a narrower decision *wins over* a broader one in overlapping
  areas, without deactivating it. If a broad decision governs `src/**` and a
  narrow one governs `src/finance/**` and overrides it, only the narrow decision
  is attached to files under `src/finance/`.

  ```bash
  wdpkr decision add "Finance-specific error handling" \
    --area 'src/finance/**' --overrides 3
  ```

Use `--relates-to` for looser links that don't change ranking.

## Editing and removing

```bash
wdpkr decision edit 7 --area 'src/finance/commission*'   # re-scope
wdpkr decision edit 7 --status deprecated                # retire it
wdpkr decision delete 4                                  # delete + scrub links
wdpkr decision list --pretty                             # review the registry
```

`edit` changes only the fields you pass; content changes re-embed the decision.
`delete` (aliased `rm`) removes the decision's vectors and registry entry and any dangling
references to it from other decisions.

## In search

- Code results gain a `governed_by` array (active decisions whose `areas` match).
- Decisions also appear as their own results, tagged `"source": "decision"` with
  a `decision://<id>` path — so `wdpkr search "why do we round this way"` finds
  the decision directly.
- `--tap decision` searches only decisions; `--no-decisions` disables recall for
  a query.
- Superseded and deprecated decisions are excluded from active results but remain
  walkable through their relationship links.

:::tip[For agents]
When you make a non-obvious architectural choice, record it with
`wdpkr decision add --area <the code it governs>`. When search returns a
`governed_by` link, read that decision before changing the code it governs.
:::
