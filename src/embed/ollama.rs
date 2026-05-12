//! Ollama local embedding adapter (`nomic-embed-text` default).
//!
//! Dimension is detected via a probe embed on initialization since Ollama
//! serves user-installed models whose dimensions aren't known statically.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::Deserialize;

use super::Embedder;
use crate::config::EmbedConfig;

const MAX_RETRIES: usize = 3;

pub struct OllamaEmbedder {
    client: reqwest::Client,
    host: String,
    model: String,
    dimension: usize,
}

impl OllamaEmbedder {
    /// Construct and probe. The probe sends a single short text to detect
    /// the model's embedding dimension — this is necessary because Ollama
    /// serves arbitrary user-installed models.
    pub async fn new(config: &EmbedConfig) -> Result<Self> {
        let client = reqwest::Client::new();
        let host = config.ollama_host.trim_end_matches('/').to_string();
        let model = config.model.clone();

        let probe = Self::embed_one(&client, &host, &model, "dimension probe")
            .await
            .context("Ollama dimension probe failed — is Ollama running and the model pulled?")?;
        let dimension = probe.len();
        if dimension == 0 {
            bail!("Ollama returned a zero-dimension embedding for model '{model}'");
        }

        Ok(Self {
            client,
            host,
            model,
            dimension,
        })
    }

    async fn embed_one(
        client: &reqwest::Client,
        host: &str,
        model: &str,
        text: &str,
    ) -> Result<Vec<f32>> {
        let body = serde_json::json!({
            "model": model,
            "input": text,
        });

        for attempt in 0..=MAX_RETRIES {
            let resp = client
                .post(format!("{host}/api/embed"))
                .json(&body)
                .send()
                .await;

            let resp = match resp {
                Ok(r) => r,
                Err(_) if attempt < MAX_RETRIES => {
                    tokio::time::sleep(backoff(attempt)).await;
                    continue;
                }
                Err(e) => return Err(e).context("Ollama API request failed"),
            };

            let status = resp.status();
            if status.is_success() {
                let api_resp: EmbedResponse =
                    resp.json().await.context("parsing Ollama response")?;
                return api_resp
                    .embeddings
                    .into_iter()
                    .next()
                    .context("Ollama returned empty embeddings");
            }

            if status.as_u16() >= 500 && attempt < MAX_RETRIES {
                tokio::time::sleep(backoff(attempt)).await;
                continue;
            }

            let error_body = resp.text().await.unwrap_or_default();
            bail!("Ollama API error ({status}): {error_body}");
        }
        bail!("Ollama API: max retries exceeded")
    }
}

#[async_trait]
impl Embedder for OllamaEmbedder {
    async fn embed(&self, text: &str) -> Result<Vec<f32>> {
        Self::embed_one(&self.client, &self.host, &self.model, text).await
    }

    async fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        // Ollama's /api/embed supports batch input in newer versions,
        // but for compatibility we process sequentially.
        let mut results = Vec::with_capacity(texts.len());
        for text in texts {
            results.push(self.embed(text).await?);
        }
        Ok(results)
    }

    fn dimension(&self) -> usize {
        self.dimension
    }
    fn max_input_tokens(&self) -> usize {
        8_192
    }
    fn provider_name(&self) -> &str {
        "ollama"
    }
    fn model_name(&self) -> &str {
        &self.model
    }
}

fn backoff(attempt: usize) -> Duration {
    Duration::from_millis(500 * 2u64.pow(attempt as u32))
}

#[derive(Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_parsing() {
        let json = r#"{"model":"nomic-embed-text","embeddings":[[0.1,0.2,0.3]]}"#;
        let resp: EmbedResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.embeddings.len(), 1);
        assert_eq!(resp.embeddings[0], vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn batch_response_parsing() {
        let json = r#"{"model":"nomic-embed-text","embeddings":[[0.1,0.2],[0.3,0.4],[0.5,0.6]]}"#;
        let resp: EmbedResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.embeddings.len(), 3);
    }

    #[test]
    fn backoff_timing() {
        assert_eq!(backoff(0), Duration::from_millis(500));
        assert_eq!(backoff(1), Duration::from_secs(1));
        assert_eq!(backoff(2), Duration::from_secs(2));
    }
}
