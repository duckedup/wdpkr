# Store adapter rules

## DuckDB (local backend)

Local, file-backed store behind the default-on `duckdb` cargo feature
(`src/store/duckdb.rs`). Selected with `store.provider = duckdb`. Goal: run wdpkr
with no hosted third party.

- **One file, namespace = column.** A single DuckDB file holds every namespace in a
  shared `documents` table keyed by `(namespace, id)`, plus a `namespaces` metadata
  table and a `wdpkr_meta` row. `delete_namespace` is a `DELETE WHERE namespace = ?` —
  no dynamic DDL.
- **Exact brute-force search, no extension.** Vectors live in a fixed-size
  `FLOAT[dim]` ARRAY ranked by the **core** `array_cosine_distance` function — the
  `vss` extension is NOT required. Score mirrors Turbopuffer: `score = 1 - dist`
  (cosine similarity), so `min_score` and the output layer are identical across
  backends.
- **HNSW-ready, deferred.** Because the query is
  `... ORDER BY array_cosine_distance(...) LIMIT k` over a fixed-size ARRAY, adding an
  HNSW index later is a pure additive `CREATE INDEX ... USING HNSW` — no query or
  schema change. It is deliberately NOT done yet: the `vss` extension is not
  Cargo-bundleable (must vendor a version-matched binary), its persistent index needs
  an experimental flag with data-loss risk, is reloaded into RAM per process, and goes
  stale on delete.
- **Avoid driver ARRAY/LIST/MAP binding.** Only the `vector` column uses an ARRAY: it
  is written as an inlined numeric literal (`[..]::FLOAT[dim]`, numbers only →
  injection-safe) and read back via `CAST(vector AS VARCHAR)`. `calls`/`called_by`/
  metadata `extra` are JSON text; `NULL` distinguishes `None` from `Some(vec![])`.
- **One dimension per file.** `wdpkr_meta` pins the embedding dimension; reopening with
  a different dimension is a hard error. Use a separate `duckdb_path` or reindex.
- **Blocking driver.** The `duckdb` crate is synchronous; every `VectorStore` method
  runs its SQL inside `spawn_blocking`, locking the `Arc<Mutex<Connection>>` only inside
  the closure (never across `.await`).
- **Tests** use an in-memory connection and carry `#[cfg_attr(miri, ignore)]` (FFI). Miri
  runs `--no-default-features`, so this module is absent there.

## Turbopuffer: v2 API only

All Turbopuffer requests MUST use the v2 API (`/v2/namespaces/{ns}`). Do NOT use v1 endpoints or v1-only parameters.

### Common v1/v2 mistakes

- **`include_vectors`** is a v1 query parameter. In v2, request vectors via `include_attributes`: list `"vector"` alongside other attribute names. Never add `include_vectors` to `QueryRequest`.
- **Column-oriented payloads** are v1. v2 uses row-oriented `upsert_rows` (array of `HashMap<String, Value>`).
- **`top_k`** in query body is v1. v2 uses `limit`.

### Schema-safe attribute requests

Requesting a specific attribute name that doesn't exist in the namespace schema returns a 400 error. This happens when querying indexes built before a new field was added (e.g., `calls`/`called_by` on older indexes). Use `include_attributes: true` when you need all attributes and can't guarantee the schema has every column.

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
