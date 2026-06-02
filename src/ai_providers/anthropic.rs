//! Anthropic Messages API adapter for [`Summarizer`](crate::summarize::Summarizer).
//!
//! Sends prompts from `summarize::prompts` to Claude Haiku (default) via the
//! Messages API. Transient errors (429, 5xx) are retried with bounded
//! exponential backoff via [`crate::http`].

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::Deserialize;

use crate::config::SummarizerConfig;
use crate::http::{self, RetryPolicy};
use crate::summarize::prompts;
use crate::summarize::{FileSummaryInput, Summarizer, SymbolSummaryInput};

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

        let policy = RetryPolicy::standard(self.max_retries, 1000);
        let resp = http::send_with_retry(&policy, "Anthropic API", || {
            self.client
                .post(&url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", "2023-06-01")
                .json(&body)
        })
        .await?;

        let status = resp.status();
        if status.is_success() {
            let api_resp: ApiResponse = resp
                .json()
                .await
                .context("parsing Anthropic API response")?;
            return extract_text(&api_resp);
        }

        let error_body = resp.text().await.unwrap_or_default();
        bail!("Anthropic API error ({status}): {error_body}");
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

    // ── Construction ──────────────────────────────────────────────────

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
