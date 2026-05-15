pub mod config;
pub mod index;
pub mod init;
pub mod prompt;
pub mod search;

pub use config::{ConfigArgs, ConfigCommand};
pub use index::IndexArgs;
pub use init::InitArgs;
pub use search::SearchArgs;

use anyhow::Result;
use clap::{Parser, Subcommand};

/// Exit codes per root SPEC § Exit codes.
pub mod exit {
    pub const SUCCESS: i32 = 0;
    pub const CONFIG_ERROR: i32 = 1;
    pub const BACKEND_ERROR: i32 = 2;
    pub const INDEX_MISSING: i32 = 3;
}

#[derive(Parser, Debug)]
#[command(
    name = "wdpkr",
    about = "Conceptual codebase search via vector retrieval over LLM-generated summaries",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Manage configuration
    Config(ConfigArgs),
    /// Search the codebase index
    Search(SearchArgs),
    /// Index the codebase
    Index(IndexArgs),
    /// Initialize wdpkr for a repository
    Init(InitArgs),
}

/// Dispatch to the appropriate subcommand handler.
pub async fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Config(args) => config::run(args).await,
        Command::Search(args) => search::run(args).await,
        Command::Index(args) => index::run(args).await,
        Command::Init(args) => init::run(args).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn no_subcommand_errors() {
        assert!(Cli::try_parse_from(["wdpkr"]).is_err());
    }

    #[test]
    fn unknown_subcommand_errors() {
        assert!(Cli::try_parse_from(["wdpkr", "bogus"]).is_err());
    }

    // ── search ────────────────────────────────────────────────────────

    #[test]
    fn search_requires_query() {
        assert!(Cli::try_parse_from(["wdpkr", "search"]).is_err());
    }

    #[test]
    fn search_parses_with_query_and_defaults() {
        let cli = Cli::try_parse_from(["wdpkr", "search", "find commission payments"]).unwrap();
        match cli.command {
            Command::Search(args) => {
                assert_eq!(args.query, "find commission payments");
                assert_eq!(args.top_k, 5);
                assert_eq!(args.symbols_per_file, 3);
                assert!(!args.no_symbols);
                assert!(args.scope.is_none());
                assert!(!args.pretty);
            }
            _ => panic!("expected Search"),
        }
    }

    #[test]
    fn search_parses_all_flags() {
        let cli = Cli::try_parse_from([
            "wdpkr",
            "search",
            "query",
            "-k",
            "10",
            "--symbols-per-file",
            "5",
            "--no-symbols",
            "--scope",
            "internal/finance/",
            "--pretty",
        ])
        .unwrap();
        match cli.command {
            Command::Search(args) => {
                assert_eq!(args.top_k, 10);
                assert_eq!(args.symbols_per_file, 5);
                assert!(args.no_symbols);
                assert_eq!(args.scope.as_deref(), Some("internal/finance/"));
                assert!(args.pretty);
            }
            _ => panic!("expected Search"),
        }
    }

    // ── index ─────────────────────────────────────────────────────────

    #[test]
    fn index_parses_with_defaults() {
        let cli = Cli::try_parse_from(["wdpkr", "index"]).unwrap();
        match cli.command {
            Command::Index(args) => {
                assert!(!args.full);
                assert!(!args.dry_run);
                assert_eq!(args.concurrency, 4);
                assert!(args.from.is_none());
                assert!(args.max_cost.is_none());
            }
            _ => panic!("expected Index"),
        }
    }

    #[test]
    fn index_parses_all_flags() {
        let cli = Cli::try_parse_from([
            "wdpkr",
            "index",
            "--full",
            "--dry-run",
            "--concurrency",
            "16",
            "--from",
            "abc123",
            "--max-cost",
            "100.0",
        ])
        .unwrap();
        match cli.command {
            Command::Index(args) => {
                assert!(args.full);
                assert!(args.dry_run);
                assert_eq!(args.concurrency, 16);
                assert_eq!(args.from.as_deref(), Some("abc123"));
                assert_eq!(args.max_cost, Some(100.0));
            }
            _ => panic!("expected Index"),
        }
    }

    // ── config ────────────────────────────────────────────────────────

    #[test]
    fn config_list_parses() {
        let cli = Cli::try_parse_from(["wdpkr", "config", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Config(ConfigArgs {
                command: ConfigCommand::List
            })
        ));
    }

    #[test]
    fn config_get_requires_key() {
        assert!(Cli::try_parse_from(["wdpkr", "config", "get"]).is_err());
    }

    #[test]
    fn config_get_parses_key() {
        let cli = Cli::try_parse_from(["wdpkr", "config", "get", "embedder.model"]).unwrap();
        match cli.command {
            Command::Config(ConfigArgs {
                command: ConfigCommand::Get { key },
            }) => assert_eq!(key, "embedder.model"),
            _ => panic!("expected Config Get"),
        }
    }

    #[test]
    fn config_set_requires_key_and_value() {
        assert!(Cli::try_parse_from(["wdpkr", "config", "set"]).is_err());
        assert!(Cli::try_parse_from(["wdpkr", "config", "set", "key"]).is_err());
    }

    #[test]
    fn config_set_parses_key_and_value() {
        let cli =
            Cli::try_parse_from(["wdpkr", "config", "set", "embedder.model", "voyage-3-large"])
                .unwrap();
        match cli.command {
            Command::Config(ConfigArgs {
                command: ConfigCommand::Set { key, value },
            }) => {
                assert_eq!(key, "embedder.model");
                assert_eq!(value, "voyage-3-large");
            }
            _ => panic!("expected Config Set"),
        }
    }

    #[test]
    fn config_init_parses() {
        let cli = Cli::try_parse_from(["wdpkr", "config", "init"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Config(ConfigArgs {
                command: ConfigCommand::Init
            })
        ));
    }

    #[test]
    fn config_edit_parses() {
        let cli = Cli::try_parse_from(["wdpkr", "config", "edit"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Config(ConfigArgs {
                command: ConfigCommand::Edit
            })
        ));
    }

    #[test]
    fn config_path_parses() {
        let cli = Cli::try_parse_from(["wdpkr", "config", "path"]).unwrap();
        assert!(matches!(
            cli.command,
            Command::Config(ConfigArgs {
                command: ConfigCommand::Path
            })
        ));
    }

    // ── init ──────────────────────────────────────────────────────────

    #[test]
    fn init_parses() {
        let cli = Cli::try_parse_from(["wdpkr", "init"]).unwrap();
        assert!(matches!(cli.command, Command::Init(_)));
    }
}
