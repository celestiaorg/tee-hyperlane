//! `teeism mailbox`: Hyperlane core on every EVM chain without a canonical deployment (today
//! Eden): a Mailbox and a MerkleTreeHook. A chain with Hyperlane's own deployment uses that and
//! needs nothing here. `teeism isms evm` initializes each mailbox with the ISM it deploys there.

use alloy::sol_types::SolValue;
use anyhow::Result;

use crate::shared::chains::{self, EvmChain};
use crate::shared::evm::{Artifact, Chain};
use crate::shared::print;
use crate::shared::settings::Ctx;

/// `teeism mailbox [--chain …]`
pub async fn run(ctx: &Ctx, selection: &[String]) -> Result<()> {
    for chain in chains::evm_selection(selection)? {
        if let Some(c) = chain.canonical {
            print::info(format!(
                "{}: Hyperlane's own mailbox {} and merkle tree hook {}",
                chain.label, c.mailbox, c.merkle_hook
            ));
            continue;
        }
        deploy(ctx, chain).await?;
    }
    Ok(())
}

async fn deploy(ctx: &Ctx, chain: &'static EvmChain) -> Result<()> {
    print::step(format!("Hyperlane core on {}", chain.label));
    let c = Chain::connect(ctx, chain).await?;
    let mut d = ctx.deployment()?;
    let e = d.evm_mut(chain.name);
    let mailbox = match e.mailbox {
        Some(m) if c.has_code(m).await? => m,
        _ => {
            let art = Artifact::load(&ctx.contracts_dir(), "Mailbox", "Mailbox")?;
            let m = c
                .deploy(
                    &format!("deploying the {} mailbox", chain.label),
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
                    &format!("deploying the {} merkle tree hook", chain.label),
                    art.initcode(&mailbox.abi_encode()).into(),
                )
                .await?;
            e.merkle_hook = Some(h);
            h
        }
    };
    print::kv("merkle tree hook", hook);
    ctx.save(&d)?;
    print::ok(format!(
        "{} core deployed; `teeism isms evm` initializes the mailbox with the ISM it deploys there",
        chain.label
    ));
    Ok(())
}
