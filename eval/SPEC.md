# eval (data)

Golden-query cases for the retrieval evaluator.

> Code lives in `src/eval/`. This directory holds case **data** only.

## Layout

- `cases/` — one file per case (`<id>.yaml` initially; format pending lock-in)

## Plan

Per root `SPEC.md` § *Evaluation*:

1. Source ~10 seed cases from real Linear tickets, with file annotations
   showing which paths turned out to be load-bearing.
2. Each case file:
   ```yaml
   id: LINEAR-1234
   query: "release commission payments to individual payees"
   relevant_paths:
     - internal/finance/commission/release.go
     - internal/finance/commission/release_test.go
   ```
3. Grow over time as queries surface where megagrep underperforms.

## Open questions

- **Schema lock-in** — pending. Will be resolved in `src/eval/cases.rs`.
- **Sensitivity**: Linear ticket text and customer-facing language may be
  sensitive. Likely policy:
  - One non-sensitive example case checked in (`example.yaml`)
  - Real cases gitignored by default; fetched from a private bucket or
    reproduced locally per developer
  - Decision deferred until first real cases are drafted
- Whether to keep eval cases per-team or per-repo if megagrep ends up used
  across many repos. v1 assumption: per-repo.
