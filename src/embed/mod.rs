//! Embedder abstraction.
//!
//! Defines the [`Embedder`] trait for embedding natural-language summaries
//! into dense vectors. Three adapters ship in v1:
//! - **Voyage** (`voyage.rs`) — default, code-trained model
//! - **Ollama** (`ollama.rs`) — local/offline, free iteration
//! - **OpenAI** (`openai.rs`) — widely available fallback
//!
//! [`build_embedder`] dispatches on [`EmbedConfig::provider`].

pub mod ollama;
pub mod openai;
pub mod voyage;

use anyhow::{Result, bail};
use async_trait::async_trait;

use crate::config::EmbedConfig;

#[async_trait]
pub trait Embedder: Send + Sync {
    /// Embed a document (the indexed side). Voyage tags these with
    /// `input_type: "document"`.
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;
    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;

    /// Embed a search query. Embedding models like Voyage produce better
    /// retrieval when queries are tagged `input_type: "query"` instead of
    /// `"document"`. Defaults to [`embed`](Self::embed) for providers that
    /// make no document/query distinction.
    async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        self.embed(text).await
    }

    fn dimension(&self) -> usize;
    fn max_input_tokens(&self) -> usize;
    fn provider_name(&self) -> &str;
    fn model_name(&self) -> &str;
}

pub fn embedder_identity(embedder: &dyn Embedder) -> String {
    format!("{}/{}", embedder.provider_name(), embedder.model_name())
}

/// Construct an [`Embedder`] from the resolved config. Ollama's constructor
/// is async (dimension probe), so the factory is async.
pub async fn build_embedder(config: &EmbedConfig) -> Result<Box<dyn Embedder>> {
    config.validate()?;
    match config.provider.as_str() {
        "voyage" => Ok(Box::new(voyage::VoyageEmbedder::new(config)?)),
        "ollama" => Ok(Box::new(ollama::OllamaEmbedder::new(config).await?)),
        "openai" => Ok(Box::new(openai::OpenAiEmbedder::new(config)?)),
        other => bail!("unknown embedder provider: '{other}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trait_is_object_safe() {
        fn _takes_embedder(_: &dyn Embedder) {}
    }

    #[test]
    fn embedder_identity_format() {
        struct Fake;
        #[async_trait]
        impl Embedder for Fake {
            async fn embed(&self, _: &str) -> Result<Vec<f32>> {
                Ok(vec![])
            }
            async fn embed_batch(&self, t: &[&str]) -> Result<Vec<Vec<f32>>> {
                Ok(t.iter().map(|_| vec![]).collect())
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
        assert_eq!(embedder_identity(&Fake), "test/fake-v1");
    }

    // ── Factory ───────────────────────────────────────────────────────

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn factory_voyage_with_key() {
        let config = EmbedConfig {
            provider: "voyage".into(),
            model: "voyage-code-3".into(),
            batch_size: 64,
            embed_mode: "summary".into(),
            voyage_api_key: "test-key".into(),
            openai_api_key: String::new(),
            ollama_host: "http://localhost:11434".into(),
        };
        let e = build_embedder(&config).await.unwrap();
        assert_eq!(e.provider_name(), "voyage");
        assert_eq!(e.dimension(), 1024);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn factory_openai_with_key() {
        let config = EmbedConfig {
            provider: "openai".into(),
            model: "text-embedding-3-large".into(),
            batch_size: 64,
            embed_mode: "summary".into(),
            voyage_api_key: String::new(),
            openai_api_key: "test-key".into(),
            ollama_host: "http://localhost:11434".into(),
        };
        let e = build_embedder(&config).await.unwrap();
        assert_eq!(e.provider_name(), "openai");
        assert_eq!(e.dimension(), 3072);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn factory_voyage_without_key_errors() {
        let config = EmbedConfig {
            provider: "voyage".into(),
            model: "voyage-code-3".into(),
            batch_size: 64,
            embed_mode: "summary".into(),
            voyage_api_key: String::new(),
            openai_api_key: String::new(),
            ollama_host: "http://localhost:11434".into(),
        };
        assert!(build_embedder(&config).await.is_err());
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn factory_unknown_provider_errors() {
        let config = EmbedConfig {
            provider: "cohere".into(),
            model: "embed-v3".into(),
            batch_size: 64,
            embed_mode: "summary".into(),
            voyage_api_key: String::new(),
            openai_api_key: String::new(),
            ollama_host: "http://localhost:11434".into(),
        };
        assert!(build_embedder(&config).await.is_err());
    }
}
