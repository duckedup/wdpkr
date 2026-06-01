//! Voyage AI embedding adapter (`voyage-code-3` default).

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::Deserialize;

use crate::config::EmbedConfig;
use crate::embed::Embedder;
use crate::http::{self, RetryPolicy};

const MAX_BATCH: usize = 128;
const MAX_RETRIES: usize = 3;

pub struct VoyageEmbedder {
    client: reqwest::Client,
    api_key: String,
    model: String,
    dimension: usize,
    max_tokens: usize,
}

impl VoyageEmbedder {
    pub fn new(config: &EmbedConfig) -> Result<Self> {
        if config.voyage_api_key.is_empty() {
            bail!("VOYAGE_API_KEY is required for Voyage embedder");
        }
        Ok(Self {
            client: reqwest::Client::new(),
            api_key: config.voyage_api_key.clone(),
            model: config.model.clone(),
            dimension: dimension_for_model(&config.model),
            max_tokens: 16_000,
        })
    }

    async fn call_api(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let body = serde_json::json!({
            "model": self.model,
            "input": texts,
            "input_type": "document",
        });

        let policy = RetryPolicy::standard(MAX_RETRIES, 1000);
        let resp = http::send_with_retry(&policy, "Voyage API", || {
            self.client
                .post("https://api.voyageai.com/v1/embeddings")
                .bearer_auth(&self.api_key)
                .json(&body)
        })
        .await?;

        let status = resp.status();
        if status.is_success() {
            let api_resp: ApiResponse = resp.json().await.context("parsing Voyage response")?;
            return Ok(api_resp.data.into_iter().map(|d| d.embedding).collect());
        }

        let error_body = resp.text().await.unwrap_or_default();
        bail!("Voyage API error ({status}): {error_body}");
    }
}

#[async_trait]
impl Embedder for VoyageEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let results = self.call_api(&[text]).await?;
        results
            .into_iter()
            .next()
            .context("Voyage returned empty result")
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let mut all = Vec::with_capacity(texts.len());
        for chunk in texts.chunks(MAX_BATCH) {
            all.extend(self.call_api(chunk).await?);
        }
        Ok(all)
    }

    fn dimension(&self) -> usize {
        self.dimension
    }
    fn max_input_tokens(&self) -> usize {
        self.max_tokens
    }
    fn provider_name(&self) -> &str {
        "voyage"
    }
    fn model_name(&self) -> &str {
        &self.model
    }
}

fn dimension_for_model(model: &str) -> usize {
    match model {
        "voyage-code-3" => 1024,
        "voyage-3-large" => 1024,
        "voyage-3" => 1024,
        "voyage-3-lite" => 512,
        _ => 1024,
    }
}

#[derive(Deserialize)]
struct ApiResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimension_lookup() {
        assert_eq!(dimension_for_model("voyage-code-3"), 1024);
        assert_eq!(dimension_for_model("voyage-3-lite"), 512);
        assert_eq!(dimension_for_model("unknown-model"), 1024);
    }

    #[test]
    fn response_parsing() {
        let json = r#"{"data":[{"embedding":[0.1,0.2,0.3],"index":0}],"model":"voyage-code-3","usage":{"total_tokens":10}}"#;
        let resp: ApiResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.data.len(), 1);
        assert_eq!(resp.data[0].embedding, vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn batch_response_parsing() {
        let json = r#"{"data":[{"embedding":[0.1,0.2],"index":0},{"embedding":[0.3,0.4],"index":1}],"model":"voyage-code-3","usage":{"total_tokens":20}}"#;
        let resp: ApiResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.data.len(), 2);
    }

    #[test]
    fn constructor_requires_api_key() {
        let config = EmbedConfig {
            provider: "voyage".into(),
            model: "voyage-code-3".into(),
            batch_size: 64,
            voyage_api_key: String::new(),
            openai_api_key: String::new(),
            ollama_host: "http://localhost:11434".into(),
        };
        assert!(VoyageEmbedder::new(&config).is_err());
    }

    #[test]
    fn constructor_succeeds_with_key() {
        let config = EmbedConfig {
            provider: "voyage".into(),
            model: "voyage-code-3".into(),
            batch_size: 64,
            voyage_api_key: "test-key".into(),
            openai_api_key: String::new(),
            ollama_host: "http://localhost:11434".into(),
        };
        let e = VoyageEmbedder::new(&config).unwrap();
        assert_eq!(e.dimension(), 1024);
        assert_eq!(e.max_input_tokens(), 16_000);
        assert_eq!(e.provider_name(), "voyage");
        assert_eq!(e.model_name(), "voyage-code-3");
    }
}
