//! `teeism celestia core`: Hyperlane core on the bridge's chain.

use anyhow::Result;

use crate::shared::celestia_cli::{self, Appd};
use crate::shared::chains::celestia as cel;
use crate::shared::print;
use crate::shared::settings::Ctx;

/// `teeism celestia core`: mailbox, merkle tree hook, and the noop ISM and hook it starts
/// with. All permissionless: the same sequence anyone would run against a real chain.
pub async fn run(ctx: &Ctx) -> Result<()> {
    celestia_cli::wait_for_chain(ctx).await?;
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
