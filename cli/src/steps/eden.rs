//! `teeism eden core`: Hyperlane core on Eden, which has no canonical deployment: a Mailbox and
//! a MerkleTreeHook. `teeism isms evm` initializes the mailbox with the ISM it deploys there.

use alloy::sol_types::SolValue;
use anyhow::Result;

use crate::shared::chains;
use crate::shared::evm::{Artifact, Chain};
use crate::shared::print;
use crate::shared::settings::Ctx;

/// `teeism eden core`
pub async fn core(ctx: &Ctx) -> Result<()> {
    let chain = chains::evm("eden")?;
    let c = Chain::connect(ctx, chain).await?;
    let mut d = ctx.deployment()?;
    let e = d.evm_mut("eden");
    let mailbox = match e.mailbox {
        Some(m) if c.has_code(m).await? => m,
        _ => {
            let art = Artifact::load(&ctx.contracts_dir(), "Mailbox", "Mailbox")?;
            let m = c
                .deploy(
                    "deploying the Eden mailbox",
                    art.initcode(&chain.domain.abi_encode()).into(),
                )
                .await?;
            e.mailbox = Some(m);
            m
        }
    };
    print::kv("mailbox", mailbox);
    let hook = match e.merkle_hook {
        Some(h) if c.has_code(h).await? => h,
        _ => {
            let art = Artifact::load(&ctx.contracts_dir(), "MerkleTreeHook", "MerkleTreeHook")?;
            let h = c
                .deploy(
                    "deploying the Eden merkle tree hook",
                    art.initcode(&mailbox.abi_encode()).into(),
                )
                .await?;
            e.merkle_hook = Some(h);
            h
        }
    };
    print::kv("merkle tree hook", hook);
    ctx.save(&d)?;
    print::ok("Eden core deployed; `teeism isms evm` initializes the mailbox with the ISM it deploys there");
    Ok(())
}
