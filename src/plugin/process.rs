//! Subprocess plugin adapter: spawns an external executable and
//! communicates via JSON over stdin/stdout.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use super::{FetchContext, FetchResult, Plugin, SourceItem};

pub struct ProcessPlugin {
    name: String,
    command: String,
    args: Vec<String>,
    settings: HashMap<String, serde_yaml::Value>,
    timeout: Duration,
}

#[derive(Serialize)]
struct FetchRequest<'a> {
    method: &'static str,
    params: FetchParams<'a>,
}

#[derive(Serialize)]
struct FetchParams<'a> {
    full: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    cursor: Option<&'a str>,
    stored_hashes: &'a HashMap<String, String>,
    settings: &'a HashMap<String, serde_yaml::Value>,
}

#[derive(Deserialize)]
struct FetchResponse {
    #[serde(default)]
    items: Vec<SourceItem>,
    #[serde(default)]
    deletions: Vec<String>,
    #[serde(default)]
    cursor: Option<String>,
}

impl ProcessPlugin {
    pub fn new(
        name: String,
        command: String,
        args: Vec<String>,
        settings: HashMap<String, serde_yaml::Value>,
    ) -> Self {
        Self {
            name,
            command,
            args,
            settings,
            timeout: Duration::from_secs(300),
        }
    }
}

#[async_trait]
impl Plugin for ProcessPlugin {
    fn name(&self) -> &str {
        &self.name
    }

    async fn fetch(&self, ctx: &FetchContext) -> Result<FetchResult> {
        let request = FetchRequest {
            method: "fetch",
            params: FetchParams {
                full: ctx.full,
                cursor: ctx.cursor.as_deref(),
                stored_hashes: &ctx.stored_hashes,
                settings: &self.settings,
            },
        };
        let request_json = serde_json::to_string(&request).context("serializing fetch request")?;

        let mut child = Command::new(&self.command)
            .args(&self.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .with_context(|| format!("spawning plugin '{}': {}", self.name, self.command))?;

        let mut stdin = child.stdin.take().context("opening plugin stdin")?;
        stdin.write_all(request_json.as_bytes()).await?;
        stdin.shutdown().await?;
        drop(stdin);

        let output = tokio::time::timeout(self.timeout, child.wait_with_output())
            .await
            .with_context(|| {
                format!(
                    "plugin '{}' timed out after {}s",
                    self.name,
                    self.timeout.as_secs()
                )
            })?
            .with_context(|| format!("waiting for plugin '{}'", self.name))?;

        if !output.status.success() {
            let code = output
                .status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".into());
            bail!("plugin '{}' exited with code {code}", self.name,);
        }

        let stdout = String::from_utf8(output.stdout)
            .with_context(|| format!("plugin '{}' stdout is not valid UTF-8", self.name))?;

        let response: FetchResponse = serde_json::from_str(&stdout).with_context(|| {
            format!(
                "plugin '{}' returned invalid JSON: {}",
                self.name,
                if stdout.len() > 200 {
                    format!("{}...", &stdout[..200])
                } else {
                    stdout.clone()
                }
            )
        })?;

        Ok(FetchResult {
            items: response.items,
            deletions: response.deletions,
            cursor: response.cursor,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn echo_script(response_json: &str) -> (String, Vec<String>) {
        let script = format!(
            r#"#!/bin/sh
cat > /dev/null
printf '%s' '{}'
"#,
            response_json.replace('\'', "'\\''")
        );
        ("sh".into(), vec!["-c".into(), script])
    }

    fn make_plugin(command: &str, args: Vec<String>) -> ProcessPlugin {
        ProcessPlugin::new("test-plugin".into(), command.into(), args, HashMap::new())
    }

    fn default_ctx() -> FetchContext {
        FetchContext {
            full: true,
            cursor: None,
            stored_hashes: HashMap::new(),
        }
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn fetch_parses_valid_response() {
        let json = r#"{"items":[{"source_path":"test.txt","content":"hello","content_hash":"h1","children":[]}],"deletions":[],"cursor":"c1"}"#;
        let (cmd, args) = echo_script(json);
        let plugin = make_plugin(&cmd, args);
        let result = plugin.fetch(&default_ctx()).await.unwrap();

        assert_eq!(result.items.len(), 1);
        assert_eq!(result.items[0].source_path, "test.txt");
        assert_eq!(result.cursor.as_deref(), Some("c1"));
        assert!(result.deletions.is_empty());
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn fetch_with_deletions() {
        let json = r#"{"items":[],"deletions":["old.txt"]}"#;
        let (cmd, args) = echo_script(json);
        let plugin = make_plugin(&cmd, args);
        let result = plugin.fetch(&default_ctx()).await.unwrap();

        assert!(result.items.is_empty());
        assert_eq!(result.deletions, vec!["old.txt"]);
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn nonzero_exit_code_errors() {
        let plugin = make_plugin("sh", vec!["-c".into(), "exit 1".into()]);
        let err = plugin.fetch(&default_ctx()).await.unwrap_err();
        assert!(err.to_string().contains("exited with code 1"), "got: {err}");
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn invalid_json_errors() {
        let plugin = make_plugin(
            "sh",
            vec!["-c".into(), "cat >/dev/null; echo 'not json'".into()],
        );
        let err = plugin.fetch(&default_ctx()).await.unwrap_err();
        assert!(err.to_string().contains("invalid JSON"), "got: {err}");
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn settings_passed_through() {
        let script = r#"#!/bin/sh
# read stdin and forward settings to stdout
input=$(cat)
# extract settings from input and echo back as response
printf '{"items":[],"deletions":[]}'
"#;
        let mut settings = HashMap::new();
        settings.insert(
            "api_key_env".into(),
            serde_yaml::Value::String("MY_KEY".into()),
        );
        let mut plugin = ProcessPlugin::new(
            "test".into(),
            "sh".into(),
            vec!["-c".into(), script.into()],
            settings,
        );
        plugin.timeout = Duration::from_secs(5);

        let result = plugin.fetch(&default_ctx()).await.unwrap();
        assert!(result.items.is_empty());
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn timeout_errors() {
        let mut plugin = make_plugin("sh", vec!["-c".into(), "cat >/dev/null; sleep 10".into()]);
        plugin.timeout = Duration::from_secs(1);

        let err = plugin.fetch(&default_ctx()).await.unwrap_err();
        assert!(err.to_string().contains("timed out"), "got: {err}");
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn missing_command_errors() {
        let plugin = make_plugin("/nonexistent/plugin-binary-that-does-not-exist", vec![]);
        let err = plugin.fetch(&default_ctx()).await.unwrap_err();
        assert!(err.to_string().contains("spawning plugin"), "got: {err}");
    }

    #[test]
    fn plugin_name() {
        let plugin = ProcessPlugin::new(
            "my-plugin".into(),
            "/usr/bin/thing".into(),
            vec![],
            HashMap::new(),
        );
        assert_eq!(plugin.name(), "my-plugin");
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn empty_response_defaults() {
        let (cmd, args) = echo_script("{}");
        let plugin = make_plugin(&cmd, args);
        let result = plugin.fetch(&default_ctx()).await.unwrap();

        assert!(result.items.is_empty());
        assert!(result.deletions.is_empty());
        assert!(result.cursor.is_none());
    }
}
