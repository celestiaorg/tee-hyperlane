//! `teeism preflight`: check every prerequisite before deploying anything, and say what is
//! missing. Read only.

use std::time::Duration;

use anyhow::{bail, Result};

use crate::shared::chains::EVM_CHAINS;
use crate::shared::evm::{self, fmt_eth};
use crate::shared::print;
use crate::shared::settings::Ctx;
use crate::shared::shell::{on_path, Cmd};
use crate::shared::signer::Signer;
use crate::steps::{build, pccs};

pub async fn run(ctx: &Ctx) -> Result<()> {
    let mut problems: Vec<String> = Vec::new();
    let mut check = |ok: bool, what: String, fix: &str| {
        if ok {
            print::ok(&what);
        } else {
            print::bad(format!("{what}: {fix}"));
            problems.push(what);
        }
    };

    print::step("tools");
    for (tool, why) in [
        ("docker", "runs the chain, the light node and the gateway"),
        ("go", "builds celestia-appd"),
        ("cargo", "builds the relayer"),
        ("forge", "compiles the contracts"),
        ("npm", "builds the UI"),
        ("git", "fetches dependencies, checks the pins are pushed"),
        ("phala", "deploys the enclaves (npm i -g phala)"),
        (
            "nix",
            "builds the enclave images (`teeism images`, `enclaves up` pin checks)",
        ),
        ("sudo", "installs the systemd units"),
    ] {
        check(on_path(tool), format!("{tool} on PATH"), why);
    }
    let docker = Cmd::new("asking docker for its version", "docker")
        .args(["info", "--format", "{{.ServerVersion}}"])
        .capture();
    check(
        matches!(docker, Ok((s, _, _)) if s.success()),
        "docker daemon answering".into(),
        "start docker",
    );

    print::step("code");
    match build::check_celestia_app(ctx) {
        Ok(dir) => check(
            true,
            format!(
                "celestia-app at {} on {}",
                dir.display(),
                build::CELESTIA_APP_BRANCH
            ),
            "",
        ),
        Err(e) => check(false, "celestia-app checkout".into(), &format!("{e:#}")),
    }

    print::step("settings");
    let owner = match Signer::owner(ctx).await {
        Ok(s) => {
            check(true, format!("EVM owner {}", s.address()), "");
            Some(s.address())
        }
        Err(e) => {
            check(false, "EVM owner".into(), &format!("{e:#}"));
            None
        }
    };
    match Signer::hot_key(ctx) {
        Ok(s) => check(
            true,
            format!("EVM hot key {} (relayer, collateral seeding)", s.address()),
            "",
        ),
        Err(e) => check(false, "EVM hot key".into(), &format!("{e:#}")),
    }
    check(
        ctx.var("ALCHEMY_KEY").is_some()
            || ctx.var("ALCHEMY_BASE_KEY").is_some()
            || (ctx.var("BASE_ARCHIVE").is_some() && ctx.var("ARBITRUM_ARCHIVE").is_some()),
        "Base and Arbitrum archive access".into(),
        "set ALCHEMY_KEY (free tier) in devnet/.env",
    );
    let phala = Cmd::new("checking the Phala login", "phala")
        .arg("status")
        .capture();
    check(
        matches!(phala, Ok((s, _, _)) if s.success()),
        "logged in to Phala Cloud".into(),
        "phala auth login <api-key>",
    );

    print::step("chains");
    if let Some(from) = &owner {
        for chain in &EVM_CHAINS {
            let rpc = chain.rpc(ctx);
            let res = async {
                let r = evm::reader(&rpc)?;
                use alloy::providers::Provider;
                let id = r.get_chain_id().await?;
                anyhow::ensure!(id == chain.chain_id, "{rpc} serves chain {id}");
                Ok::<_, anyhow::Error>(r.get_balance(*from).await?)
            }
            .await;
            match res {
                Ok(b) => check(
                    !b.is_zero(),
                    format!("{}: {from} holds {} (gas token)", chain.label, fmt_eth(b)),
                    "fund it",
                ),
                Err(e) => check(false, format!("{} at {rpc}", chain.label), &format!("{e:#}")),
            }
        }
    }
    let http = reqwest::Client::new();
    let beacon = ctx.var_or("SEPOLIA_BEACON", "http://127.0.0.1:9596");
    let gloas = async {
        let v: serde_json::Value = http
            .get(format!("{beacon}/eth/v1/beacon/light_client/finality_update"))
            .timeout(Duration::from_secs(10))
            .send()
            .await?
            .json()
            .await?;
        Ok::<_, anyhow::Error>(v["version"].as_str().unwrap_or("").to_string())
    }
    .await;
    match gloas {
        Ok(v) => check(
            v == "gloas",
            format!("Sepolia beacon {beacon} serves {v} light-client updates"),
            "needs Lodestar serving gloas",
        ),
        Err(e) => check(false, format!("Sepolia beacon at {beacon}"), &format!("{e:#}")),
    }
    let reth = ctx.var_or("SEPOLIA_RPC", "http://127.0.0.1:8545");
    let ok = evm::reader(&reth).is_ok() && {
        use alloy::providers::Provider;
        match evm::reader(&reth) {
            Ok(r) => r.get_block_number().await.is_ok(),
            Err(_) => false,
        }
    };
    check(
        ok,
        format!("Sepolia execution node at {reth}"),
        "the relayer reads proofs from it",
    );

    print::step("Intel");
    let intel = crate::steps::pccs::seed::Intel::new();
    match intel.standard(pccs::DEFAULT_FMSPC).await {
        Ok(n) => check(
            true,
            format!("Intel PCS answering; standard TCB evaluation {n}"),
            "",
        ),
        Err(e) => check(false, "Intel PCS".into(), &format!("{e:#}")),
    }

    if !problems.is_empty() {
        bail!(
            "{} prerequisite(s) missing:\n  - {}",
            problems.len(),
            problems.join("\n  - ")
        );
    }
    print::ok("ready");
    Ok(())
}
