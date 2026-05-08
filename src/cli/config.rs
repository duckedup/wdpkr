use anyhow::Result;
use clap::{Args, Subcommand};

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
    anyhow::bail!("megagrep config init is not yet implemented")
}

async fn run_get(_key: &str) -> Result<()> {
    anyhow::bail!("megagrep config get is not yet implemented")
}

async fn run_set(_key: &str, _value: &str) -> Result<()> {
    anyhow::bail!("megagrep config set is not yet implemented")
}

async fn run_list() -> Result<()> {
    anyhow::bail!("megagrep config list is not yet implemented")
}

async fn run_edit() -> Result<()> {
    anyhow::bail!("megagrep config edit is not yet implemented")
}

async fn run_path() -> Result<()> {
    anyhow::bail!("megagrep config path is not yet implemented")
}
