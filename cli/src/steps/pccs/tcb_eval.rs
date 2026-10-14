//! `teeism pccs tcb-eval <n>`: contracts for one more TCB evaluation number on each chain,
//! registered on its router and seeded. Intel's standard number moves twelve months after it
//! is released, and a chain without contracts for the new one stops verifying that day.

use alloy::primitives::Address;
use anyhow::{bail, Result};

use super::deploy::{deploy_eval, Code};
use super::{dcap_dir, pccs_dir, seed};
use crate::shared::chains;
use crate::shared::evm::{self, Chain};
use crate::shared::intel::Intel;
use crate::shared::print;
use crate::shared::record::Pccs;
use crate::shared::settings::Ctx;

/// `teeism pccs tcb-eval <n>`: contracts for one more TCB evaluation number on each chain,
/// registered on its router and seeded.
pub async fn run(ctx: &Ctx, chains: &[String], number: u32, fmspc: &str) -> Result<()> {
    let (published, _) = Intel::new().tdx_evals().await?;
    if !published.contains(&number) {
        bail!("Intel does not publish TDX evaluation {number} (it publishes {published:?})");
    }
    for chain in chains::evm_selection(chains)? {
        let mut d = ctx.deployment()?;
        let mut p = super::pccs_for(&d, chain)?.clone();
        print::step(format!("evaluation {number} on {}", chain.label));
        let c = Chain::connect(ctx, chain).await?;
        add_eval(ctx, &c, &mut p, number, evm::relayer_key_address(ctx)?).await?;
        d.evm_mut(chain.name).pccs = Some(p.clone());
        ctx.save(&d)?;
        seed::publish(&seed::seeder(ctx, chain).await?, &p, fmspc, false).await?;
    }
    Ok(())
}

/// Add contracts for one more evaluation number to an existing deployment.
pub async fn add_eval(ctx: &Ctx, c: &Chain, p: &mut Pccs, n: u32, seeder: Address) -> Result<()> {
    let code = Code {
        pccs: pccs_dir(ctx),
        dcap: dcap_dir(ctx),
    };
    let daos = deploy_eval(&code, c, p, n, seeder).await?;
    p.evals.insert(n, daos);
    Ok(())
}
