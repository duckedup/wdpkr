pub mod config;
pub mod decision;
pub mod delete;
pub mod eval;
pub mod index;
pub mod init;
pub mod prompt;
pub mod reinforce;
pub mod search;

pub use config::{ConfigArgs, ConfigCommand};
pub use decision::DecisionArgs;
pub use delete::DeleteArgs;
pub use eval::EvalArgs;
pub use index::IndexArgs;
pub use init::InitArgs;
pub use reinforce::ReinforceArgs;
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
    /// Delete vectors from the index by file path glob
    Delete(DeleteArgs),
    /// Mark document(s) as recently used, refreshing their decay ranking
    Reinforce(ReinforceArgs),
    /// Initialize wdpkr for a repository
    Init(InitArgs),
    /// Run evaluation suite to measure search quality and compression
    Eval(EvalArgs),
    /// Record and manage store-native architectural decisions
    Decision(DecisionArgs),
}

/// Dispatch to the appropriate subcommand handler.
pub async fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Config(args) => config::run(args).await,
        Command::Search(args) => search::run(args).await,
        Command::Index(args) => index::run(args).await,
        Command::Delete(args) => delete::run(args).await,
        Command::Reinforce(args) => reinforce::run(args).await,
        Command::Init(args) => init::run(args).await,
        Command::Eval(args) => eval::run(args).await,
        Command::Decision(args) => decision::run(args).await,
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
                assert!(args.scope.is_empty());
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
                assert_eq!(args.scope, vec!["internal/finance/".to_string()]);
                assert!(args.pretty);
            }
            _ => panic!("expected Search"),
        }
    }

    #[test]
    fn search_tap_defaults_empty() {
        let cli = Cli::try_parse_from(["wdpkr", "search", "q"]).unwrap();
        match cli.command {
            Command::Search(args) => assert!(args.tap.is_empty()),
            _ => panic!("expected Search"),
        }
    }

    #[test]
    fn search_parses_repeated_tap() {
        let cli =
            Cli::try_parse_from(["wdpkr", "search", "q", "--tap", "files", "--tap", "linear"])
                .unwrap();
        match cli.command {
            Command::Search(args) => {
                assert_eq!(args.tap, vec!["files".to_string(), "linear".to_string()]);
            }
            _ => panic!("expected Search"),
        }
    }

    #[test]
    fn search_provider_alias_still_works() {
        // `--provider` is a deprecated hidden alias for `--tap`.
        let cli = Cli::try_parse_from(["wdpkr", "search", "q", "--provider", "notion"]).unwrap();
        match cli.command {
            Command::Search(args) => assert_eq!(args.tap, vec!["notion".to_string()]),
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
                assert!(args.tap.is_none());
                assert!(!args.docstring);
            }
            _ => panic!("expected Index"),
        }
    }

    #[test]
    fn index_parses_docstring_flag() {
        let cli = Cli::try_parse_from(["wdpkr", "index", "--docstring"]).unwrap();
        match cli.command {
            Command::Index(args) => assert!(args.docstring),
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

    #[test]
    fn index_parses_tap_flag() {
        let cli = Cli::try_parse_from(["wdpkr", "index", "--tap", "files"]).unwrap();
        match cli.command {
            Command::Index(args) => {
                assert_eq!(args.tap.as_deref(), Some("files"));
            }
            _ => panic!("expected Index"),
        }
    }

    #[test]
    fn index_tap_with_no_value_errors() {
        assert!(Cli::try_parse_from(["wdpkr", "index", "--tap"]).is_err());
    }

    #[test]
    fn index_parses_repeated_doc() {
        let cli = Cli::try_parse_from([
            "wdpkr",
            "index",
            "--tap",
            "notion",
            "--doc",
            "https://app.notion.com/p/Spec-399cb3ca",
            "--doc",
            "7a2b9f",
        ])
        .unwrap();
        match cli.command {
            Command::Index(args) => {
                assert_eq!(args.tap.as_deref(), Some("notion"));
                assert_eq!(
                    args.doc,
                    vec![
                        "https://app.notion.com/p/Spec-399cb3ca".to_string(),
                        "7a2b9f".to_string()
                    ]
                );
            }
            _ => panic!("expected Index"),
        }
    }

    // ── reinforce ─────────────────────────────────────────────────────

    #[test]
    fn reinforce_requires_an_id() {
        assert!(Cli::try_parse_from(["wdpkr", "reinforce"]).is_err());
    }

    #[test]
    fn reinforce_parses_repeated_ids() {
        let cli =
            Cli::try_parse_from(["wdpkr", "reinforce", "notion://399cb3ca", "notion://7a2b9f"])
                .unwrap();
        match cli.command {
            Command::Reinforce(args) => {
                assert_eq!(
                    args.ids,
                    vec![
                        "notion://399cb3ca".to_string(),
                        "notion://7a2b9f".to_string()
                    ]
                );
            }
            _ => panic!("expected Reinforce"),
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
