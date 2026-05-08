use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use crate::config::{DEFAULT_CONFIG_YAML, FileConfig, ResolvedConfig};

#[derive(Args, Debug)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Write default config file to ~/.config/megagrep/config.yaml
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
            "config file already exists at {}; use `megagrep config edit` to modify it",
            path.display()
        );
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    // Write the template verbatim (not via serde) to preserve comments.
    std::fs::write(&path, DEFAULT_CONFIG_YAML)
        .with_context(|| format!("writing {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    println!("Wrote default config to {}", path.display());
    Ok(())
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
            "no config file at {}; run `megagrep config init` first",
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
            "MEGAGREP_STORE_PROVIDER",
            "MEGAGREP_EMBED_PROVIDER",
            "MEGAGREP_EMBED_MODEL",
            "MEGAGREP_EMBED_BATCH_SIZE",
            "MEGAGREP_SUMMARIZER_PROVIDER",
            "MEGAGREP_SUMMARIZER_MODEL",
            "MEGAGREP_NAMESPACE",
            "MEGAGREP_DEFAULT_BRANCH",
            "MEGAGREP_CONCURRENCY",
            "MEGAGREP_MAX_COST",
            "MEGAGREP_HWM_SUCCESS_THRESHOLD",
            "TURBOPUFFER_API_KEY",
            "VOYAGE_API_KEY",
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "OLLAMA_HOST",
            "XDG_CONFIG_HOME",
        ]);
        let tmp = std::env::temp_dir().join(format!(
            "megagrep-cli-{label}-{}-{}",
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

    #[tokio::test]
    #[serial]
    async fn init_creates_default_config() {
        let tmp = clear_and_setup("init");
        run(ConfigArgs {
            command: ConfigCommand::Init,
        })
        .await
        .unwrap();

        let path = FileConfig::path().unwrap();
        assert!(path.exists(), "config file should exist after init");

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(
            content.contains("provider: turbopuffer"),
            "should contain default store provider"
        );
        assert!(
            content.contains("# Every field is optional"),
            "should preserve template comments"
        );

        teardown(&tmp);
    }

    #[tokio::test]
    #[serial]
    async fn init_errors_if_file_exists() {
        let tmp = clear_and_setup("init-exists");
        run(ConfigArgs {
            command: ConfigCommand::Init,
        })
        .await
        .unwrap();

        let err = run(ConfigArgs {
            command: ConfigCommand::Init,
        })
        .await
        .unwrap_err();
        assert!(err.to_string().contains("already exists"));

        teardown(&tmp);
    }

    #[tokio::test]
    #[serial]
    #[cfg(unix)]
    async fn init_sets_0600_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = clear_and_setup("init-perms");
        run(ConfigArgs {
            command: ConfigCommand::Init,
        })
        .await
        .unwrap();

        let path = FileConfig::path().unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

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
