# Store adapter rules

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

### Reference

Turbopuffer v2 docs: https://turbopuffer.com/docs
