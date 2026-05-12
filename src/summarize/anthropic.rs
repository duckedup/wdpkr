//! Anthropic Messages API adapter for [`Summarizer`].
//!
//! Sends prompts from `prompts.rs` to Claude Haiku (default) via the
//! Messages API. Includes bounded exponential-backoff retry for transient
//! errors (429, 5xx).

use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::Deserialize;

use super::prompts;
use super::{FileSummaryInput, Summarizer, SymbolSummaryInput};
use crate::config::SummarizerConfig;

pub struct AnthropicSummarizer {
    client: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
    max_retries: usize,
}

impl AnthropicSummarizer {
    pub fn new(config: &SummarizerConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            client: reqwest::Client::new(),
            api_key: config.api_key.clone(),
            model: config.model.clone(),
            base_url: "https://api.anthropic.com".into(),
            max_retries: 3,
        })
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    async fn call_api(&self, user_message: &str) -> Result<String> {
        let url = format!("{}/v1/messages", self.base_url);
        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": 1024,
            "system": prompts::SYSTEM_PROMPT,
            "messages": [
                { "role": "user", "content": user_message }
            ]
        });

        let mut last_err = None;

        for attempt in 0..=self.max_retries {
            let result = self
                .client
                .post(&url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .json(&body)
                .send()
                .await;

            let resp = match result {
                Ok(r) => r,
                Err(e) => {
                    if attempt < self.max_retries {
                        let delay = backoff_delay(attempt);
                        eprintln!(
                            "warning: Anthropic API request failed (attempt {}): {e}, retrying in {delay:?}",
                            attempt + 1
                        );
                        tokio::time::sleep(delay).await;
                        last_err = Some(format!("{e}"));
                        continue;
                    }
                    return Err(e).context("Anthropic API request failed");
                }
            };

            let status = resp.status();
            if status.is_success() {
                let api_resp: ApiResponse = resp
                    .json()
                    .await
                    .context("parsing Anthropic API response")?;
                return extract_text(&api_resp);
            }

            if is_retryable(status.as_u16()) && attempt < self.max_retries {
                let delay = backoff_delay(attempt);
                let body_preview = resp.text().await.unwrap_or_default();
                eprintln!(
                    "warning: Anthropic API returned {status} (attempt {}): {}, retrying in {delay:?}",
                    attempt + 1,
                    body_preview.chars().take(200).collect::<String>()
                );
                tokio::time::sleep(delay).await;
                last_err = Some(format!("{status}"));
                continue;
            }

            let error_body = resp.text().await.unwrap_or_default();
            bail!("Anthropic API error ({status}): {error_body}");
        }

        bail!(
            "Anthropic API: max retries ({}) exceeded; last error: {}",
            self.max_retries,
            last_err.unwrap_or_else(|| "unknown".into())
        )
    }
}

#[async_trait]
impl Summarizer for AnthropicSummarizer {
    async fn summarize_file(&self, input: &FileSummaryInput) -> Result<String> {
        let msg = prompts::file_user_message(input);
        self.call_api(&msg).await
    }

