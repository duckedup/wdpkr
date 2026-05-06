# store

Vector store abstraction + adapters.

## Purpose

Defines the `VectorStore` trait that the indexer (writes) and searcher
(reads) talk to. v1 ships a single Turbopuffer adapter; the trait is shaped
for future swap-in (Qdrant, Milvus, local FAISS).

## Public surface

The trait and supporting types are finalized in root `SPEC.md` §
*VectorStore trait*.

- `#[async_trait] pub trait VectorStore: Send + Sync` — namespace lifecycle,
  metadata r/w, upsert / delete-by-ids / delete-by-file, search
- `pub struct Namespace(pub String)`
- `pub struct NamespaceMetadata { hwm_sha, embedder, extra }`
- `pub struct VectorDocument` — id, vector, summary, filterable attributes
- `pub enum ChunkKind { File, Symbol }`
- `pub struct SearchOptions` — `top_k`, `path_prefix`, `chunk_kind`,
  `language`, `min_score`
- `pub struct SearchResult`, `UpsertStats`
- `pub struct TurbopufferStore`, `impl VectorStore for TurbopufferStore`

## Files

- `mod.rs` — trait + supporting types + ID-derivation helpers
- `turbopuffer.rs` — Turbopuffer adapter via `reqwest`

## Plan

1. Define types & trait verbatim from SPEC.
2. Deterministic IDs: `hash(file_path, chunk_kind, symbol_name, content_hash)`
   so re-upserts overwrite cleanly and the indexer can emit upserts without
   tracking prior IDs.
3. Turbopuffer adapter:
   - Namespace ops via Turbopuffer's HTTP API
   - Server-side attribute filters for `file_path`, `chunk_kind`, `language`,
     `symbol_kind`
   - `delete_by_file` uses attribute-filtered delete (single round trip)
   - Batched upserts internal to the adapter
   - HWM stored via `set_metadata` / `get_metadata` (namespace metadata, not
     a row)

## Open questions

- Validate Turbopuffer's attribute-filter coverage against our needs
  (root SPEC open Q #6). If any filter has to be done client-side, that's a
  surprise we want to surface early.
- Hash function for IDs — `blake3` (fast, modern) vs `sha2` (boring,
  ubiquitous). Leaning `blake3`.
- BM25 / hybrid search is **explicitly out of v1** — do not surface in the
  trait. Add later via an extension trait if eval shows it's worth it.
