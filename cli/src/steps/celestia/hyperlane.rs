//! Hyperlane on the bridge's Celestia chain: `teeism celestia core` (mailbox, merkle tree hook,
//! noop ISM and hook) and `teeism celestia warp` (the TIA and teeUSD tokens).

use anyhow::{bail, Context, Result};

use crate::shared::chains::celestia as cel;
use crate::shared::print;
use crate::shared::settings::Ctx;
use crate::steps::celestia::appd::{self, Appd};

/// `teeism celestia core`: mailbox, merkle tree hook, and the noop ISM and hook it starts
/// with. All permissionless: the same sequence anyone would run against a real chain.
pub async fn core(ctx: &Ctx) -> Result<()> {
    appd::wait_for_chain(ctx).await?;
    let appd = Appd::new(ctx)?;
    let mut d = ctx.deployment()?;
    let c = &mut d.celestia;

    // A noop ISM gives the mailbox a default before the TEE ISMs exist; those pin a light
    // client checkpoint, which needs the chain running.
    if c.noop_ism.is_none() {
        let tx = appd.tx(
            "creating a noop ISM",
            "relayer",
            &["hyperlane", "ism", "create-noop"],
        )?;
        c.noop_ism = Some(tx.attr(
            "hyperlane.core.interchain_security.v1.EventCreateNoopIsm",
            "ism_id",
        )?);
    }
    print::kv("noop ism", c.noop_ism.as_deref().unwrap_or(""));
    if c.mailbox.is_none() {
        let tx = appd.tx(
            "creating the mailbox",
            "relayer",
            &[
                "hyperlane",
                "mailbox",
                "create",
                c.noop_ism.as_deref().unwrap_or(""),
                &cel::DOMAIN.to_string(),
            ],
        )?;
        c.mailbox = Some(tx.attr("hyperlane.core.v1.EventCreateMailbox", "mailbox_id")?);
    }
    let mailbox = c.mailbox.clone().unwrap_or_default();
    print::kv("mailbox", &mailbox);
    if c.merkle_hook.is_none() {
        let tx = appd.tx(
            "creating the merkle tree hook",
            "relayer",
            &["hyperlane", "hooks", "merkle", "create", &mailbox],
        )?;
        c.merkle_hook = Some(tx.attr(
            "hyperlane.core.post_dispatch.v1.EventCreateMerkleTreeHook",
            "merkle_tree_hook_id",
        )?);
    }
    let merkle = c.merkle_hook.clone().unwrap_or_default();
    print::kv("merkle tree hook", &merkle);
    if c.noop_hook.is_none() {
        let tx = appd.tx(
            "creating a noop hook",
            "relayer",
            &["hyperlane", "hooks", "noop", "create"],
        )?;
        c.noop_hook = Some(tx.attr(
            "hyperlane.core.post_dispatch.v1.EventCreateNoopHook",
            "noop_hook_id",
        )?);
    }
    let noop_hook = c.noop_hook.clone().unwrap_or_default();
    print::kv("noop hook", &noop_hook);
    ctx.save(&d)?;

    // The merkle tree hook is the default because its tree is what the enclave proves. Warp
    // refuses to dispatch without a required hook; noop keeps dispatch free until
    // `teeism paymaster` puts the gas paymaster there.
    let m = appd.query("reading the mailbox", &["hyperlane", "mailbox", &mailbox])?;
    let default_hook = m["mailbox"]["default_hook"].as_str().unwrap_or("");
    let required_hook = m["mailbox"]["required_hook"].as_str().unwrap_or("");
    if default_hook.is_empty() || required_hook.is_empty() {
        appd.tx(
            "pointing the mailbox at the merkle tree hook",
            "relayer",
            &[
                "hyperlane",
                "mailbox",
                "set",
                &mailbox,
                "--default-hook",
                &merkle,
                "--required-hook",
                &noop_hook,
            ],
        )?;
    }
    print::ok("hyperlane core is up");
    Ok(())
}

/// teeUSD: a dollar token we issue, with a fixed supply minted once.
const TEEUSD_SUPPLY: u128 = 1_000_000_000_000_000; // 1B at 6 decimals
/// A made-up origin that mints the supply in one message and is then removed. "teeU".
const MINT_DOMAIN: u32 = 1952802133;
const MINT_ROUTER: &str = "0x0000000000000000000000000000000000000000000000000000000074656555";

// ---------------------------------------------------------------- teeism celestia warp

