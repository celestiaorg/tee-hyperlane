//! Reading an enclave: its quote, what each kind of ISM pins from it, mr_td, and the Phala
//! defaults the other enclave steps use.
//!
//! The one piece that cannot be local: a TDX quote has to come from real Intel hardware.

use std::time::Duration;

use alloy::primitives::keccak256;
use anyhow::{bail, Context, Result};
use serde_json::Value;

use crate::shared::chains::ENCLAVES;
use crate::shared::settings::Ctx;

pub const DEFAULT_INSTANCE: &str = "tdx.small";

/// The chains a CVM target serves.
pub fn chains_in(target: &str) -> Vec<&'static str> {
    if target == "all" {
        ENCLAVES.iter().map(|(c, _)| *c).collect()
    } else {
        ENCLAVES
            .iter()
            .map(|(c, _)| *c)
            .filter(|c| *c == target)
            .collect()
    }
}

/// What `GET /identity` returns: the quote, and dstack's unsigned description of the CVM.
pub struct Identity {
    pub raw: Value,
    pub quote: Vec<u8>,
}

pub async fn fetch_identity(url: &str) -> Result<Identity> {
    let raw: Value = reqwest::Client::new()
        .get(format!("{url}/identity"))
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .with_context(|| format!("the enclave at {url} did not answer /identity"))?
        .error_for_status()
        .with_context(|| format!("the enclave at {url} refused /identity"))?
        .json()
        .await
        .with_context(|| format!("{url}/identity is not JSON"))?;
    let q = raw["quote"]
        .as_str()
        .with_context(|| format!("{url}/identity carries no quote"))?;
    let quote = hex::decode(q.trim_start_matches("0x"))
        .with_context(|| format!("the quote from {url} is not hex"))?;
    Ok(Identity { raw, quote })
}

/// The TD report body: after the 48-byte quote header, plus a 6-byte descriptor in v5.
pub fn report_body(quote: &[u8]) -> Result<&[u8]> {
    if quote.len() < 48 + 6 + 584 {
        bail!(
            "the quote is {} bytes, too short to hold a TD report",
            quote.len()
        );
    }
    let version = u16::from_le_bytes([quote[0], quote[1]]);
    let start = match version {
        4 => 48,
        5 => 48 + 6,
        v => bail!("quote version {v}; only v4 and v5 are understood"),
    };
    Ok(&quote[start..])
}

/// What a TeeDcapIsm pins: keccak256(mr_td ++ mr_config_id ++ rtmr0 ++ rtmr1 ++ rtmr2). RTMR3 is
/// left out on purpose: it carries the app and instance ids, so including it would tie the ISM
/// to one CVM rather than to the code it runs.
pub fn measurements(quote: &[u8]) -> Result<String> {
    let b = report_body(quote)?;
    let mut pre = b[136..232].to_vec();
    pre.extend_from_slice(&b[328..472]);
    Ok(format!("{}", keccak256(pre)))
}

/// mr_td, the measurement of the TD's firmware and OS image: what a new dstack OS moves.
pub fn mr_td(quote: &[u8]) -> Result<String> {
    Ok(format!("0x{}", hex::encode(&report_body(quote)?[136..184])))
}

/// The dstack app_compose the enclave measured, from its unsigned info.
pub fn app_compose(id: &Identity) -> Result<String> {
    let tcb = &id.raw["info"]["tcb_info"];
    let tcb: Value = match tcb.as_str() {
        Some(s) => serde_json::from_str(s).context("tcb_info is not JSON")?,
        None => tcb.clone(),
    };
    tcb["app_compose"]
        .as_str()
        .map(str::to_string)
        .context("the enclave reported no app_compose")
}

pub fn measured_compose(id: &Identity) -> Result<String> {
    let ac: Value = serde_json::from_str(&app_compose(id)?).context("app_compose is not JSON")?;
    ac["docker_compose_file"]
        .as_str()
        .map(str::to_string)
        .context("app_compose has no docker_compose_file")
}

pub async fn healthy(url: &str) -> bool {
    matches!(
        reqwest::Client::new().get(format!("{url}/health")).timeout(Duration::from_secs(15)).send().await,
        Ok(r) if r.status().is_success()
    )
}

pub fn identity_file(ctx: &Ctx, chain: &str) -> std::path::PathBuf {
    ctx.state_dir.join("identity").join(format!("{chain}.json"))
}
