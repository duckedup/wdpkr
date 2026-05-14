use clap::Parser;
use wdpkr::cli::{self, Cli, Command};

fn main() {
    let cli = Cli::parse();

    // Per SPEC: current_thread for fast cold start on search/config/init;
    // multi_thread for the indexer's bounded-concurrency pipeline.
    let runtime = match &cli.command {
        Command::Index(_) => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime"),
        _ => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime"),
    };

    if let Err(e) = runtime.block_on(cli::dispatch(cli)) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}
