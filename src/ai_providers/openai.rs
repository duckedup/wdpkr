//! OpenAI embedding adapter (`text-embedding-3-large` default).

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::Deserialize;

use crate::config::EmbedConfig;
use crate::embed::Embedder;
use crate::http::{self, RetryPolicy};

const MAX_BATCH: usize = 2048;
const MAX_RETRIES: usize = 3;

pub struct OpenAiEmbedder {
    client: reqwest::Client,
    api_key: String,
    model: String,
    dimension: usize,
    max_tokens: usize,
}

impl OpenAiEmbedder {
    pub fn new(config: &EmbedConfig) -> Result<Self> {
        if config.openai_api_key.is_empty() {
            bail!("OPENAI_API_KEY is required for OpenAI embedder");
        }
        Ok(Self {
            client: reqwest::Client::new(),
            api_key: config.openai_api_key.clone(),
            model: config.model.clone(),
            dimension: dimension_for_model(&config.model),
            max_tokens: 8_191,
        })
    }

    async fn call_api(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        let body = serde_json::json!({
            "model": self.model,
            "input": texts,
        });

        let policy = RetryPolicy::standard(MAX_RETRIES, 1000);
        let resp = http::send_with_retry(&policy, "OpenAI API", || {
            self.client
                .post("https://api.openai.com/v1/embeddings")
                .bearer_auth(&self.api_key)
                .json(&body)
        })
        .await?;

        let status = resp.status();
        if status.is_success() {
            let api_resp: ApiResponse = resp.json().await.context("parsing OpenAI response")?;
            let mut results: Vec<_> = api_resp.data.into_iter().collect();
            results.sort_by_key(|d| d.index);
            return Ok(results.into_iter().map(|d| d.embedding).collect());
        }

        let error_body = resp.text().await.unwrap_or_default();
        bail!("OpenAI API error ({status}): {error_body}");
    }
}

#[async_trait]
impl Embedder for OpenAiEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let results = self.call_api(&[text]).await?;
        results
            .into_iter()
            .next()
            .context("OpenAI returned empty result")
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
        "openai"
    }
    fn model_name(&self) -> &str {
        &self.model
    }
}

fn dimension_for_model(model: &str) -> usize {
    match model {
        "text-embedding-3-large" => 3072,
        "text-embedding-3-small" => 1536,
        "text-embedding-ada-002" => 1536,
        _ => 3072,
    }
}

#[derive(Deserialize)]
struct ApiResponse {
    data: Vec<EmbeddingData>,
}

#[derive(Deserialize)]
struct EmbeddingData {
    embedding: Vec<f32>,
    index: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimension_lookup() {
        assert_eq!(dimension_for_model("text-embedding-3-large"), 3072);
        assert_eq!(dimension_for_model("text-embedding-3-small"), 1536);
        assert_eq!(dimension_for_model("text-embedding-ada-002"), 1536);
        assert_eq!(dimension_for_model("unknown"), 3072);
    }

    #[test]
    fn response_parsing_sorted_by_index() {
        let json = r#"{"data":[{"embedding":[0.3,0.4],"index":1},{"embedding":[0.1,0.2],"index":0}],"model":"text-embedding-3-large","usage":{"total_tokens":20}}"#;
        let resp: ApiResponse = serde_json::from_str(json).unwrap();
        let mut sorted: Vec<_> = resp.data.into_iter().collect();
        sorted.sort_by_key(|d| d.index);
        assert_eq!(sorted[0].embedding, vec![0.1, 0.2]);
        assert_eq!(sorted[1].embedding, vec![0.3, 0.4]);
    }

    #[test]
    fn constructor_requires_api_key() {
        let config = EmbedConfig {
            provider: "openai".into(),
            model: "text-embedding-3-large".into(),
            batch_size: 64,
            voyage_api_key: String::new(),
            openai_api_key: String::new(),
            ollama_host: "http://localhost:11434".into(),
        };
        assert!(OpenAiEmbedder::new(&config).is_err());
    }

    #[test]
    fn constructor_succeeds_with_key() {
        let config = EmbedConfig {
            provider: "openai".into(),
            model: "text-embedding-3-large".into(),
            batch_size: 64,
            voyage_api_key: String::new(),
            openai_api_key: "test-key".into(),
            ollama_host: "http://localhost:11434".into(),
        };
        let e = OpenAiEmbedder::new(&config).unwrap();
        assert_eq!(e.dimension(), 3072);
        assert_eq!(e.max_input_tokens(), 8_191);
        assert_eq!(e.provider_name(), "openai");
    }
}
