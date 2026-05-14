use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use crate::config::{FileConfig, ResolvedConfig};

#[derive(Args, Debug)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Write default config file to ~/.config/wdpkr/config.yaml
    Init,
    /// Get a config value by dotted key (e.g. "embedder.model")
    Get {
        /// Dotted config key
        key: String,
    },
    /// Set a config value by dotted key
    Set {
        /// Dotted config key
        key: String,
        /// Value to set
        value: String,
    },
    /// Show all config values and their sources
    List,
    /// Open config file in $EDITOR
    Edit,
    /// Print the resolved config file path
    Path,
}

pub async fn run(args: ConfigArgs) -> Result<()> {
    match args.command {
        ConfigCommand::Init => run_init().await,
        ConfigCommand::Get { key } => run_get(&key).await,
        ConfigCommand::Set { key, value } => run_set(&key, &value).await,
        ConfigCommand::List => run_list().await,
        ConfigCommand::Edit => run_edit().await,
        ConfigCommand::Path => run_path().await,
    }
}

async fn run_init() -> Result<()> {
    let path = FileConfig::path()?;
    if path.exists() {
        bail!(
            "config file already exists at {}; use `wdpkr config edit` to modify it",
            path.display()
        );
    }

    println!("wdpkr config init — setting up ~/.config/wdpkr/config.yaml\n");

    // ── Vector store ──
    let store_provider = prompt_choice("Vector store provider", &["turbopuffer"], "turbopuffer")?;
    let turbopuffer_key = prompt_secret("Turbopuffer API key (TURBOPUFFER_API_KEY)")?;

    // ── Embedder ──
    let embed_provider = prompt_choice(
        "Embedding provider",
        &["voyage", "ollama", "openai"],
        "voyage",
    )?;
    let embed_model = match embed_provider.as_str() {
        "voyage" => prompt_choice(
            "Embedding model",
            &["voyage-code-3", "voyage-3-large", "voyage-3-lite"],
            "voyage-code-3",
        )?,
        "ollama" => prompt_choice("Embedding model", &["nomic-embed-text"], "nomic-embed-text")?,
        "openai" => prompt_choice(
            "Embedding model",
            &["text-embedding-3-large", "text-embedding-3-small"],
            "text-embedding-3-large",
        )?,
        _ => "voyage-code-3".into(),
    };

    let voyage_key = if embed_provider == "voyage" {
        prompt_secret("Voyage API key (VOYAGE_API_KEY)")?
    } else {
        String::new()
    };
    let openai_key = if embed_provider == "openai" {
        prompt_secret("OpenAI API key (OPENAI_API_KEY)")?
    } else {
        String::new()
    };

    // ── Summarizer ──
    let summarizer_provider = prompt_choice("Summarizer provider", &["anthropic"], "anthropic")?;
    let summarizer_model = prompt_choice(
        "Summarizer model",
        &["claude-haiku-4-5-20251001", "claude-sonnet-4-6"],
        "claude-haiku-4-5-20251001",
    )?;
    let anthropic_key = prompt_secret("Anthropic API key (ANTHROPIC_API_KEY)")?;

    // ── Build FileConfig ──
    let file_config = FileConfig {
        store: Some(crate::config::FileStoreConfig {
            provider: Some(store_provider),
            turbopuffer_api_key: non_empty(turbopuffer_key),
        }),
        embedder: Some(crate::config::FileEmbedConfig {
            provider: Some(embed_provider),
            model: Some(embed_model),
            voyage_api_key: non_empty(voyage_key),
            openai_api_key: non_empty(openai_key),
            ..Default::default()
        }),
        summarizer: Some(crate::config::FileSummarizerConfig {
            provider: Some(summarizer_provider),
            model: Some(summarizer_model),
            anthropic_api_key: non_empty(anthropic_key),
        }),
        ..Default::default()
    };

    let saved_path = file_config.save()?;
    println!("\nConfig saved to {}", saved_path.display());
    println!("API keys are stored in this file (mode 0600). Env vars override file values.");
    Ok(())
}

fn prompt_choice(label: &str, options: &[&str], default: &str) -> Result<String> {
    if options.len() == 1 {
        println!("{label}: {default}");
        return Ok(default.to_string());
    }
    eprint!("{label}");
    for (i, opt) in options.iter().enumerate() {
        let marker = if *opt == default { " (default)" } else { "" };
        eprint!("\n  {}) {opt}{marker}", i + 1);
    }
    eprint!("\nChoice [{}]: ", default);

    let input = read_line()?;
    if input.is_empty() {
        return Ok(default.to_string());
    }
    if let Ok(idx) = input.parse::<usize>()
        && idx >= 1
        && idx <= options.len()
    {
        return Ok(options[idx - 1].to_string());
    }
    if options.contains(&input.as_str()) {
        return Ok(input);
    }
    eprintln!("  Using default: {default}");
    Ok(default.to_string())
}

