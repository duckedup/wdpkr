# Store adapter rules

The `VectorStore` / `StoreProvider` traits and the provider registry live in
`wdpkr-core`, which ships **no** backend. These two are wdpkr's, and they reach
the engine only by registering into that registry.

## Registering a backend

`register_backends()` in `mod.rs` is the single place backends are wired in. A
provider must also declare its settings via `StoreProvider::settings` — one
`SettingSpec` per key, carrying the file key, env var, default, secret flag,
and any deprecated flat aliases. Core resolves those through the normal
defaults → file → env chain, which is what makes `store.<backend>.<key>`
work in `config.yaml` and `wdpkr config list` without core knowing the backend
exists. Mark credentials `secret: true` so they are withheld from `config list`.

Settings resolve only for backends registered at the time config is resolved,
so registration has to come first — `crate::config`'s entry points handle that,
and the CLI goes through them rather than core's constructors.

## nidus (local backend)

Local, file-backed store (`src/store/nidus.rs`), selected with
`store.provider = nidus`. Goal: run wdpkr with no hosted third party **and no
FFI** — [`nidus`](https://crates.io/crates/nidus) is a pure-Rust embeddable
vector store, so the whole binary builds/links without a C/C++ toolchain. This
is what replaced the former bundled-DuckDB backend and lets the internal product
vendor wdpkr cleanly.

- **One directory, namespace = collection.** A single nidus directory holds every
  namespace; the wdpkr `Namespace` maps to a nidus collection (isolated id space).
  `delete_namespace` → `drop_collection`; create/exists → `create_collection`/
  `has_collection`. All ops are idempotent-guarded with `has_collection`.
- **Exact brute-force cosine.** nidus also offers ANN, quantization, and sealed
  segments, but wdpkr opens with all of them off (`Config` defaults), so the
  default search path *is* the exact scan — `SearchOpts::exact` is left at its
  default rather than forced. `Hit::score` is already
  cosine similarity, matching Turbopuffer's `score = 1 - distance`, so `min_score`
  and the output layer are identical across backends — no score transform.
- **Attributes are typed `Value`s, not SQL.** A `VectorDocument`'s fields become a
  `Record`'s `attrs` map: `Value::Str` (strings), `Value::Int` (line numbers),
  `Value::List` (calls/called_by). Optional fields are **omitted** from `attrs` when
  `None`; this is what preserves the not-indexed (`None`) vs empty (`Some(vec![])`)
  call-graph distinction — an absent key reads back `None`, a `List([])` reads back
  `Some(vec![])`. Namespace metadata (hwm_sha/embedder/extra) lives in nidus's
  per-collection `get_meta`/`set_meta` string map; `extra` is JSON-encoded.
- **AND-only filters; OR via merged searches.** nidus `Filter` is a conjunction of
  `Predicate`s. `chunk_kind`/`language` push down as `Predicate::Eq`; a single path
  prefix as `Predicate::IGlob("file_path", "{prefix}*")` — `IGlob` (nidus ≥ 0.43)
  folds ASCII case, so `--scope` pushdown means the same thing on both backends.
  Index-time deletes keep case-sensitive `Glob`: they mutate data and their paths
  come from the walker verbatim. nidus glob `*` **crosses
  `/`** (verified by test), matching DuckDB GLOB / Turbopuffer Glob, so a scope
  matches nested files. Multiple prefixes (OR semantics) can't be one filter, so
  they run as separate searches merged by id (best score), sorted, truncated to
  `top_k`.
- **One dimension per directory.** The dimension is fixed at open via
  `nidus::Config`; reopening a directory with a different dimension is a hard error
  (verified by test). Use a separate `store.nidus.path` or reindex.
- **Synchronous, `&mut` for writes.** nidus is sync and its writes need `&mut`, so
  the store wraps one `Nidus` in `Arc<Mutex<_>>` and runs every method inside
  `spawn_blocking`, locking only inside the closure (never across `.await`). Each
  mutating op `flush()`es so a reopened directory sees the data.
- **On-disk format stays v1.** nidus ≥ 0.60 has a format-version-2 manifest, but v2
  is only written by `Nidus::set_open_profile` / `nidus configure`, which wdpkr never
  calls — it opens with `open_dir` and takes the built-in defaults. So a wdpkr-written
  store stays v1-readable and older wdpkr builds keep opening it (verified against a
  0.43-written store: read back, searched, and re-opened by 0.43 after a 0.65
  write+flush). If wdpkr ever starts recording an open profile, that becomes a one-way
  upgrade and needs a release note.
- **Tests.** The conversion helpers (`to_record`/`record_to_doc`/meta map) are pure
  Rust and Miri-safe. The store tests use a tokio runtime (reactor FFI) so they
  carry `#[cfg_attr(miri, ignore)]` — nidus itself is pure Rust.

## Turbopuffer: v2 API only

All Turbopuffer requests MUST use the v2 API (`/v2/namespaces/{ns}`). Do NOT use v1 endpoints or v1-only parameters.

### Common v1/v2 mistakes

- **`include_vectors`** is a v1 query parameter. In v2, request vectors via `include_attributes`: list `"vector"` alongside other attribute names. Never add `include_vectors` to `QueryRequest`.
- **Column-oriented payloads** are v1. v2 uses row-oriented `upsert_rows` (array of `HashMap<String, Value>`).
- **`top_k`** in query body is v1. v2 uses `limit`.

### Schema-safe attribute requests

Requesting a specific attribute name that doesn't exist in the namespace schema returns a 400 error. This happens when querying indexes built before a new field was added (e.g., `calls`/`called_by` on older indexes). Use `include_attributes: true` when you need all attributes and can't guarantee the schema has every column.

The same schema strictness applies to **filters**: a filter (query *or* `delete_by_filter`) that references an attribute the namespace has never stored returns a 400 whose body contains `attribute not found` (e.g. ``filter error in key `file_path`: attribute not found``). A freshly-created namespace holds only the `__wdpkr_meta__` row, so `file_path`/`chunk_kind` don't exist until the first doc is upserted. `delete_by_file`/`delete_by_glob` therefore tolerate this specific error as a no-op (`is_missing_attribute_error`) — the rows they'd match cannot exist yet, so the delete-before-upsert on a first index must not fail. Only match this narrow substring; every other error (auth, rate-limit, namespace-not-found) must still surface.

### v2 query patterns

```rust
// Return specific attributes (including vectors when needed):
include_attributes: Some(json!(["vector", "file_path", "summary", ...]))

// Return all non-vector attributes:
include_attributes: Some(json!(true))

// Exclude vectors implicitly by listing only the attributes you need:
include_attributes: Some(json!(["file_path", "content_hash"]))
```

### Pagination

v2 has **no cursor-based pagination**. Do NOT add `cursor`/`next_cursor` fields. To page through all rows:

1. Order by ID: `rank_by: ["id", "asc"]`
2. After each page, filter with `["id", "Gt", last_id]`
3. Stop when the page returns fewer rows than the limit

### Reference

Turbopuffer v2 docs: https://turbopuffer.com/docs
