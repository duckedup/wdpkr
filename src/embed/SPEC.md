# embed

Embedder trait + provider adapters.

## Purpose

Embeds natural-language summaries into dense vectors. Three v1
implementations: Voyage (default), Ollama (local), OpenAI (fallback).
The trait is finalized in root `SPEC.md` § *Embedder trait*.

## Public surface

- `#[async_trait] pub trait Embedder: Send + Sync`
  - `embed`, `embed_batch`, `dimension`, `max_input_tokens`,
    `provider_name`, `model_name`
- `pub struct VoyageEmbedder`
- `pub struct OllamaEmbedder`
- `pub struct OpenAiEmbedder`
- `pub fn build_embedder(cfg: &EmbedConfig) -> anyhow::Result<Box<dyn Embedder>>` — factory

## Files

- `mod.rs` — trait + factory + dimension lookup tables
- `voyage.rs` — `voyage-code-3` default (1024 dims, 16k token context)
- `ollama.rs` — `nomic-embed-text` default; dimension probed on init
- `openai.rs` — `text-embedding-3-large` default (3072 dims, 8191 tokens)

## Plan

1. Trait verbatim from SPEC.
2. Provider-specific batch limits handled internally:
   - Voyage: 128 / call
   - OpenAI: 2048 / call
   - Ollama: sequential (no batch endpoint)
3. Token-aware truncation with single stderr warning when input exceeds
   `max_input_tokens` — safety net only; summarizer should size outputs
   to fit.
4. Voyage / OpenAI dimension via hardcoded lookup; Ollama probes once on
   init and caches.
5. Factory dispatches on `cfg.provider` and validates required credentials
   are present.

## Open questions

- `voyage-code-3` vs `voyage-3-large` for our summary embeddings (root SPEC
  open Q #5) — eval-driven; both are easy to swap.
- Whether to cap retries inside the adapter or push that policy out to the
  indexer. Leaning: simple bounded exponential-backoff inside the adapter
  for transient HTTP failures; indexer-level retry only for cross-file
  reasoning (e.g., advance HWM only above success threshold).
