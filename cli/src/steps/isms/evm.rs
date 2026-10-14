//! `teeism isms evm`: a TeeDcapIsm per EVM chain, pinning the Celestia enclave. Re-pins in
//! place on an identity change; replaces from the last state only when it must.

use alloy::primitives::{Address, Bytes, U256};
use alloy::sol_types::{SolCall, SolValue};
use anyhow::{bail, Context, Result};

use super::*;
use crate::shared::chains;
use crate::shared::evm::{Artifact, Chain};
use crate::shared::print;
use crate::shared::settings::Ctx;
use crate::steps::config;

fn celestia_enclave(ctx: &Ctx) -> Result<(String, String)> {
    let d = ctx.deployment()?;
    let e = d
        .enclaves
        .chains
        .get("celestia")
        .context("no celestia enclave; run `teeism enclaves up`")?;
    let m = e
        .measurements
        .clone()
        .context("the celestia enclave's measurements are not recorded; run `teeism enclaves up`")?;
    let i = e
        .identity_digest
        .clone()
        .context("the celestia enclave's identity is not recorded; run `teeism enclaves up`")?;
    Ok((m, i))
}

/// `teeism isms evm`
pub async fn run(ctx: &Ctx, selection: &[String]) -> Result<()> {
    let rev = revision(ctx)?;
    // An EVM destination verifies a Celestia-origin attestation, so these pin the Celestia enclave.
    let (measurements, identity) = celestia_enclave(ctx)?;
    print::kv("measurements", &measurements);
    print::kv("identity", &identity);
    let hook = ctx
        .deployment()?
        .celestia
        .merkle_hook
        .context("no Celestia merkle tree hook; run `teeism celestia core`")?;
    config::write(ctx)?;
    let mut head_genesis: Option<Vec<u8>> = None;

    for chain in chains::evm_selection(selection)? {
        print::step(format!("TeeDcapIsm on {}", chain.label));
        let c = Chain::connect(ctx, chain).await?;
        let mut d = ctx.deployment()?;
        let entry = crate::steps::pccs::pccs_for(&d, chain)?.entrypoint;
        let mailbox = mailbox_of(&d, chain)?;
        let mut genesis = None;

        if let Some(existing) = d.evm(chain.name).and_then(|e| e.ism) {
            let ism = ITeeDcapIsm::new(existing, &c.p);
            let cur = format!(
                "{}",
                ism.enclaveMeasurements()
                    .call()
                    .await
                    .with_context(|| format!("reading {existing}"))?
            );
            // The first deployments have no VERSION(), and the call reverts: revision 1.
            let cur_rev = ism.VERSION().call().await.unwrap_or(1);
            // The verifier is fixed at construction, so a new PCCS deployment needs a new ISM.
            let cur_dcap = ism
                .dcap()
                .call()
                .await
                .with_context(|| format!("reading {existing}'s verifier"))?;
            if cur == measurements && cur_rev == rev && cur_dcap == entry {
                print::info(format!("{existing} already pins this enclave"));
                continue;
            }
            if cur_dcap != entry {
                print::info(format!("{existing} verifies through {cur_dcap}, not this PCCS's entrypoint {entry}; replacing it"));
            } else if cur_rev == rev && rev >= 3 {
                let owner = ism
                    .owner()
                    .call()
                    .await
                    .with_context(|| format!("reading {existing}'s owner"))?;
                if owner == c.from {
                    print::info(format!("re-pinning {existing}: {cur} -> {measurements}"));
                    let data = ITeeDcapIsm::setEnclaveCall {
                        _enclaveMeasurements: b32(&measurements)?,
                        _identityDigest: b32(&identity)?,
                    }
                    .abi_encode();
                    c.send("setEnclave", existing, data.into()).await?;
                    let got = format!("{}", ism.enclaveMeasurements().call().await?);
                    if got != measurements {
                        bail!("{existing} pins {got} after the re-pin, expected {measurements}");
                    }
                    print::ok(format!("{existing} re-pinned"));
                    continue;
                }
                print::warn(format!("{existing} is owned by {owner}, not {}; replacing it (its owner should re-pin it instead)", c.from));
            } else {
                print::info(format!(
                    "{existing} is contract revision {cur_rev}; replacing it with revision {rev}"
                ));
            }
            let old = ism
                .state()
                .call()
                .await
                .with_context(|| format!("reading {existing}'s state"))?;
            genesis = Some(resume_from(&old, &identity)?);
            print::info(format!("resuming from {existing}'s last state"));
        }
        let genesis = match genesis {
            Some(g) => g,
            None => {
                if head_genesis.is_none() {
                    head_genesis = Some(genesis_at_head(ctx, "celestia", &identity)?);
                }
                head_genesis.clone().unwrap_or_default()
            }
        };

        let art = Artifact::load(&ctx.contracts_dir(), "TeeDcapIsm", "TeeDcapIsm")?;
        let args = (
            entry,
            b32(&measurements)?,
            b32(&identity)?,
            b32(&hook)?,
            mailbox,
            Bytes::from(genesis),
            U256::from(MAX_QUOTE_SKEW),
        )
            .abi_encode_params();
        let ism = c
            .deploy("deploying TeeDcapIsm", art.initcode(&args).into())
            .await?;
        let got = pinned_measurements(&c.rpc, ism).await?;
        if got != measurements {
            bail!("the new ISM {ism} pins {got}, expected {measurements}");
        }
        d.evm_mut(chain.name).ism = Some(ism);
        ctx.save(&d)?;
        print::ok(format!("{ism}"));

        // A mailbox of our own (Eden) takes the first ISM as its default.
        if chain.canonical.is_none() {
            let mb = IMailbox::new(mailbox, &c.p);
            if mb
                .defaultIsm()
                .call()
                .await
                .with_context(|| format!("reading {mailbox}'s default ISM"))?
                == Address::ZERO
            {
                let hook_addr = d
                    .evm(chain.name)
                    .and_then(|e| e.merkle_hook)
                    .context("no merkle tree hook recorded")?;
                let data = IMailbox::initializeCall {
                    _owner: c.from,
                    _defaultIsm: ism,
                    _defaultHook: hook_addr,
                    _requiredHook: hook_addr,
                }
                .abi_encode();
                c.send("initializing the mailbox", mailbox, data.into()).await?;
                print::info(format!(
                    "mailbox {mailbox} initialized: default ISM {ism}, hooks {hook_addr}"
                ));
            }
        }
    }
    Ok(())
}
