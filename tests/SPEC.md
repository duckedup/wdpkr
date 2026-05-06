# tests

Cross-module integration tests.

## Purpose

Exercises real flows that span multiple modules — config layering, full
indexing pipeline against a fixture repo, search round-trips against a mock
store. Per-module **unit** tests live alongside their code in
`#[cfg(test)] mod tests`; this directory is for tests that need to compose
modules.

## Plan

Initial test files (added as the corresponding modules become real):

- `config_resolution.rs` — defaults / file / env / CLI flag layering, using
  `serial_test` for env-var isolation. Mirrors the testing pattern from
  root `SPEC.md` § *Testing pattern*.
- `pipeline_against_fixture.rs` — small fixture repo committed under
  `tests/fixtures/`; mock embedder + mock store; verify the
  `chunk → summarize → embed → upsert` pipeline produces the expected
  `VectorDocument`s.
- `search_smoke.rs` — given a seeded mock store, verify `SearchRun::run`
  returns the expected file-and-symbols ordering.
- `eval_harness_smoke.rs` — load a single hand-crafted eval case and
  assert metric computation is correct against a deterministic mock store.

## Open questions

- Whether any integration test should hit a real Turbopuffer namespace
  (cost + flakiness + credential requirement). Leaning **fully mocked** —
  real-API smoke tests live outside the CI test suite, run manually.
- Fixture repo size — small enough to be non-noise (~10 source files, two
  languages) but representative enough to exercise the AST walker.
