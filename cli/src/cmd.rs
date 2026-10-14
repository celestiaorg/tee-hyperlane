//! The command tree, and what each command runs.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use std::collections::BTreeSet;

use crate::chains::{self, EvmChain};
use crate::ctx::Ctx;
use crate::evm::{self, Chain};
use crate::{errors, out, pccs};

/// Work a command had to leave undone, each item said as it happened. Exits 3 rather than 1,
/// so a partial run is never mistaken for a finished one or for a hard failure.
#[derive(Debug)]
pub struct Incomplete(pub Vec<String>);

impl std::fmt::Display for Incomplete {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} item(s) left undone; fix the cause and run it again (finished steps are skipped):", self.0.len())?;
        for item in &self.0 {
            write!(f, "\n  - {item}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Incomplete {}

#[derive(Parser)]
#[command(name = "teeism", about = "Deploy, rotate, inspect and tear down the TEE ISM bridge", version)]
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
        #[arg(long, default_value = crate::enclaves::DEFAULT_OS_IMAGE)]
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
    errors::load(&[
        &ctx.contracts_dir(),
        &pccs::pccs_dir(&ctx),
        &pccs::dcap_dir(&ctx),
    ]);
    let mut ctx = ctx;
    match cli.cmd {
        Cmd::Preflight => crate::preflight::run(&ctx).await,
        Cmd::Build => crate::build::build(&ctx),
        Cmd::Celestia { cmd } => match cmd {
            CelestiaCmd::Up { new_image } => crate::chain::up(&mut ctx, new_image).await,
            CelestiaCmd::Core => crate::chain::core(&ctx).await,
            CelestiaCmd::Warp => crate::chain::warp(&ctx).await,
        },
        Cmd::Mocha { cmd: MochaCmd::Up } => crate::mocha::up(&ctx).await,
        Cmd::Images { chains } => crate::images::run(&ctx, &chains),
        Cmd::Enclaves { cmd } => match cmd {
            EnclavesCmd::Up { chains, os_image, unpinned } => {
                crate::enclaves::up(&ctx, crate::enclaves::UpArgs { chains, unpinned, os_image }).await
            }
            EnclavesCmd::Verify { target, app_id, ism_chain, rebuild } => {
                crate::enclaves::verify(&ctx, crate::enclaves::VerifyArgs { target, app_id, ism_chain, rebuild }).await
            }
            EnclavesCmd::Identity { chains } => {
                let chains = if chains.is_empty() { ctx.deployment()?.enclaves.chains.keys().cloned().collect() } else { chains };
                for c in chains {
                    crate::enclaves::record_identity(&ctx, &c).await?;
                }
                Ok(())
            }
            EnclavesCmd::Delete { app_id, retired } => match (app_id, retired) {
                (_, true) => crate::enclaves::delete_retired(&ctx),
                (Some(id), false) => crate::enclaves::delete(&id),
                (None, false) => bail!("give --app-id <id> or --retired"),
            },
        },
        Cmd::Pccs { cmd } => pccs_cmd(&ctx, cmd).await,
        Cmd::Eden { cmd: EdenCmd::Core } => crate::warp::eden_core(&ctx).await,
        Cmd::Isms { cmd } => match cmd {
            IsmsCmd::Evm { chains } => crate::isms::evm(&ctx, &chains).await,
            IsmsCmd::Celestia { chains } => crate::isms::celestia(&ctx, &chains).await,
        },
        Cmd::Warp { cmd: WarpCmd::Evm { chains } } => crate::warp::evm(&ctx, &chains).await,
        Cmd::Paymaster => crate::paymaster::run(&ctx).await,
        Cmd::Config => crate::config::write(&ctx),
        Cmd::Services { cmd } => match cmd {
            ServicesCmd::Install => crate::services::install(&ctx).await,
            ServicesCmd::Restart => crate::services::restart(&ctx).await,
            ServicesCmd::Stop => crate::services::stop(),
        },
        Cmd::Ui { public_url } => crate::ui::run(&ctx, &public_url),
        Cmd::Smoke { chains, amount, timeout_mins } => crate::smoke::run(&ctx, &chains, amount, timeout_mins).await,
        Cmd::Status => crate::status::run(&ctx).await,
        Cmd::Teardown { yes } => crate::teardown::run(&mut ctx, yes).await,
        Cmd::CheckSecrets { staged, install_hook } => {
            if install_hook {
                crate::secrets::install_hook(&ctx)
            } else {
                crate::secrets::check(&ctx, staged)
            }
        }
    }
}

/// The relayer's key signs collateral uploads: it needs the uploader role and nothing else,
/// so a Ledger-owned deployment is still seeded unattended.
async fn seeder(ctx: &Ctx, chain: &'static EvmChain) -> Result<Chain> {
    let key = ctx.require("EVM_PRIVATE_KEY", "it uploads Intel's collateral")?;
    let s: alloy::signers::local::PrivateKeySigner =
        key.trim().trim_start_matches("0x").parse().context("EVM_PRIVATE_KEY is not a valid private key")?;
    let from = s.address();
    Chain::with_wallet(chain, chain.rpc(ctx), alloy::network::EthereumWallet::from(s), from).await
}

async fn pccs_cmd(ctx: &Ctx, cmd: PccsCmd) -> Result<()> {
    match cmd {
        PccsCmd::Build => {
            out::step("compiling Automata's PCCS and DCAP verifier");
            pccs::build(ctx)
        }
        PccsCmd::Deploy { chains, namespace, fmspc } => {
            let intel = pccs::Intel::new();
            let wanted = pccs::evals_to_deploy(&intel, &fmspc).await?;
            for chain in chains::evm_selection(&chains)? {
                let mut d = ctx.deployment()?;
                let existing = d.evm(chain.name).and_then(|e| e.pccs.clone());
                let ns = namespace
                    .clone()
                    .or_else(|| existing.as_ref().map(|p| p.namespace.clone()))
                    .unwrap_or_else(|| pccs::DEFAULT_NAMESPACE.to_string());
                let mut evals: BTreeSet<u32> = wanted.clone();
                if let Some(p) = &existing {
                    if p.namespace == ns {
                        evals.extend(p.evals.keys());
                    }
                }
                out::step(format!("PCCS on {} (namespace {ns}, evaluations {evals:?})", chain.label));
                let c = Chain::connect(ctx, chain).await?;
                let seeder = evm::relayer_key_address(ctx)?;
                let p = pccs::deploy(ctx, &c, &ns, &evals, seeder).await?;
                d.evm_mut(chain.name).pccs = Some(p.clone());
                ctx.save(&d)?;
                out::step(format!("seeding {}", chain.label));
                pccs::seed(&seeder_chain(ctx, chain).await?, &p, &fmspc, false).await?;
                out::ok(format!("{}: entrypoint {}", chain.label, p.entrypoint));
            }
            Ok(())
        }
        PccsCmd::Seed { chains, fmspc, dry_run } => {
            let d = ctx.deployment()?;
            for chain in chains::evm_selection(&chains)? {
                let p = pccs::pccs_for(&d, chain)?;
                out::step(format!("seeding {}", chain.label));
                pccs::seed(&seeder_chain(ctx, chain).await?, p, &fmspc, dry_run).await?;
            }
            Ok(())
        }
        PccsCmd::Status { chains, fmspc, warn_days } => pccs_status(ctx, &chains, &fmspc, warn_days).await,
        PccsCmd::TcbEval { chains, number, fmspc } => {
            let intel = pccs::Intel::new();
            let (published, _) = intel.tdx_evals().await?;
            if !published.contains(&number) {
                bail!("Intel does not publish TDX evaluation {number} (it publishes {published:?})");
            }
            for chain in chains::evm_selection(&chains)? {
                let mut d = ctx.deployment()?;
                let mut p = pccs::pccs_for(&d, chain)?.clone();
                out::step(format!("evaluation {number} on {}", chain.label));
                let c = Chain::connect(ctx, chain).await?;
                pccs::add_eval(ctx, &c, &mut p, number, evm::relayer_key_address(ctx)?).await?;
                d.evm_mut(chain.name).pccs = Some(p.clone());
                ctx.save(&d)?;
                pccs::seed(&seeder_chain(ctx, chain).await?, &p, &fmspc, false).await?;
            }
            Ok(())
        }
        PccsCmd::CheckQuote { chains, quote, enclave } => {
            let d = ctx.deployment()?;
            let raw = match (quote, enclave) {
                (Some(path), _) => {
                    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
                    hex::decode(text.trim().trim_start_matches("0x")).with_context(|| format!("{} is not hex", path.display()))?
                }
                (None, Some(e)) => {
                    let url = d.enclaves.chains.get(&e).map(|x| x.url.clone()).with_context(|| format!("no {e} enclave recorded"))?;
                    crate::enclaves::fetch_identity(&url).await?.quote
                }
                (None, None) => bail!("give --quote <file> or --enclave <chain>"),
            };
            for chain in chains::evm_selection(&chains)? {
                let p = pccs::pccs_for(&d, chain)?;
                pccs::check_quote(&chain.rpc(ctx), p, &raw).await.with_context(|| format!("on {}", chain.label))?;
                out::ok(format!("{} accepts the quote", chain.label));
            }
            Ok(())
        }
    }
}

async fn seeder_chain(ctx: &Ctx, chain: &'static EvmChain) -> Result<Chain> {
    seeder(ctx, chain).await
}

async fn pccs_status(ctx: &Ctx, chains: &[String], fmspc: &str, warn_days: u64) -> Result<()> {
    let d = ctx.deployment()?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
    let mut problems = Vec::new();
    for chain in chains::evm_selection(chains)? {
        let Ok(p) = pccs::pccs_for(&d, chain) else {
            problems.push(format!("{}: no PCCS deployed", chain.label));
            continue;
        };
        out::step(chain.label);
        for row in pccs::validity(&chain.rpc(ctx), p, fmspc).await? {
            match row.window {
                Err(e) => {
                    out::bad(format!("{:<28} ERROR    {e}", row.label));
                    problems.push(format!("{}: {} unreadable", chain.label, row.label));
                }
                Ok((_, 0)) => {
                    out::bad(format!("{:<28} MISSING  nothing published", row.label));
                    problems.push(format!("{}: {} missing", chain.label, row.label));
                }
                Ok((_, until)) => {
                    let left = (until as i64 - now as i64) / 86400;
                    if until < now {
                        out::bad(format!("{:<28} EXPIRED  {}", row.label, date(until)));
                        problems.push(format!("{}: {} expired", chain.label, row.label));
                    } else if (left as u64) < warn_days {
                        out::bad(format!("{:<28} EXPIRING {} ({left}d)", row.label, date(until)));
                        problems.push(format!("{}: {} expires in {left}d", chain.label, row.label));
                    } else {
                        out::ok(format!("{:<28} {} ({left}d)", row.label, date(until)));
                    }
                }
            }
        }
    }
    if !problems.is_empty() {
        bail!(
            "collateral needs attention:\n  - {}\nEXPIRING, EXPIRED and MISSING are fixed by `teeism pccs seed`; \
             ERROR means the record or the endpoint is wrong, not the collateral",
            problems.join("\n  - ")
        );
    }
    out::ok(format!("all collateral valid for more than {warn_days} days"));
    Ok(())
}

/// A unix time as `YYYY-MM-DD`.
pub fn date(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    // Civil-from-days (Howard Hinnant), to avoid a date dependency for one format.
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}