fn prompt_secret(label: &str) -> Result<String> {
    eprint!("{label} (Enter to skip): ");
    read_line()
}

fn read_line() -> Result<String> {
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .context("reading input")?;
    Ok(input.trim().to_string())
}

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() { None } else { Some(s) }
}

async fn run_get(key: &str) -> Result<()> {
    let resolved = ResolvedConfig::new()?;
    match resolved.get(key) {
        Some(entry) => {
            println!("{}", entry.value);
            Ok(())
        }
        None => bail!("unknown config key: {key}"),
    }
}

async fn run_set(key: &str, value: &str) -> Result<()> {
    let mut file = FileConfig::load()?.unwrap_or_default();
    file.set(key, value)?;
    let path = file.save()?;
    println!("{key} = {value}");
    println!("Saved to {}", path.display());
    Ok(())
}

async fn run_list() -> Result<()> {
    let resolved = ResolvedConfig::new()?;
    let entries = resolved.entries();
    let max_key = entries.iter().map(|e| e.key.len()).max().unwrap_or(0);
    for entry in &entries {
        println!(
            "{:<width$} = {:<25} [{}]",
            entry.key,
            entry.value,
            entry.source.label(),
            width = max_key
        );
    }
    Ok(())
}

async fn run_edit() -> Result<()> {
    let path = FileConfig::path()?;
    if !path.exists() {
        bail!(
            "no config file at {}; run `wdpkr config init` first",
            path.display()
        );
    }
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
    let status = std::process::Command::new(&editor)
        .arg(&path)
        .status()
        .with_context(|| format!("launching {editor}"))?;
    if !status.success() {
        bail!("{editor} exited with {status}");
    }
    Ok(())
}

