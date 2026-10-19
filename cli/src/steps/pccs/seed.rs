//! `teeism pccs seed`: publish Intel's current collateral into each chain's PCCS. The monthly job.
//! Also fetching it: every artifact is signed by a chain ending in Intel SGX Root CA, so nothing
//! here is trusted; the DAOs on chain check each signature.

use std::collections::BTreeSet;

use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use anyhow::{bail, Context, Result};

use super::{ca, ICollateral, IRoles, JsonObj, ATTESTER_ROLE};
use crate::shared::chains::{self, EvmChain};
use crate::shared::evm::Chain;
use crate::shared::print;
use crate::shared::record::{Pccs, VersionedDaos};
use crate::shared::settings::Ctx;
use crate::shared::signer::Signer;

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
    Chain::with_signer(chain, chain.rpc(ctx), Signer::hot_key(ctx)?).await
}

// ---------------------------------------------------------------- fetching from Intel

pub const SGX_API: &str = "https://api.trustedservices.intel.com/sgx/certification/v4";
pub const TDX_API: &str = "https://api.trustedservices.intel.com/tdx/certification/v4";
const ROOT_CRL_FALLBACK: &str = "https://certificates.trustedservices.intel.com/IntelSGXRootCA.der";

/// A signed JSON object exactly as Intel served it, and its signature.
pub struct Signed {
    pub body: String,
    pub signature: Vec<u8>,
}

pub struct Intel {
    http: reqwest::Client,
}

pub struct Fetched {
    pub body: Vec<u8>,
    pub headers: reqwest::header::HeaderMap,
}

impl Intel {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
        }
    }

    /// GET with retries: Intel rate-limits, which is the common failure for a scheduled run.
    pub async fn get(&self, what: &str, url: &str) -> Result<Fetched> {
        let mut last = String::new();
        for attempt in 0..4 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(2 * attempt)).await;
            }
            match self
                .http
                .get(url)
                .timeout(std::time::Duration::from_secs(60))
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => {
                    let headers = r.headers().clone();
                    let body = r
                        .bytes()
                        .await
                        .with_context(|| format!("reading {what} from {url}"))?
                        .to_vec();
                    return Ok(Fetched { body, headers });
                }
                Ok(r) => last = format!("HTTP {}", r.status()),
                Err(e) => last = e.to_string(),
            }
        }
        bail!("could not fetch {what} from Intel ({url}): {last}")
    }

    pub async fn json(&self, what: &str, url: &str) -> Result<(String, reqwest::header::HeaderMap)> {
        let f = self.get(what, url).await?;
        let s = String::from_utf8(f.body).with_context(|| format!("{what} from {url} is not text"))?;
        Ok((s, f.headers))
    }

    /// The TDX evaluation numbers Intel publishes.
    pub async fn tdx_evals(&self) -> Result<(BTreeSet<u32>, String)> {
        let (raw, _) = self
            .json(
                "Intel's TDX evaluation numbers",
                &format!("{TDX_API}/tcbevaluationdatanumbers"),
            )
            .await?;
        let v: serde_json::Value =
            serde_json::from_str(&raw).context("Intel's evaluation-number list is not JSON")?;
        let list = v["tcbEvaluationDataNumbers"]["tcbEvalNumbers"]
            .as_array()
            .context("Intel's evaluation-number list has no tcbEvalNumbers")?;
        let set = list
            .iter()
            .filter_map(|x| x["tcbEvaluationDataNumber"].as_u64().map(|n| n as u32))
            .collect();
        Ok((set, raw))
    }

    /// The evaluation number Intel calls standard today: the one the verifier reads.
    pub async fn standard(&self, fmspc: &str) -> Result<u32> {
        let (raw, _) = self
            .json("TCB info", &format!("{TDX_API}/tcb?fmspc={fmspc}"))
            .await?;
        let v: serde_json::Value = serde_json::from_str(&raw).context("Intel's TCB info is not JSON")?;
        v["tcbInfo"]["tcbEvaluationDataNumber"]
            .as_u64()
            .map(|n| n as u32)
            .context("Intel's TCB info names no tcbEvaluationDataNumber")
    }
}

/// The evaluation numbers a new deployment needs: Intel's standard today, and every number
/// Intel already publishes after it, so the day the next one becomes standard is covered.
pub async fn evals_to_deploy(intel: &Intel, fmspc: &str) -> Result<BTreeSet<u32>> {
    let standard = intel.standard(fmspc).await?;
    let (published, _) = intel.tdx_evals().await?;
    let mut out: BTreeSet<u32> = published.into_iter().filter(|n| *n >= standard).collect();
    out.insert(standard);
    Ok(out)
}

/// The certificates in an issuer-chain header: URL-escaped PEM, leaf first, root last.
pub fn issuer_chain(headers: &reqwest::header::HeaderMap, name: &str) -> Result<Vec<Vec<u8>>> {
    let raw = headers
        .get(name)
        .with_context(|| format!("Intel's response carried no {name} header"))?
        .to_str()
        .with_context(|| format!("{name} is not text"))?;
    let pem = percent_encoding::percent_decode_str(raw)
        .decode_utf8()
        .context("issuer chain is not UTF-8")?;
    let mut certs = Vec::new();
    for block in pem.split("-----BEGIN CERTIFICATE-----").skip(1) {
        let body = block.split("-----END CERTIFICATE-----").next().unwrap_or("");
        let b64: String = body.chars().filter(|c| !c.is_whitespace()).collect();
        use base64::Engine;
        certs.push(
            base64::engine::general_purpose::STANDARD
                .decode(b64)
                .context("a certificate in the chain is not base64")?,
        );
    }
    if certs.len() < 2 {
        bail!(
            "{name} has {} certificate(s), expected the issuer and the root",
            certs.len()
        );
    }
    Ok(certs)
}

/// The root CA CRL's location, from the root certificate's distribution point.
pub fn crl_url(root: &[u8]) -> String {
    use x509_parser::extensions::{DistributionPointName, GeneralName, ParsedExtension};
    if let Ok((_, cert)) = x509_parser::parse_x509_certificate(root) {
        for ext in cert.extensions() {
            if let ParsedExtension::CRLDistributionPoints(points) = ext.parsed_extension() {
                for p in points.iter() {
                    if let Some(DistributionPointName::FullName(names)) = &p.distribution_point {
                        for n in names {
                            if let GeneralName::URI(uri) = n {
                                return uri.to_string();
                            }
                        }
                    }
                }
            }
        }
    }
    ROOT_CRL_FALLBACK.to_string()
}

/// The object under `key`, lifted from Intel's response as the exact bytes it served. The
/// signature covers those bytes, so it is never re-serialised.
pub fn signed_object(raw: &str, key: &str) -> Result<Signed> {
    let at = raw
        .find(&format!("\"{key}\""))
        .with_context(|| format!("no \"{key}\" in Intel's response"))?;
    let open = at
        + raw[at..]
            .find('{')
            .with_context(|| format!("\"{key}\" is not an object"))?;
    let mut depth = 0usize;
    let mut end = None;
    for (i, ch) in raw[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end.with_context(|| format!("\"{key}\" never closes"))?;
    let v: serde_json::Value = serde_json::from_str(raw).context("Intel's response is not JSON")?;
    let sig = v["signature"]
        .as_str()
        .context("Intel's response has no signature")?;
    Ok(Signed {
        body: raw[open..=end].to_string(),
        signature: hex::decode(sig).context("Intel's signature is not hex")?,
    })
}
