//! `teeism paymaster`: the interchain gas paymaster on Celestia, which a Celestia-origin
//! transfer pays for its delivery on the far side.
//!
//! The paymaster is owned by the `bridge` key, which the gas oracle signs as when it posts
//! each destination's gas price. The mailbox's default hook becomes the paymaster; its
//! required hook becomes the merkle tree hook, whose tree the enclave proves.

use anyhow::{Context, Result};

use crate::shared::print;
use crate::shared::settings::Ctx;
use crate::shared::shell::Cmd;
use crate::steps::celestia::appd::{self, Appd};
use crate::steps::celestia::up::BRIDGE_KEY;
use crate::steps::services::write_config;

/// What the bridge key is kept topped up to, for the gas oracle's transactions.
const BRIDGE_FLOOR: u128 = 10_000_000; // 10 TIA
const BRIDGE_TOP_UP: &str = "100000000utia"; // 100 TIA

pub async fn run(ctx: &Ctx) -> Result<()> {
    appd::wait_for_chain(ctx).await?;
    let appd = Appd::new(ctx)?;
    let mut d = ctx.deployment()?;
    let (key, account) = BRIDGE_KEY;
    if !appd.has_key(key)? {
        let m = ctx.require("CELESTIA_MNEMONIC", "the bridge key derives from it")?;
        appd.recover_key(key, account, &m)?;
    }
    let bridge = appd.address(key)?;
    d.celestia.bridge = Some(bridge.clone());
    print::kv("bridge key", &bridge);

    if d.celestia.igp.is_none() {
        let tx = appd.tx(
            "creating the paymaster",
            "relayer",
            &["hyperlane", "hooks", "igp", "create", "utia"],
        )?;
        d.celestia.igp = Some(tx.attr("hyperlane.core.post_dispatch.v1.EventCreateIgp", "igp_id")?);
        ctx.save(&d)?;
    }
    let igp = d.celestia.igp.clone().unwrap_or_default();
    print::kv("paymaster", &igp);

    if appd.utia_balance(&bridge)? < BRIDGE_FLOOR {
        appd.tx(
            "funding the bridge key",
            "relayer",
            &["bank", "send", "relayer", &bridge, BRIDGE_TOP_UP],
        )?;
    }
    let v = appd.query("reading the paymaster", &["hyperlane", "hooks", "igp", &igp])?;
    let owner = v["igp"]["owner"].as_str().unwrap_or("");
    if owner != bridge {
        appd.tx(
            "handing the paymaster to the bridge key",
            "relayer",
            &[
                "hyperlane",
                "hooks",
                "igp",
                "set-owner",
                &igp,
                "--new-owner",
                &bridge,
            ],
        )?;
    }

    let mailbox = d
        .celestia
        .mailbox
        .clone()
        .context("no mailbox; run `teeism celestia core`")?;
    let merkle = d
        .celestia
        .merkle_hook
        .clone()
        .context("no merkle tree hook; run `teeism celestia core`")?;
    let m = appd.query("reading the mailbox", &["hyperlane", "mailbox", &mailbox])?;
    if m["mailbox"]["default_hook"].as_str() != Some(igp.as_str())
        || m["mailbox"]["required_hook"].as_str() != Some(merkle.as_str())
    {
        appd.tx(
            "making the paymaster the mailbox's default hook",
            "relayer",
            &[
                "hyperlane",
                "mailbox",
                "set",
                &mailbox,
                "--required-hook",
                &merkle,
                "--default-hook",
                &igp,
            ],
        )?;
    }

    // Post every destination's gas price once now: a paymaster without a price for a domain
    // refuses every dispatch to it.
    write_config(ctx)?;
    print::step("posting gas prices once (the gas-oracle service keeps them current)");
    let path = format!(
        "{}:{}",
        ctx.bin_dir().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    Cmd::new(
        "running the gas oracle once",
        ctx.repo.join("tee-hyperlane/target/release/gas-oracle"),
    )
    .arg("--config")
    .arg(ctx.state_dir.join("gas-oracle.toml"))
    .arg("--once")
    .env("CELHOME", ctx.celestia_home().to_string_lossy())
    .env("PATH", path)
    .run()?;
    let q = appd.query(
        "reading the paymaster's gas configs",
        &["hyperlane", "hooks", "destination-gas-configs", &igp],
    )?;
    print::info(format!("gas configs: {q}"));
    print::ok("paymaster ready");
    Ok(())
}