/// `teeism celestia warp`: the Celestia side of both assets.
///
///   TIA     native: collateral here, synthetic on every EVM chain.
///   teeUSD  ours: synthetic here too, with a fixed supply minted once below.
///
/// Remote routers are enrolled by `teeism warp evm`, which owns both directions of a route.
pub async fn warp(ctx: &Ctx) -> Result<()> {
    appd::wait_for_chain(ctx).await?;
    let appd = Appd::new(ctx)?;
    let mut d = ctx.deployment()?;
    let mailbox = d
        .celestia
        .mailbox
        .clone()
        .context("no mailbox yet; run `teeism celestia core`")?;

    if d.celestia.tia_token.is_none() {
        let tx = appd.tx(
            "creating the TIA collateral token",
            "relayer",
            &["warp", "create-collateral-token", &mailbox, cel::DENOM],
        )?;
        d.celestia.tia_token = Some(tx.attr("hyperlane.warp.v1.EventCreateCollateralToken", "token_id")?);
        ctx.save(&d)?;
    }
    print::kv("TIA token", d.celestia.tia_token.as_deref().unwrap_or(""));

    if d.celestia.teeusd_token.is_none() {
        let tx = appd.tx(
            "creating the teeUSD synthetic token",
            "relayer",
            &["warp", "create-synthetic-token", &mailbox],
        )?;
        d.celestia.teeusd_token = Some(tx.attr("hyperlane.warp.v1.EventCreateSyntheticToken", "token_id")?);
        ctx.save(&d)?;
    }
    let token = d.celestia.teeusd_token.clone().unwrap_or_default();
    let denom = d.teeusd_denom().unwrap_or_default();
    print::kv("teeUSD token", &token);

    // There is no mint module, so the supply arrives the way any synthetic's does: as a
    // Hyperlane message. The token briefly trusts the noop ISM and a made-up origin, takes one
    // message minting the supply to `user`, then drops that origin. With it gone nothing can
    // mint again. Each step checks the chain first, so an interrupted run is finished by
    // running it again.
    let mint_enrolled = || -> Result<bool> {
        Ok(appd::remote_routers(&appd, &token)?
            .iter()
            .any(|(dom, _)| *dom == MINT_DOMAIN))
    };
    let supply = appd.total_supply(&denom)?;
    if supply == 0 {
        let holder = d
            .celestia
            .user
            .clone()
            .context("no user key recorded; run `teeism celestia up`")?;
        print::step(format!("minting {TEEUSD_SUPPLY} {denom} to {holder}"));
        let noop = d
            .celestia
            .noop_ism
            .clone()
            .context("no noop ISM; run `teeism celestia core`")?;
        appd.tx(
            "letting teeUSD trust the noop ISM for the mint",
            "relayer",
            &["warp", "set-token", &token, "--ism-id", &noop],
        )?;
        if !mint_enrolled()? {
            appd.tx(
                "enrolling the mint origin",
                "relayer",
                &[
                    "warp",
                    "enroll-remote-router",
                    &token,
                    &MINT_DOMAIN.to_string(),
                    MINT_ROUTER,
                    "0",
                ],
            )?;
        }
        let msg = mint_message(&token, &appd::bech32_to_hex32(&holder)?, TEEUSD_SUPPLY);
        appd.tx(
            "delivering the mint message",
            "relayer",
            &["hyperlane", "mailbox", "process", &mailbox, "0x", &msg],
        )?;
        let after = appd.total_supply(&denom)?;
        if after != TEEUSD_SUPPLY {
            bail!("teeUSD supply is {after} after the mint, expected {TEEUSD_SUPPLY}");
        }
    }
    if mint_enrolled()? {
        appd.tx(
            "dropping the mint origin, fixing the supply",
            "relayer",
            &["warp", "unroll-remote-router", &token, &MINT_DOMAIN.to_string()],
        )?;
    }
    print::kv("teeUSD supply", appd.total_supply(&denom)?);
    print::ok(
        "celestia warp side ready; the tokens are pointed at the routing ISM by `teeism isms celestia`",
    );
    Ok(())
}

/// A Hyperlane v3 message from the mint origin to the teeUSD token, whose body is a warp
/// transfer of `amount` to `recipient`.
fn mint_message(token: &str, recipient32: &str, amount: u128) -> String {
    let strip = |s: &str| s.trim_start_matches("0x").to_lowercase();
    format!(
        "0x03{:08x}{:08x}{}{:08x}{}{}{:064x}",
        0u32,
        MINT_DOMAIN,
        strip(MINT_ROUTER),
        cel::DOMAIN,
        strip(token),
        strip(recipient32),
        amount
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn mint_message_layout() {
        let m = super::mint_message(
            "0x726f757465725f61707000000000000000000000000000020000000000000002",
            "0x000000000000000000000000318d22faa1e0f29eac7ef644a8fac676f6688d1e",
            1,
        );
        // version(1) nonce(4) origin(4) sender(32) destination(4) recipient(32) body(64)
        assert_eq!(m.len(), 2 + 2 * (1 + 4 + 4 + 32 + 4 + 32 + 64));
        assert!(m.starts_with("0x0300000000746565550000"));
    }
}
