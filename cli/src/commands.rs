//! Every `teeism` command, in the order a deploy runs them, and the file that implements it.
//!
//! | command                         | file                          |
//! |---------------------------------|-------------------------------|
//! | preflight                       | steps/preflight.rs            |
//! | build                           | steps/build.rs                |
//! | celestia up                     | steps/celestia/up.rs          |
//! | celestia core, celestia warp    | steps/celestia/hyperlane.rs   |
//! | mocha up                        | steps/celestia/mocha.rs       |
//! | images                          | steps/enclaves/images.rs      |
//! | enclaves up, identity, delete   | steps/enclaves/up.rs          |
//! | enclaves verify                 | steps/enclaves/verify.rs      |
//! | pccs deploy, tcb-eval           | steps/pccs/deploy.rs          |
//! | pccs seed                       | steps/pccs/seed.rs            |
//! | pccs status, check-quote        | steps/pccs/status.rs          |
//! | eden core                       | steps/eden.rs                 |
//! | isms evm, isms celestia         | steps/isms.rs                 |
//! | warp evm                        | steps/warp.rs                 |
//! | paymaster                       | steps/celestia/paymaster.rs   |
//! | config, services                | steps/services.rs             |
//! | ui                              | steps/ui.rs                   |
//! | smoke                           | steps/smoke.rs                |
//! | status                          | steps/status.rs               |
//! | teardown                        | steps/teardown.rs             |
//! | check-secrets                   | steps/check_secrets.rs        |
//!
//! This file only parses arguments and calls the step. Shared code is under `shared/`.

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::shared::settings::Ctx;
use crate::steps::pccs;

#[derive(Parser)]
#[command(
    name = "teeism",
    about = "Deploy, rotate, inspect and tear down the TEE ISM bridge",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Check every prerequisite (tools, keys, endpoints, balances) without changing anything.
    Preflight,
    /// Build celestia-appd and its helpers, the relayer, the gas oracle and every contract.
    Build,
    /// The bridge's own Celestia chain.
    Celestia {
        #[command(subcommand)]
        cmd: CelestiaCmd,
    },
    /// The mocha light node Eden's headers are read through.
    Mocha {
        #[command(subcommand)]
        cmd: MochaCmd,
    },
    /// Build, push and pin the enclave images (x86_64 Linux with Nix).
    Images {
        #[arg(long = "chain")]
        chains: Vec<String>,
    },
    /// The enclaves on Phala Cloud.
    Enclaves {
        #[command(subcommand)]
        cmd: EnclavesCmd,
    },
    /// Automata's on-chain PCCS and DCAP verifier on the EVM chains.
    Pccs {
        #[command(subcommand)]
        cmd: PccsCmd,
    },
    /// Hyperlane on Eden, which has no canonical deployment.
    Eden {
        #[command(subcommand)]
        cmd: EdenCmd,
    },
    /// The ISMs that admit attested messages.
    Isms {
        #[command(subcommand)]
        cmd: IsmsCmd,
    },
    /// The warp routes' EVM routers, enrolled both ways.
    Warp {
        #[command(subcommand)]
        cmd: WarpCmd,
    },
    /// The gas paymaster on Celestia and its owner key.
    Paymaster,
    /// Write the relayer's and the gas oracle's config from the deployment record.
    Config,
    /// The relayer and gas oracle systemd units.
    Services {
        #[command(subcommand)]
        cmd: ServicesCmd,
    },
    /// Build the bridge UI for this deployment and serve it through the gateway on :3000.
    Ui {
        /// How browsers reach this host, e.g. http://178.199.12.26:3000
        #[arg(long)]
        public_url: String,
    },
    /// Real transfers on every route, both assets, both directions, each waited on.
    Smoke {
        #[arg(long = "chain")]
        chains: Vec<String>,
        /// Per transfer, in base units (6 decimals).
        #[arg(long, default_value_t = 100_000)]
        amount: u128,
        #[arg(long, default_value_t = 30)]
        timeout_mins: u64,
    },
    /// What is deployed, and whether it answers.
    Status,
    /// Stop and delete everything this host runs for the bridge, and its CVMs.
    Teardown {
        /// Without this, only list what would be deleted.
        #[arg(long)]
        yes: bool,
    },
    /// Refuse credentials in tracked files.
    CheckSecrets {
        /// Only what is staged for commit.
        #[arg(long)]
        staged: bool,
        /// Install as the repository's pre-commit hook.
        #[arg(long)]
        install_hook: bool,
    },
}

#[derive(Subcommand)]
pub enum CelestiaCmd {
    /// Create the genesis (first run only) and start the node.
    Up {
        /// Restart a running node on the image the current celestia-app checkout builds.
        #[arg(long)]
        new_image: bool,
    },
    /// Hyperlane core: mailbox, merkle tree hook, noop ISM and hook.
    Core,
    /// The Celestia side of the warp routes: TIA collateral, teeUSD with its fixed supply.
    Warp,
}

