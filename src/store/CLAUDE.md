# Store adapter rules

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
- **Exact brute-force cosine.** nidus's only search mode. `Hit::score` is already
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
  prefix as `Predicate::Glob("file_path", "{prefix}*")`. nidus glob `*` **crosses
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