    async fn summarize_symbol(&self, input: &SymbolSummaryInput) -> Result<String> {
        let msg = prompts::symbol_user_message(input);
        self.call_api(&msg).await
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

fn backoff_delay(attempt: usize) -> Duration {
    Duration::from_millis(1000 * 2u64.pow(attempt as u32))
}

fn is_retryable(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 529)
}

#[derive(Deserialize)]
struct ApiResponse {
    content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    _block_type: String,
    text: Option<String>,
}

fn extract_text(response: &ApiResponse) -> Result<String> {
    response
        .content
        .iter()
        .find_map(|block| block.text.clone())
        .context("Anthropic API response contained no text content")
}

/// Construct a [`Summarizer`] from the resolved config.
pub fn build_summarizer(config: &SummarizerConfig) -> Result<Box<dyn Summarizer>> {
    match config.provider.as_str() {
        "anthropic" => Ok(Box::new(AnthropicSummarizer::new(config)?)),
        other => bail!("summarizer provider '{other}' is not yet implemented"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::Import;

    // ── Response parsing ──────────────────────────────────────────────

    #[test]
    fn parse_successful_response() {
        let json = r#"{
            "id": "msg_123",
            "type": "message",
            "role": "assistant",
            "content": [
                {
                    "type": "text",
                    "text": "Commission payment release service handling individual and batch payouts."
                }
            ],
            "model": "claude-haiku-4-5-20251001",
            "stop_reason": "end_turn",
            "usage": { "input_tokens": 100, "output_tokens": 20 }
        }"#;
        let resp: ApiResponse = serde_json::from_str(json).unwrap();
        let text = extract_text(&resp).unwrap();
        assert!(text.contains("Commission payment release"));
    }

    #[test]
    fn parse_response_with_no_text_errors() {
        let json = r#"{ "content": [] }"#;
        let resp: ApiResponse = serde_json::from_str(json).unwrap();
        assert!(extract_text(&resp).is_err());
    }

    #[test]
    fn parse_response_skips_non_text_blocks() {
        let json = r#"{
            "content": [
                { "type": "tool_use", "text": null },
                { "type": "text", "text": "The actual summary." }
            ]
        }"#;
        let resp: ApiResponse = serde_json::from_str(json).unwrap();
        let text = extract_text(&resp).unwrap();
        assert_eq!(text, "The actual summary.");
    }

    // ── Retry classification ──────────────────────────────────────────

    #[test]
    fn retryable_status_codes() {
        assert!(is_retryable(429));
        assert!(is_retryable(500));
        assert!(is_retryable(502));
        assert!(is_retryable(503));
        assert!(is_retryable(529));
    }

    #[test]
    fn non_retryable_status_codes() {
        assert!(!is_retryable(200));
        assert!(!is_retryable(400));
        assert!(!is_retryable(401));
        assert!(!is_retryable(403));
        assert!(!is_retryable(404));
    }

    // ── Backoff timing ────────────────────────────────────────────────

    #[test]
    fn backoff_is_exponential() {
        assert_eq!(backoff_delay(0), Duration::from_secs(1));
        assert_eq!(backoff_delay(1), Duration::from_secs(2));
        assert_eq!(backoff_delay(2), Duration::from_secs(4));
        assert_eq!(backoff_delay(3), Duration::from_secs(8));
    }

    // ── Request construction ──────────────────────────────────────────

    #[test]
    fn file_prompt_builds_valid_message() {
        let input = FileSummaryInput {
            file_path: "src/payments.rs".into(),
            content: "pub fn pay() {}".into(),
            imports: vec![Import {
                module: "anyhow".into(),
                names: vec!["Result".into()],
            }],
            language: "rust".into(),
        };
        let msg = prompts::file_user_message(&input);
        assert!(msg.contains("src/payments.rs"));
        assert!(msg.contains("pub fn pay()"));
        assert!(msg.contains("anyhow"));
    }

    #[test]
    fn symbol_prompt_threads_file_summary() {
        let input = SymbolSummaryInput {
            symbol_name: "pay".into(),
            symbol_kind: "function".into(),
            body: "pub fn pay() {}".into(),
            signature: Some("pub fn pay()".into()),
            doc_comment: None,
            file_path: "src/payments.rs".into(),
            file_summary: "Payment processing service.".into(),
        };
        let msg = prompts::symbol_user_message(&input);
        assert!(msg.contains("Payment processing service."));
        assert!(msg.contains("pay"));
    }

    // ── Factory ───────────────────────────────────────────────────────

    #[test]
    fn build_summarizer_unknown_provider_errors() {
        let config = SummarizerConfig {
            provider: "openai".into(),
            model: "gpt-4".into(),
            api_key: "key".into(),
        };
        assert!(build_summarizer(&config).is_err());
    }

    #[test]
    fn build_summarizer_anthropic_without_key_errors() {
        let config = SummarizerConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5-20251001".into(),
            api_key: String::new(),
        };
        match build_summarizer(&config) {
            Ok(_) => panic!("should fail without API key"),
            Err(e) => assert!(e.to_string().contains("ANTHROPIC_API_KEY")),
        }
    }

    #[test]
    fn build_summarizer_anthropic_with_key_succeeds() {
        let config = SummarizerConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5-20251001".into(),
            api_key: "test-key".into(),
        };
        let s = build_summarizer(&config).unwrap();
        assert_eq!(s.model_name(), "claude-haiku-4-5-20251001");
    }

    // ── AnthropicSummarizer construction ──────────────────────────────

    #[test]
    fn custom_base_url() {
        let config = SummarizerConfig {
            provider: "anthropic".into(),
            model: "claude-haiku-4-5-20251001".into(),
            api_key: "key".into(),
        };
        let s = AnthropicSummarizer::new(&config)
            .unwrap()
            .with_base_url("http://localhost:8080");
        assert_eq!(s.base_url, "http://localhost:8080");
    }
}
