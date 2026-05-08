use anyhow::Result;
use clap::Args;

#[derive(Args, Debug)]
pub struct InitArgs {}

pub async fn run(_args: InitArgs) -> Result<()> {
    anyhow::bail!("megagrep init is not yet implemented")
}