#[derive(Subcommand)]
pub enum MochaCmd {
    /// Initialise (first run only) and start the light node.
    Up,
}

#[derive(Subcommand)]
pub enum EnclavesCmd {
    /// Deploy the shared CVM (or each --chain alone) and record the identities ISMs pin.
    Up {
        #[arg(long = "chain")]
        chains: Vec<String>,
        /// The dstack OS image; a different one is a different mr_td.
        #[arg(long, default_value = crate::steps::enclaves::quote::DEFAULT_OS_IMAGE)]
        os_image: String,
        /// Skip the check that the images are what this source builds and are pushed.
        #[arg(long)]
        unpinned: bool,
    },
    /// Check a running enclave against this checkout, link by link.
    Verify {
        /// `all` for the shared CVM, or a chain deployed alone.
        #[arg(long, default_value = "all")]
        target: String,
        #[arg(long)]
        app_id: Option<String>,
        /// Also compare with the ISM on this EVM chain.
        #[arg(long)]
        ism_chain: Option<String>,
        /// Rebuild the images with Nix and compare them with what the compose pins.
        #[arg(long)]
        rebuild: bool,
    },
    /// Re-read the identities ISMs pin from the recorded enclaves' quotes.
    Identity {
        #[arg(long = "chain")]
        chains: Vec<String>,
    },
    /// Delete a CVM on Phala: one by id, or every CVM a newer one replaced.
    Delete {
        #[arg(long, required_unless_present = "retired")]
        app_id: Option<String>,
        #[arg(long)]
        retired: bool,
    },
}

#[derive(Subcommand)]
pub enum EdenCmd {
    /// Deploy a Mailbox and MerkleTreeHook on Eden.
    Core,
}

#[derive(Subcommand)]
pub enum IsmsCmd {
    /// A TeeDcapIsm per EVM chain, pinning the Celestia enclave. Re-pins on an identity change.
    Evm {
        #[arg(long = "chain")]
        chains: Vec<String>,
    },
    /// An x/teeism ISM per EVM origin on Celestia, and the routing ISM. Re-pins on a change.
    Celestia {
        #[arg(long = "chain")]
        chains: Vec<String>,
    },
}

#[derive(Subcommand)]
pub enum WarpCmd {
    /// The TIA and teeUSD routers on each EVM chain, enrolled with Celestia both ways.
    Evm {
        #[arg(long = "chain")]
        chains: Vec<String>,
    },
}

#[derive(Subcommand)]
pub enum ServicesCmd {
    /// Install, enable and start the units (sudo), then wait for every route.
    Install,
    /// Rewrite the config, restart, and wait for every route.
    Restart,
    Stop,
}

#[derive(Subcommand)]
pub enum PccsCmd {
    /// Fetch Automata's Solidity dependencies and compile the PCCS and the verifier.
    Build,
    /// Deploy (or finish deploying) the PCCS and DCAP verifier, then seed it.
    Deploy {
        #[arg(long = "chain")]
        chains: Vec<String>,
        /// CREATE2 salt namespace. A new one is a new, independent deployment.
        #[arg(long)]
        namespace: Option<String>,
        #[arg(long, default_value = pccs::DEFAULT_FMSPC)]
        fmspc: String,
    },
    /// Publish Intel's current collateral (the monthly job).
    Seed {
        #[arg(long = "chain")]
        chains: Vec<String>,
        #[arg(long, default_value = pccs::DEFAULT_FMSPC)]
        fmspc: String,
        /// Check everything and fetch from Intel, but send nothing.
        #[arg(long)]
        dry_run: bool,
    },
    /// When each chain's collateral expires. Read only.
    Status {
        #[arg(long = "chain")]
        chains: Vec<String>,
        #[arg(long, default_value = pccs::DEFAULT_FMSPC)]
        fmspc: String,
        /// Report anything valid for fewer days than this.
        #[arg(long, default_value_t = 14)]
        warn_days: u64,
    },
    /// Add contracts for a TCB evaluation number, register them, and seed.
    TcbEval {
        #[arg(long = "chain")]
        chains: Vec<String>,
        number: u32,
        #[arg(long, default_value = pccs::DEFAULT_FMSPC)]
        fmspc: String,
    },
    /// Run a TDX quote through a chain's verifier, as its ISM will.
    CheckQuote {
        #[arg(long = "chain")]
        chains: Vec<String>,
        /// A file holding the quote as hex.
        #[arg(long, conflicts_with = "enclave")]
        quote: Option<std::path::PathBuf>,
        /// Fetch a fresh quote from this deployment's enclave for this chain (e.g. celestia).
        #[arg(long)]
        enclave: Option<String>,
    },
}

