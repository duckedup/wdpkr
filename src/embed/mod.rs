//! Embedder abstraction.
//!
//! Defines the [`Embedder`] trait for embedding natural-language summaries
//! into dense vectors. The concrete adapters live in [`crate::ai_providers`]
//! (Voyage, OpenAI, Ollama); [`build_embedder`] consults the provider
//! registry and dispatches on [`EmbedConfig::provider`].

use anyhow::{Result, bail};
use async_trait::async_trait;

use crate::ai_providers::{self, Capability};
use crate::config::EmbedConfig;

#[async_trait]
pub trait Embedder: Send + Sync {
    async fn embed(&self, text: &str) -> Result<Vec<f32>>;
    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;
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
    if !ai_providers::supports(&config.provider, Capability::Embed) {
        bail!(
            "provider '{}' is not a valid embedder; available embedders: {}",
            config.provider,
            ai_providers::names_with(Capability::Embed).join(", ")
        );
    }
    match config.provider.as_str() {
        "voyage" => Ok(Box::new(ai_providers::voyage::VoyageEmbedder::new(config)?)),
        "ollama" => Ok(Box::new(
            ai_providers::ollama::OllamaEmbedder::new(config).await?,
        )),
        "openai" => Ok(Box::new(ai_providers::openai::OpenAiEmbedder::new(config)?)),
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
            voyage_api_key: String::new(),
            openai_api_key: String::new(),
            ollama_host: "http://localhost:11434".into(),
        };
        assert!(build_embedder(&config).await.is_err());
    }
}
