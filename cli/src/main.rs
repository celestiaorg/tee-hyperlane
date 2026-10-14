//! teeism: deploy, rotate, inspect and tear down the TEE ISM bridge.
//!
//! Every command is safe to re-run: each step reads what exists before creating anything, and
//! records what it created in devnet/.state/deployment.json as it goes. Any failure stops the
//! command with what it was doing and why; nothing is skipped silently.

mod build;
mod celestia;
mod chain;
mod chains;
mod cmd;
mod config;
mod ctx;
mod enclaves;
mod errors;
mod evm;
mod images;
mod isms;
mod mocha;
mod out;
mod paymaster;
mod pccs;
mod preflight;
mod run;
mod secrets;
mod services;
mod smoke;
mod state;
mod status;
mod teardown;
mod tomlx;
mod ui;
mod warp;

use clap::Parser;

#[tokio::main]
async fn main() {
    let cli = cmd::Cli::parse();
    if let Err(e) = cmd::dispatch(cli).await {
        out::error(format!("{e}"));
        for cause in e.chain().skip(1) {
            eprintln!("  caused by: {cause}");
        }
        std::process::exit(match e.downcast_ref::<cmd::Incomplete>() {
            Some(_) => 3,
            None => 1,
        });
    }
}