pub async fn dispatch(cli: Cli) -> Result<()> {
    let ctx = Ctx::load()?;
    crate::shared::evm::load_revert_names(&[
        &ctx.contracts_dir(),
        &pccs::pccs_dir(&ctx),
        &pccs::dcap_dir(&ctx),
    ]);
    let mut ctx = ctx;
    match cli.cmd {
        Cmd::Preflight => crate::steps::preflight::run(&ctx).await,
        Cmd::Build => crate::steps::build::build(&ctx),
        Cmd::Celestia { cmd } => match cmd {
            CelestiaCmd::Up { new_image } => crate::steps::celestia::up::run(&mut ctx, new_image).await,
            CelestiaCmd::Core => crate::steps::celestia::hyperlane::core(&ctx).await,
            CelestiaCmd::Warp => crate::steps::celestia::hyperlane::warp(&ctx).await,
        },
        Cmd::Mocha { cmd: MochaCmd::Up } => crate::steps::celestia::mocha::up(&ctx).await,
        Cmd::Images { chains } => crate::steps::enclaves::images::run(&ctx, &chains),
        Cmd::Enclaves { cmd } => match cmd {
            EnclavesCmd::Up {
                chains,
                os_image,
                unpinned,
            } => {
                crate::steps::enclaves::up::run(
                    &ctx,
                    crate::steps::enclaves::up::Args {
                        chains,
                        unpinned,
                        os_image,
                    },
                )
                .await
            }
            EnclavesCmd::Verify {
                target,
                app_id,
                ism_chain,
                rebuild,
            } => {
                crate::steps::enclaves::verify::run(
                    &ctx,
                    crate::steps::enclaves::verify::Args {
                        target,
                        app_id,
                        ism_chain,
                        rebuild,
                    },
                )
                .await
            }
            EnclavesCmd::Identity { chains } => crate::steps::enclaves::up::identity(&ctx, chains).await,
            EnclavesCmd::Delete { app_id, retired } => {
                crate::steps::enclaves::up::delete_cmd(&ctx, app_id, retired)
            }
        },
        Cmd::Pccs { cmd } => match cmd {
            PccsCmd::Build => crate::steps::build::automata(&ctx),
            PccsCmd::Deploy {
                chains,
                namespace,
                fmspc,
            } => pccs::deploy::run(&ctx, &chains, namespace, &fmspc).await,
            PccsCmd::Seed {
                chains,
                fmspc,
                dry_run,
            } => pccs::seed::run(&ctx, &chains, &fmspc, dry_run).await,
            PccsCmd::Status {
                chains,
                fmspc,
                warn_days,
            } => pccs::status::run(&ctx, &chains, &fmspc, warn_days).await,
            PccsCmd::TcbEval {
                chains,
                number,
                fmspc,
            } => pccs::deploy::tcb_eval(&ctx, &chains, number, &fmspc).await,
            PccsCmd::CheckQuote {
                chains,
                quote,
                enclave,
            } => pccs::status::check_quote_cmd(&ctx, &chains, quote, enclave).await,
        },
        Cmd::Eden { cmd: EdenCmd::Core } => crate::steps::eden::core(&ctx).await,
        Cmd::Isms { cmd } => match cmd {
            IsmsCmd::Evm { chains } => crate::steps::isms::evm(&ctx, &chains).await,
            IsmsCmd::Celestia { chains } => crate::steps::isms::celestia(&ctx, &chains).await,
        },
        Cmd::Warp {
            cmd: WarpCmd::Evm { chains },
        } => crate::steps::warp::evm(&ctx, &chains).await,
        Cmd::Paymaster => crate::steps::celestia::paymaster::run(&ctx).await,
        Cmd::Config => crate::steps::services::write_config(&ctx),
        Cmd::Services { cmd } => match cmd {
            ServicesCmd::Install => crate::steps::services::install(&ctx).await,
            ServicesCmd::Restart => crate::steps::services::restart(&ctx).await,
            ServicesCmd::Stop => crate::steps::services::stop(),
        },
        Cmd::Ui { public_url } => crate::steps::ui::run(&ctx, &public_url),
        Cmd::Smoke {
            chains,
            amount,
            timeout_mins,
        } => crate::steps::smoke::run(&ctx, &chains, amount, timeout_mins).await,
        Cmd::Status => crate::steps::status::run(&ctx).await,
        Cmd::Teardown { yes } => crate::steps::teardown::run(&mut ctx, yes).await,
        Cmd::CheckSecrets { staged, install_hook } => {
            if install_hook {
                crate::steps::check_secrets::install_hook(&ctx)
            } else {
                crate::steps::check_secrets::check(&ctx, staged)
            }
        }
    }
}
