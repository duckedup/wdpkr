//! wdpkr — the CLI over [`wdpkr_core`].
//!
//! The engine lives in `wdpkr-core`: repo walking, tree-sitter chunking, LLM
//! summarization, embedding, search, and the `VectorStore` seam. This crate is
//! what core deliberately leaves out — the command-line surface, and the two
//! concrete store backends wdpkr ships with (Turbopuffer and nidus).
//!
//! Core's modules are re-exported here unchanged, so `wdpkr::chunk`,
//! `wdpkr::indexer`, `wdpkr::testing`, and friends keep resolving to the same
//! items they always did.

// ── Re-exported engine ───────────────────────────────────────────────────
pub use wdpkr_core::{
    ai_providers, chunk, decision, embed, eval, http, indexer, search, summarize, tap, testing,
};

// ── wdpkr's own layers ───────────────────────────────────────────────────
pub mod cli;
pub mod config;
pub mod store;
