//! `teeism build`: every binary and contract the other steps run, built from this checkout and
//! the celestia-app checkout beside it. Always rebuilt (each tool is incremental), so nothing
//! runs from a stale build.

use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::shared::print;
use crate::shared::settings::Ctx;
use crate::shared::shell::Cmd;

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
        (
            "./x/teeism/cmd/teeism-collateral",
            "fetches Intel collateral for Celestia-destination attestations",
        ),
        (
            "./x/teeism/cmd/teeism-identity",
            "digests an enclave identity the way x/teeism does",
        ),
    ] {
        let name = pkg.rsplit('/').next().unwrap_or(pkg);
        print::step(format!("building {name} ({what})"));
        Cmd::new(format!("building {name} from {}", app.display()), "go")
            .args(["build", "-o"])
            .arg(bin.join(name))
            .arg(pkg)
            .cwd(&app)
            .run()?;
    }

    print::step("building the relayer (tee-hyperlane) and the gas oracle");
    Cmd::new("building tee-coprocessor and gas-oracle", "cargo")
        .args([
            "build",
            "--release",
            "--locked",
            "-p",
            "tee-coprocessor",
            "-p",
            "gas-oracle",
        ])
        .cwd(ctx.repo.join("tee-hyperlane"))
        .run()?;

    print::step("compiling TeeDcapIsm and the Hyperlane contracts it deploys beside");
    let contracts = ctx.contracts_dir();
    if !contracts.join("node_modules/@hyperlane-xyz/core").is_dir() {
        Cmd::new("installing the Hyperlane Solidity packages", "npm")
            .args(["ci", "--silent"])
            .cwd(&contracts)
            .run()?;
    }
    // Our ISM, and the Hyperlane contracts teeism deploys from out/: the warp routers, and
    // Eden's mailbox and merkle tree hook. Nothing in src/ imports those, so they are named
    // here, and `--lib-paths lib` stops forge treating node_modules as a library, which it
    // would otherwise only compile when imported. Tests are skipped: they need forge-std,
    // which nothing fetches.
    Cmd::new("compiling tee-hyperlane/contracts", "forge")
        .args([
            "build",
            "--skip",
            "test",
            "--lib-paths",
            "lib",
            "src",
            "node_modules/@hyperlane-xyz/core/contracts/token/HypERC20.sol",
            "node_modules/@hyperlane-xyz/core/contracts/Mailbox.sol",
            "node_modules/@hyperlane-xyz/core/contracts/hooks/MerkleTreeHook.sol",
        ])
        .cwd(&contracts)
        .run()?;

    print::step("compiling Automata's PCCS and DCAP verifier");
    automata(ctx)?;
    print::ok(format!(
        "built into {} and the projects' out/ directories",
        bin.display()
    ));
    Ok(())
}

/// Fetch Automata's Solidity dependencies at the revisions their foundry.lock pins and
/// compile both projects. The dependency directories are gitignored.
pub fn automata(ctx: &Ctx) -> Result<()> {
    use crate::shared::shell::Cmd;
    let pccs = crate::steps::pccs::pccs_dir(ctx);
    let dcap = crate::steps::pccs::dcap_dir(ctx);
    let deps: [(&Path, &str, &str, &str); 6] = [
        (
            &pccs,
            "lib/forge-std",
            "https://github.com/foundry-rs/forge-std",
            "155d547c449afa8715f538d69454b83944117811",
        ),
        (
            &pccs,
            "lib/openzeppelin-contracts",
            "https://github.com/OpenZeppelin/openzeppelin-contracts",
            "442886ed5ff8a0b9ab477b191f5238541ee6d772",
        ),
        (
            &pccs,
            "lib/solady",
            "https://github.com/Vectorized/solady",
            "772a49d91b004eb55dc9129ae57dd36fbb8417ee",
        ),
        (
            &dcap,
            "lib/forge-std",
            "https://github.com/foundry-rs/forge-std",
            "3b20d60d14b343ee4f908cb8079495c07f5e8981",
        ),
        (
            &dcap,
            "lib/risc0-ethereum",
            "https://github.com/risc0/risc0-ethereum",
            "365e7b2db4f620fa256580c27558d2623362b9ae",
        ),
        (
            &dcap,
            "lib/sp1-contracts",
            "https://github.com/succinctlabs/sp1-contracts",
            "512b5e029abc27f6e46a3c7eba220dac83ecc306",
        ),
    ];
    for (project, dir, url, rev) in deps {
        pin_dependency(&project.join(dir), url, rev)?;
    }
    // risc0-ethereum carries the OpenZeppelin that Automata's remappings name.
    Cmd::new("fetching risc0-ethereum's OpenZeppelin", "git")
        .arg("-C")
        .arg(dcap.join("lib/risc0-ethereum"))
        .args([
            "submodule",
            "update",
            "-q",
            "--init",
            "lib/openzeppelin-contracts",
        ])
        .output()?;
    let link = dcap.join("lib/automata-on-chain-pccs");
    if !link.exists() {
        #[cfg(unix)]
        std::os::unix::fs::symlink("../../../pccs", &link)
            .with_context(|| format!("linking {}", link.display()))?;
    }
    Cmd::new("compiling the Automata PCCS", "forge")
        .args(["build", "--skip", "test", "--skip", "script"])
        .cwd(&pccs)
        .run()?;
    Cmd::new("compiling the Automata DCAP verifier", "forge")
        .args(["build", "--skip", "test", "--skip", "script", "--remappings"])
        .arg("@openzeppelin/contracts/=lib/automata-on-chain-pccs/lib/openzeppelin-contracts/contracts/")
        .cwd(&dcap)
        .run()?;
    Ok(())
}

/// Put a gitignored dependency checkout at exactly `rev`. The commit is fetched by its hash,
/// which works on a shallow or stale clone too. A checkout that still cannot reach it (left by
/// older tooling) is deleted and cloned again: the directory only ever holds a fetched copy.
fn pin_dependency(path: &Path, url: &str, rev: &str) -> Result<()> {
    let git = |what: &str| Cmd::new(what, "git").arg("-C").arg(path);
    if path.join(".git").exists() {
        let head = git("reading a dependency's revision")
            .args(["rev-parse", "HEAD"])
            .output()?;
        if head.trim() == rev {
            return Ok(());
        }
        let fetched = git(&format!("fetching {rev} of {url}"))
            .args(["fetch", "-q", "origin", rev])
            .output();
        let checked = fetched.and_then(|_| {
            git(&format!("checking out {rev} of {url}"))
                .args(["checkout", "-q", rev])
                .output()
        });
        if checked.is_ok() {
            return Ok(());
        }
        print::info(format!("{} cannot reach {rev}; cloning it again", path.display()));
        std::fs::remove_dir_all(path).with_context(|| format!("removing {}", path.display()))?;
    }
    print::info(format!("fetching {url} at {rev}"));
    Cmd::new(format!("cloning {url}"), "git")
        .args(["clone", "-q", url])
        .arg(path)
        .output()?;
    git(&format!("checking out {rev} of {url}"))
        .args(["checkout", "-q", rev])
        .output()?;
    Ok(())
}
