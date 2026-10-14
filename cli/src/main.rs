//! teeism: deploy, rotate, inspect and tear down the TEE ISM bridge.
//!
//! Every command is safe to re-run: each step reads what exists before creating anything, and
//! records what it created in devnet/.state/deployment.json as it goes. Any failure stops the
//! command with what it was doing and why; nothing is skipped silently.

mod commands;
mod shared;
mod steps;

use clap::Parser;

#[tokio::main]
async fn main() {
    let cli = commands::Cli::parse();
    if let Err(e) = commands::dispatch(cli).await {
        shared::print::error(format!("{e}"));
        for cause in e.chain().skip(1) {
            eprintln!("  caused by: {cause}");
        }
        std::process::exit(match e.downcast_ref::<shared::incomplete::Incomplete>() {
            Some(_) => 3,
            None => 1,
        });
    }
}
