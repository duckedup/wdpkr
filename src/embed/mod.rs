//! Embedder abstraction.
//!
//! Defines the [`Embedder`] trait for embedding natural-language summaries
//! into dense vectors. v1 ships three adapters — Voyage (default), Ollama
//! (local/offline), OpenAI (fallback) — each in a separate file. A factory
//! function dispatches on [`EmbedConfig::provider`].
//!
//! Implementation tracks root `SPEC.md` § Embedder trait.

use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait Embedder: Send + Sync {
    /// Embed a single text string, returning a dense vector.
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;

    /// Embed a batch of texts. Implementations handle chunking against
    /// their provider's batch-size limits internally — callers pass all
    /// texts at once without worrying about per-provider caps.
    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;

    /// The dimensionality of vectors this embedder produces. For providers
    /// with fixed dimensions (Voyage, OpenAI), this is a constant looked
    /// up from a hardcoded map. For Ollama, detected via a probe embed
    /// on initialization.
    fn dimension(&self) -> usize;

    /// Max input tokens the model accepts. Text exceeding this is
    /// truncated (with a warning) before embedding.
    fn max_input_tokens(&self) -> usize;

    /// Provider name for logging and diagnostics (e.g. "voyage", "ollama").
    fn provider_name(&self) -> &str;

    /// Model identifier for logging and config validation.
    fn model_name(&self) -> &str;
}

/// Compact description of an embedder's identity, suitable for storing in
/// [`NamespaceMetadata::embedder`] to detect mismatches between indexing
/// and search. Format: `"provider/model"` (e.g. `"voyage/voyage-code-3"`).
pub fn embedder_identity(embedder: &dyn Embedder) -> String {
    format!("{}/{}", embedder.provider_name(), embedder.model_name())
}

/// Construct an [`Embedder`] from the resolved config. Dispatches on
/// `config.provider`. Returns an error if the provider is not yet
/// implemented — real adapters (Voyage, Ollama, OpenAI) plug in here
/// when their issues land.
pub fn build_embedder(config: &crate::config::EmbedConfig) -> Result<Box<dyn Embedder>> {
    anyhow::bail!(
        "embedder provider '{}' is not yet implemented (model: {})",
        config.provider,
        config.model,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn _assert_send<T: Send>() {}
    fn _assert_sync<T: Sync>() {}

    #[test]
    fn trait_is_object_safe() {
        fn _takes_embedder(_: &dyn Embedder) {}
    }

    #[test]
    fn embedder_identity_format() {
        struct FakeEmbedder;

        #[async_trait]
        impl Embedder for FakeEmbedder {
            async fn embed(&self, _text: &str) -> Result<Vec<f32>> {
                Ok(vec![0.0; 3])
            }
            async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
                Ok(texts.iter().map(|_| vec![0.0; 3]).collect())
            }
            fn dimension(&self) -> usize {
                3
            }
            fn max_input_tokens(&self) -> usize {
                8192
            }
            fn provider_name(&self) -> &str {
                "test"
            }
            fn model_name(&self) -> &str {
                "fake-v1"
            }
        }

        let e = FakeEmbedder;
        assert_eq!(embedder_identity(&e), "test/fake-v1");
        assert_eq!(e.dimension(), 3);
        assert_eq!(e.max_input_tokens(), 8192);
    }
}
