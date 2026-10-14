//! `teeism ui`: build the bridge UI against this deployment and serve it, with the chain's RPC
//! and REST and the relayer's API, through the gateway on port 3000.
//!
//! The deployment's ids are baked into the build, so it is built on the host that serves it.

use anyhow::{bail, Context, Result};
use std::fmt::Write as _;

use crate::chains::{celestia as cel, EVM_CHAINS};
use crate::ctx::Ctx;
use crate::isms;
use crate::out;
use crate::run::Cmd;

pub fn env_local(ctx: &Ctx, host: &str) -> Result<String> {
    let d = ctx.deployment()?;
    let c = &d.celestia;
    let need = |v: &Option<String>, what: &str| v.clone().with_context(|| format!("no {what} recorded; finish the deploy first"));
    let mut s = String::new();
    let w = &mut s;
    writeln!(w, "# Written by `teeism ui` from devnet/.state/deployment.json.")?;
    writeln!(w, "VITE_CELESTIA_NAME=Celestia teeism")?;
    writeln!(w, "VITE_CELESTIA_CHAIN_ID={}", cel::CHAIN_ID)?;
    writeln!(w, "VITE_CELESTIA_DOMAIN={}", cel::DOMAIN)?;
    writeln!(w, "VITE_CELESTIA_RPC={host}/rpc")?;
    writeln!(w, "VITE_CELESTIA_REST={host}/rest")?;
    writeln!(w, "VITE_CELESTIA_EXPLORER={host}")?;
    writeln!(w, "VITE_RELAYER_API={host}/api")?;
    writeln!(w, "VITE_CELESTIA_MAILBOX_ID={}", need(&c.mailbox, "mailbox")?)?;
    // The routing ISM, which fans every origin out to the ISM pinning its enclave.
    writeln!(w, "VITE_CELESTIA_ISM_ID={}", need(&c.routing_ism, "routing ISM")?)?;
    writeln!(w, "VITE_CELESTIA_IGP_ID={}", need(&c.igp, "paymaster")?)?;
    writeln!(w, "VITE_CELESTIA_TIA_ROUTER={}", need(&c.tia_token, "TIA token")?)?;
    writeln!(w, "VITE_CELESTIA_TEEUSD_ROUTER={}", need(&c.teeusd_token, "teeUSD token")?)?;
    for chain in &EVM_CHAINS {
        let e = d.evm(chain.name).with_context(|| format!("nothing deployed on {}", chain.label))?;
        let up = chain.name.to_uppercase();
        let addr = |a: Option<alloy::primitives::Address>, what: &str| {
            a.map(|a| a.to_string()).with_context(|| format!("no {what} on {}", chain.label))
        };
        writeln!(w, "VITE_{up}_RPC={host}/evm/{}/", chain.name)?;
        writeln!(w, "VITE_{up}_ISM={}", addr(e.ism, "ISM")?)?;
        writeln!(w, "VITE_{up}_TIA_ROUTER={}", addr(e.tia_router, "TIA router")?)?;
        writeln!(w, "VITE_{up}_TEEUSD_ROUTER={}", addr(e.teeusd_router, "teeUSD router")?)?;
        if chain.canonical.is_none() {
            writeln!(w, "VITE_{up}_MAILBOX={}", isms::mailbox_of(&d, chain)?)?;
        }
    }
    // No proving: the destination verifies the enclave's quote directly.
    writeln!(w, "VITE_PROVING_SECONDS=30")?;
    Ok(s)
}

pub fn run(ctx: &Ctx, public_url: &str) -> Result<()> {
    let host = public_url.trim_end_matches('/');
    if !host.starts_with("http") {
        bail!("--public-url must be a URL like http://<host>:3000, not `{host}`");
    }
    let app = ctx.repo.join("bridge-app");
    std::fs::write(app.join(".env.local"), env_local(ctx, host)?).context("writing bridge-app/.env.local")?;
    out::info("wrote bridge-app/.env.local");
    out::step("building the UI");
    Cmd::new("installing the UI's packages", "npm").args(["ci", "--silent"]).cwd(&app).run()?;
    Cmd::new("building the UI", "npm").args(["run", "build"]).env("VITE_DEVNET", "1").cwd(&app).run()?;
    // A build that still names the chain at localhost would work on this host and nowhere
    // else, and fail only in a visitor's browser.
    let assets = std::fs::read_dir(app.join("dist/assets")).context("the build produced no dist/assets")?;
    for f in assets.flatten() {
        if f.path().extension().is_some_and(|e| e == "js") && std::fs::read_to_string(f.path())?.contains("localhost:26657") {
            bail!("{} points at localhost:26657; the UI would only work on this machine", f.path().display());
        }
    }
    out::step("starting the gateway on :3000");
    Cmd::new("starting the gateway", "docker")
        .args(["compose", "-f"])
        .arg(ctx.devnet().join("gateway/docker-compose.yml"))
        .args(["up", "-d", "--force-recreate"])
        .env("UI_DIST", app.join("dist").to_string_lossy())
        .run()?;
    out::ok(format!("UI at {host}/"));
    Ok(())
}
