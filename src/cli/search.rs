use anyhow::Result;
use clap::Args;

#[derive(Args, Debug)]
pub struct SearchArgs {
    /// Natural-language query to search the codebase index
    pub query: String,

    /// Max file-level results
    #[arg(short = 'k', long, default_value_t = 5)]
    pub top_k: usize,

    /// Max symbols returned per file
    #[arg(long, default_value_t = 3)]
    pub symbols_per_file: usize,

    /// File-level results only, omit symbol nesting
    #[arg(long)]
    pub no_symbols: bool,

    /// Limit search to a subtree (path prefix)
    #[arg(long)]
    pub scope: Option<String>,

    /// Human-readable output instead of JSON
    #[arg(long)]
    pub pretty: bool,
}

pub async fn run(_args: SearchArgs) -> Result<()> {
    anyhow::bail!("megagrep search is not yet implemented")
}
