//! `teeism build`: every binary and contract the other steps run, built from this checkout and
//! the celestia-app checkout beside it. Always rebuilt (each tool is incremental), so nothing
//! runs from a stale build.

use anyhow::{bail, Result};

use crate::ctx::Ctx;
use crate::out;
use crate::pccs;
use crate::run::Cmd;

/// celestia-app is only ever built from this branch. `main` there is someone else's
/// production code and has none of x/teeism.
pub const CELESTIA_APP_BRANCH: &str = "jonas/tee-ism";

/// Fail unless the celestia-app checkout is on the one branch this bridge runs.
pub fn check_celestia_app(ctx: &Ctx) -> Result<std::path::PathBuf> {
    let dir = ctx.celestia_app_dir();
    if !dir.join("go.mod").is_file() {
        bail!(
            "no celestia-app checkout at {}. Clone it with \
             `git clone -b {CELESTIA_APP_BRANCH} https://github.com/celestiaorg/celestia-app.git {}`, \
             or set CELESTIA_APP_DIR",
            dir.display(),
            dir.display()
        );
    }
    let branch = Cmd::new("reading celestia-app's branch", "git")
        .arg("-C")
        .arg(&dir)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()?;
    if branch.trim() != CELESTIA_APP_BRANCH {
        bail!(
            "{} is on branch `{}`; this bridge runs only `{CELESTIA_APP_BRANCH}` (git -C {} checkout {CELESTIA_APP_BRANCH})",
            dir.display(),
            branch.trim(),
            dir.display()
        );
    }
    Ok(dir)
}

pub fn build(ctx: &Ctx) -> Result<()> {
    let app = check_celestia_app(ctx)?;
    let bin = ctx.bin_dir();
    std::fs::create_dir_all(&bin)?;
    for (pkg, what) in [
        ("./cmd/celestia-appd", "the chain's node and CLI"),
        ("./x/teeism/cmd/teeism-collateral", "fetches Intel collateral for Celestia-destination attestations"),
        ("./x/teeism/cmd/teeism-identity", "digests an enclave identity the way x/teeism does"),
    ] {
        let name = pkg.rsplit('/').next().unwrap_or(pkg);
        out::step(format!("building {name} ({what})"));
        Cmd::new(format!("building {name} from {}", app.display()), "go")
            .args(["build", "-o"])
            .arg(bin.join(name))
            .arg(pkg)
            .cwd(&app)
            .run()?;
    }

    out::step("building the relayer (tee-hyperlane) and the gas oracle");
    Cmd::new("building tee-coprocessor and gas-oracle", "cargo")
        .args(["build", "--release", "--locked", "-p", "tee-coprocessor", "-p", "gas-oracle"])
        .cwd(ctx.repo.join("tee-hyperlane"))
        .run()?;

    out::step("compiling TeeDcapIsm and the Hyperlane contracts it deploys beside");
    let contracts = ctx.contracts_dir();
    if !contracts.join("node_modules/@hyperlane-xyz/core").is_dir() {
        Cmd::new("installing the Hyperlane Solidity packages", "npm").args(["ci", "--silent"]).cwd(&contracts).run()?;
    }
    Cmd::new("compiling tee-hyperlane/contracts", "forge").arg("build").cwd(&contracts).run()?;

    out::step("compiling Automata's PCCS and DCAP verifier");
    pccs::build(ctx)?;
    out::ok(format!("built into {} and the projects' out/ directories", bin.display()));
    Ok(())
}
