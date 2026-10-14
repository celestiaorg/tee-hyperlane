//! `teeism pccs seed`: publish Intel's current collateral into one chain's PCCS. The monthly job.

use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use anyhow::{bail, Context, Result};

use super::{ca, ICollateral, IRoles, JsonObj, ATTESTER_ROLE};
use crate::shared::chains::{self, EvmChain};
use crate::shared::evm::Chain;
use crate::shared::intel::{crl_url, issuer_chain, signed_object, Intel, SGX_API, TDX_API};
use crate::shared::print;
use crate::shared::record::{Pccs, VersionedDaos};
use crate::shared::settings::Ctx;

fn obj(raw: &str, key: &str) -> Result<JsonObj> {
    let s = signed_object(raw, key)?;
    Ok(JsonObj {
        body: s.body,
        signature: s.signature.into(),
    })
}

/// Publish everything the verifier reads, for every evaluation number the router has
/// contracts for. Every artifact is Intel-signed and the DAOs check that signature, so the key
/// that sends needs no trust, only the uploader role.
pub async fn publish(c: &Chain, p: &Pccs, fmspc: &str, dry_run: bool) -> Result<()> {
    let intel = Intel::new();
    let (published, eval_tdx_raw) = intel.tdx_evals().await?;
    let standard = intel.standard(fmspc).await?;

    // Check everything before the first transaction, so a gap is reported as nothing sent
    // rather than as a half-seeded chain.
    let mut problems = Vec::new();
    if !p.evals.contains_key(&standard) {
        problems.push(format!(
            "evaluation {standard} is Intel's standard today, which is what the verifier reads, but this \
             deployment has no contracts for it. Add them: teeism pccs tcb-eval --chain {} {standard}",
            c.name()
        ));
    }
    let evals: Vec<(u32, VersionedDaos)> = p
        .evals
        .iter()
        .filter(|(n, _)| published.contains(n))
        .map(|(n, d)| (*n, d.clone()))
        .collect();
    for (n, _) in p.evals.iter().filter(|(n, _)| !published.contains(n)) {
        print::warn(format!("Intel no longer publishes evaluation {n}; skipping it"));
    }
    for (n, d) in &evals {
        for dao in [d.qe_identity, d.fmspc_tcb] {
            if !IRoles::new(dao, &c.p)
                .hasAnyRole(c.from, U256::from(ATTESTER_ROLE))
                .call()
                .await?
            {
                problems.push(format!(
                    "{} lacks ATTESTER_ROLE on {dao} (evaluation {n}); `teeism pccs deploy` grants it",
                    c.from
                ));
            }
        }
    }
    if !IRoles::new(p.tcb_eval_dao, &c.p)
        .hasAnyRole(c.from, U256::from(ATTESTER_ROLE))
        .call()
        .await?
    {
        problems.push(format!(
            "{} lacks ATTESTER_ROLE on the TCB evaluation DAO {}",
            c.from, p.tcb_eval_dao
        ));
    }
    if !problems.is_empty() {
        bail!(
            "nothing was published on {}:\n  - {}",
            c.chain.label,
            problems.join("\n  - ")
        );
    }
    print::info(format!(
        "evaluations {:?} (Intel's standard is {standard}), seeder {}",
        evals.iter().map(|(n, _)| n).collect::<Vec<_>>(),
        c.from
    ));

    let tcb = intel
        .get("TCB info", &format!("{TDX_API}/tcb?fmspc={fmspc}"))
        .await?;
    let tcb_chain = issuer_chain(&tcb.headers, "TCB-Info-Issuer-Chain")?;
    let plat = intel
        .get(
            "the PCK platform CRL",
            &format!("{SGX_API}/pckcrl?ca=platform&encoding=der"),
        )
        .await?;
    let proc = intel
        .get(
            "the PCK processor CRL",
            &format!("{SGX_API}/pckcrl?ca=processor&encoding=der"),
        )
        .await?;
    let plat_chain = issuer_chain(&plat.headers, "SGX-PCK-CRL-Issuer-Chain")?;
    let proc_chain = issuer_chain(&proc.headers, "SGX-PCK-CRL-Issuer-Chain")?;
    let signing = tcb_chain[0].clone();
    let root = tcb_chain[1].clone();
    let root_crl_url = crl_url(&root);
    let root_crl = intel.get("the root CA CRL", &root_crl_url).await?.body;
    let (eval_sgx_raw, _) = intel
        .json(
            "Intel's SGX evaluation numbers",
            &format!("{SGX_API}/tcbevaluationdatanumbers"),
        )
        .await?;
    let mut per_eval = Vec::new();
    for (n, d) in &evals {
        let (tcb_raw, _) = intel
            .json(
                &format!("evaluation-{n} TCB info"),
                &format!("{TDX_API}/tcb?fmspc={fmspc}&tcbEvaluationDataNumber={n}"),
            )
            .await?;
        let (qe_raw, _) = intel
            .json(
                &format!("the evaluation-{n} QE identity"),
                &format!("{TDX_API}/qe/identity?tcbEvaluationDataNumber={n}"),
            )
            .await?;
        per_eval.push((
            *n,
            d.clone(),
            obj(&tcb_raw, "tcbInfo")?,
            obj(&qe_raw, "enclaveIdentity")?,
        ));
    }

    if dry_run {
        print::info("dry run: every check passed and Intel served every artifact; nothing was sent");
        return Ok(());
    }

    use ICollateral as C;
    let pcs = p.pcs_dao;
    let mut uploads: Vec<(String, Address, Vec<u8>)> = vec![
        (
            "root CA certificate".into(),
            pcs,
            C::upsertPcsCertificatesCall {
                ca: ca::ROOT,
                cert: root.into(),
            }
            .abi_encode(),
        ),
        (
            "TCB signing certificate".into(),
            pcs,
            C::upsertPcsCertificatesCall {
                ca: ca::SIGNING,
                cert: signing.into(),
            }
            .abi_encode(),
        ),
        (
            "PCK platform CA certificate".into(),
            pcs,
            C::upsertPcsCertificatesCall {
                ca: ca::PLATFORM,
                cert: plat_chain[0].clone().into(),
            }
            .abi_encode(),
        ),
        // The processor CRL is issued by a different CA, whose certificate has to be there too
        // or its CRL has nothing to be verified against.
        (
            "PCK processor CA certificate".into(),
            pcs,
            C::upsertPcsCertificatesCall {
                ca: ca::PROCESSOR,
                cert: proc_chain[0].clone().into(),
            }
            .abi_encode(),
        ),
        (
            format!("root CA CRL ({root_crl_url})"),
            pcs,
            C::upsertRootCACrlCall {
                rootcacrl: root_crl.into(),
            }
            .abi_encode(),
        ),
        (
            "PCK platform CRL".into(),
            pcs,
            C::upsertPckCrlCall {
                ca: ca::PLATFORM,
                crl: plat.body.into(),
            }
            .abi_encode(),
        ),
        (
            "PCK processor CRL".into(),
            pcs,
            C::upsertPckCrlCall {
                ca: ca::PROCESSOR,
                crl: proc.body.into(),
            }
            .abi_encode(),
        ),
    ];
    for (plat_name, raw) in [("SGX", &eval_sgx_raw), ("TDX", &eval_tdx_raw)] {
        let obj = obj(raw, "tcbEvaluationDataNumbers")?;
        uploads.push((
            format!("{plat_name} evaluation numbers"),
            p.tcb_eval_dao,
            C::upsertTcbEvaluationDataCall { tcbEvalObj: obj }.abi_encode(),
        ));
    }
    for (n, d, tcb_obj, qe_obj) in per_eval {
        // EnclaveId TD_QE = 2, PCS API version 4.
        uploads.push((
            format!("evaluation-{n} TDX QE identity"),
            d.qe_identity,
            C::upsertEnclaveIdentityCall {
                id: U256::from(2),
                version: U256::from(4),
                enclaveIdentityObj: qe_obj,
            }
            .abi_encode(),
        ));
        uploads.push((
            format!("evaluation-{n} TDX TCB info"),
            d.fmspc_tcb,
            C::upsertFmspcTcbCall { tcbInfoObj: tcb_obj }.abi_encode(),
        ));
    }

    // Every upload is attempted, and the run fails at the end if any did not land. The DAOs
    // refuse an artifact identical to the one they hold, which is "already current", not a
    // failure: those need very different responses.
    let mut failures = Vec::new();
    for (label, to, data) in uploads {
        match c.send(&label, to, data.into()).await {
            Ok(_) => print::info(format!("{label}: published")),
            Err(e) if format!("{e:#}").contains("Duplicate_Collateral") => {
                print::info(format!("{label}: already current"))
            }
            Err(e) => {
                print::bad(format!("{label}: {e:#}"));
                failures.push(label);
            }
        }
    }
    if !failures.is_empty() {
        bail!(
            "{} artifact(s) could not be published on {}: {} (the reasons are above)",
            failures.len(),
            c.chain.label,
            failures.join(", ")
        );
    }
    Ok(())
}

/// `teeism pccs seed`
pub async fn run(ctx: &Ctx, chains: &[String], fmspc: &str, dry_run: bool) -> Result<()> {
    let d = ctx.deployment()?;
    for chain in chains::evm_selection(chains)? {
        let p = super::pccs_for(&d, chain)?;
        print::step(format!("seeding {}", chain.label));
        publish(&seeder(ctx, chain).await?, p, fmspc, dry_run).await?;
    }
    Ok(())
}

/// The relayer's key signs collateral uploads. It needs the uploader role and nothing else, so
/// a Ledger-owned deployment is still seeded unattended.
pub async fn seeder(ctx: &Ctx, chain: &'static EvmChain) -> Result<Chain> {
    let key = ctx.require("EVM_PRIVATE_KEY", "it uploads Intel's collateral")?;
    let s: alloy::signers::local::PrivateKeySigner = key
        .trim()
        .trim_start_matches("0x")
        .parse()
        .context("EVM_PRIVATE_KEY is not a valid private key")?;
    let from = s.address();
    Chain::with_wallet(
        chain,
        chain.rpc(ctx),
        alloy::network::EthereumWallet::from(s),
        from,
    )
    .await
}