async fn run_path() -> Result<()> {
    println!("{}", FileConfig::path()?.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_helpers::{remove_env, remove_envs, set_env};
    use serial_test::serial;
    use std::path::PathBuf;

    fn clear_and_setup(label: &str) -> PathBuf {
        remove_envs(&[
            "WDPKR_STORE_PROVIDER",
            "WDPKR_EMBED_PROVIDER",
            "WDPKR_EMBED_MODEL",
            "WDPKR_EMBED_BATCH_SIZE",
            "WDPKR_SUMMARIZER_PROVIDER",
            "WDPKR_SUMMARIZER_MODEL",
            "WDPKR_NAMESPACE",
            "WDPKR_DEFAULT_BRANCH",
            "WDPKR_CONCURRENCY",
            "WDPKR_MAX_COST",
            "WDPKR_HWM_SUCCESS_THRESHOLD",
            "TURBOPUFFER_API_KEY",
            "VOYAGE_API_KEY",
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "OLLAMA_HOST",
            "XDG_CONFIG_HOME",
        ]);
        let tmp = std::env::temp_dir().join(format!(
            "wdpkr-cli-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        set_env("XDG_CONFIG_HOME", tmp.to_str().unwrap());
        tmp
    }

    fn teardown(tmp: &std::path::Path) {
        remove_env("XDG_CONFIG_HOME");
        std::fs::remove_dir_all(tmp).ok();
    }

    #[tokio::test]
    #[serial]
    async fn path_prints_resolved_path() {
        let tmp = clear_and_setup("path");
        run(ConfigArgs {
            command: ConfigCommand::Path,
        })
        .await
        .unwrap();
        teardown(&tmp);
    }

    #[test]
    #[serial]
    fn file_config_save_writes_providers_and_keys() {
        let tmp = clear_and_setup("save-config");
        let file_config = FileConfig {
            store: Some(crate::config::FileStoreConfig {
                provider: Some("turbopuffer".into()),
                turbopuffer_api_key: Some("tp-key-123".into()),
            }),
            embedder: Some(crate::config::FileEmbedConfig {
                provider: Some("voyage".into()),
                model: Some("voyage-code-3".into()),
                voyage_api_key: Some("voy-key".into()),
                ..Default::default()
            }),
            summarizer: Some(crate::config::FileSummarizerConfig {
                provider: Some("anthropic".into()),
                model: Some("claude-haiku-4-5-20251001".into()),
                anthropic_api_key: Some("ant-key".into()),
            }),
            ..Default::default()
        };
        let path = file_config.save().unwrap();
        assert!(path.exists());

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("turbopuffer"), "store provider: {content}");
        assert!(content.contains("tp-key-123"), "store key: {content}");
        assert!(content.contains("voyage-code-3"), "embed model: {content}");
        assert!(content.contains("voy-key"), "voyage key: {content}");
        assert!(content.contains("ant-key"), "anthropic key: {content}");

        teardown(&tmp);
    }

    #[test]
    #[serial]
    fn api_key_resolves_from_file() {
        let tmp = clear_and_setup("key-from-file");
        let file_config = FileConfig {
            store: Some(crate::config::FileStoreConfig {
                provider: Some("turbopuffer".into()),
                turbopuffer_api_key: Some("file-tp-key".into()),
            }),
            summarizer: Some(crate::config::FileSummarizerConfig {
                anthropic_api_key: Some("file-ant-key".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        file_config.save().unwrap();

        let resolved = ResolvedConfig::new().unwrap();
        assert_eq!(resolved.config.store.api_key, "file-tp-key");
        assert_eq!(resolved.config.summarizer.api_key, "file-ant-key");

        teardown(&tmp);
    }

    #[test]
    #[serial]
    fn env_overrides_file_api_key() {
        use crate::config::test_helpers::set_env;
        let tmp = clear_and_setup("key-env-override");
        let file_config = FileConfig {
            store: Some(crate::config::FileStoreConfig {
                turbopuffer_api_key: Some("file-key".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        file_config.save().unwrap();

        set_env("TURBOPUFFER_API_KEY", "env-key");
        let resolved = ResolvedConfig::new().unwrap();
        assert_eq!(resolved.config.store.api_key, "env-key");
        assert_eq!(
            resolved.sources.store.api_key,
            crate::config::Source::Env("TURBOPUFFER_API_KEY")
        );

        teardown(&tmp);
    }

    #[tokio::test]
    #[serial]
    async fn get_returns_default_value() {
        let tmp = clear_and_setup("get-default");
        run(ConfigArgs {
            command: ConfigCommand::Get {
                key: "embedder.provider".into(),
            },
        })
        .await
        .unwrap();
        teardown(&tmp);
    }

    #[tokio::test]
    #[serial]
    async fn get_errors_on_unknown_key() {
        let tmp = clear_and_setup("get-unknown");
        let err = run(ConfigArgs {
            command: ConfigCommand::Get {
                key: "nonexistent.key".into(),
            },
        })
        .await
        .unwrap_err();
        assert!(err.to_string().contains("unknown config key"));
        teardown(&tmp);
    }

    #[tokio::test]
    #[serial]
    async fn set_then_get_round_trips() {
        let tmp = clear_and_setup("set-get");

        run(ConfigArgs {
            command: ConfigCommand::Set {
                key: "indexer.concurrency".into(),
                value: "32".into(),
            },
        })
        .await
        .unwrap();

        // Verify via the config module that the value persisted to the file
        // and resolves correctly through the full chain.
        let resolved = ResolvedConfig::new().unwrap();
        let entry = resolved.get("indexer.concurrency").unwrap();
        assert_eq!(entry.value, "32");
        assert_eq!(entry.source, crate::config::Source::File);

        teardown(&tmp);
    }

    #[tokio::test]
    #[serial]
    async fn set_invalid_value_errors() {
        let tmp = clear_and_setup("set-bad");
        let err = run(ConfigArgs {
            command: ConfigCommand::Set {
                key: "indexer.concurrency".into(),
                value: "not-a-number".into(),
            },
        })
        .await
        .unwrap_err();
        assert!(err.to_string().contains("indexer.concurrency"));
        teardown(&tmp);
    }

    #[tokio::test]
    #[serial]
    async fn set_unknown_key_errors() {
        let tmp = clear_and_setup("set-unknown");
        let err = run(ConfigArgs {
            command: ConfigCommand::Set {
                key: "bogus.key".into(),
                value: "whatever".into(),
            },
        })
        .await
        .unwrap_err();
        assert!(err.to_string().contains("unknown config key"));
        teardown(&tmp);
    }

    #[tokio::test]
    #[serial]
    async fn list_succeeds_with_defaults() {
        let tmp = clear_and_setup("list");
        run(ConfigArgs {
            command: ConfigCommand::List,
        })
        .await
        .unwrap();
        teardown(&tmp);
    }

    #[tokio::test]
    #[serial]
    async fn edit_errors_without_config_file() {
        let tmp = clear_and_setup("edit-missing");
        let err = run(ConfigArgs {
            command: ConfigCommand::Edit,
        })
        .await
        .unwrap_err();
        assert!(err.to_string().contains("no config file"));
        teardown(&tmp);
    }
}
