use anyhow::Result;
use clap::Args;

#[derive(Args, Debug)]
pub struct IndexArgs {
    /// Ignore high-water mark, full reindex
    #[arg(long)]
    pub full: bool,

    /// Estimate cost without making API calls or writing to the store
    #[arg(long)]
    pub dry_run: bool,

    /// Bound parallel API calls
    #[arg(long)]
    pub concurrency: Option<usize>,

    /// Override starting SHA for diff (manual recovery)
    #[arg(long)]
    pub from: Option<String>,

    /// Hard cost cap in USD; abort if estimated remaining cost exceeds this
    #[arg(long)]
    pub max_cost: Option<f64>,
}

pub async fn run(_args: IndexArgs) -> Result<()> {
    anyhow::bail!("megagrep index is not yet implemented")
}
