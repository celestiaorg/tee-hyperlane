//! Reading an enclave: its quote, what each kind of ISM pins from it, mr_td, and the Phala
//! defaults the other enclave steps use.
//!
//! The one piece that cannot be local: a TDX quote has to come from real Intel hardware.

use std::time::Duration;

use alloy::primitives::keccak256;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::shared::chains::ENCLAVES;
use crate::shared::settings::Ctx;

/// The dstack OS image the enclave identity is defined for: 0.6, whose event log has no
/// `mr-kms`. A 0.5 CVM cannot be pinned (x/teeism's identity v2). Asked for explicitly, so a
/// change of Phala's default never moves the identity unnoticed.
pub const DEFAULT_OS_IMAGE: &str = "dstack-0.6.1";
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

/// What the signed `mr_config_id` binds, checked against `compose_hash`.
///
/// dstack 0.5 wrote v1: `0x01 ++ compose_hash`. dstack 0.6 writes v3:
/// `0x03 ++ sha256("dstack-mr-config-v3:" ++ 0x00 ++ document)`, where the document is the
/// canonical (sorted, compact) JSON of the compose hash, the app and instance ids and the KMS
/// key. The document is rebuilt from what the enclave reports; those values are unsigned, but
/// only the true ones hash to the signed `mr_config_id`. Returns what was matched.
pub fn mr_config_binds(cfg: &[u8], id: &Identity, compose_hash: &str) -> Result<String> {
    anyhow::ensure!(cfg.len() == 48, "mr_config_id is {} bytes, not 48", cfg.len());
    anyhow::ensure!(
        cfg[33..].iter().all(|b| *b == 0),
        "mr_config_id is not zero-padded"
    );
    match cfg[0] {
        1 => {
            anyhow::ensure!(
                hex::encode(&cfg[1..33]) == compose_hash,
                "mr_config_id v1 carries {}, not the compose hash {compose_hash}",
                hex::encode(&cfg[1..33])
            );
            Ok(format!("v1, the compose hash {compose_hash}"))
        }
        3 => {
            let info = &id.raw["info"];
            let ac: Value = serde_json::from_str(&app_compose(id)?).context("app_compose is not JSON")?;
            let text = |v: &Value| v.as_str().map(|s| s.trim_start_matches("0x").to_lowercase());
            let key_provider = text(&ac["key_provider"]).unwrap_or_else(|| {
                if ac["kms_enabled"].as_bool() == Some(true) {
                    "kms".into()
                } else {
                    "none".into()
                }
            });
            let mut base = std::collections::BTreeMap::new();
            base.insert("version", serde_json::json!(3));
            base.insert("compose_hash", serde_json::json!(compose_hash));
            base.insert("key_provider", serde_json::json!(key_provider));
            if let Some(app) = text(&info["app_id"]) {
                base.insert("app_id", serde_json::json!(app));
            }
            // dstack leaves these out when empty, and adds init_script_hashes for manifest v3.
            let optional: Vec<(&str, Value)> = [
                ("instance_id", text(&info["instance_id"]).map(Value::from)),
                (
                    "key_provider_id",
                    text(&ac["key_provider_id"])
                        .filter(|s| !s.is_empty())
                        .map(Value::from),
                ),
                ("init_script_hashes", Some(serde_json::json!([]))),
            ]
            .into_iter()
            .filter_map(|(k, v)| v.map(|v| (k, v)))
            .collect();
            for mask in 0..(1u32 << optional.len()) {
                let mut doc = base.clone();
                for (i, (k, v)) in optional.iter().enumerate() {
                    if mask & (1 << i) != 0 {
                        doc.insert(k, v.clone());
                    }
                }
                let document = serde_json::to_string(&doc)?;
                let mut h = Sha256::new();
                h.update(b"dstack-mr-config-v3:\x00");
                h.update(document.as_bytes());
                if h.finalize().as_slice() == &cfg[1..33] {
                    return Ok(format!("v3, the document {document}"));
                }
            }
            bail!("mr_config_id v3 matches no document built from compose hash {compose_hash} and the enclave's app id, instance id and KMS key")
        }
        v => bail!("mr_config_id version {v} is not one this CLI knows (dstack 0.5 wrote 1, 0.6 writes 3)"),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    const COMPOSE: &str = "ee935eba5e1771db4a3ce71900bed00f77213335a0abbc24d622f64a419bc543";

    /// The values the dstack 0.6.1 CVM `4de9f0be…` reported on 2026-10-26, and its signed
    /// mr_config_id.
    fn live() -> (Identity, Vec<u8>) {
        let app_compose = serde_json::json!({
            "manifest_version": 3,
            "key_provider": "kms",
            "kms_enabled": true,
            "key_provider_id": "3059301306072a8648ce3d020106082a8648ce3d030107034200048844eb42ccdf8c52fd4f174f362fcb9bbd19c45fd48f1edec2d8f1ca23536ec1a74021b4cee610c074f8294d431b2b7fee2c39e5333fdaf0a4522d43fb159d9f",
            "docker_compose_file": "services: {}",
        });
        let raw = serde_json::json!({ "info": {
            "app_id": "4de9f0be7b151f67fd717d8f06ac45da5ff70d84",
            "instance_id": "deb8f3729c1e49fbab0c7976b9722f5ab0029bcb",
            "compose_hash": COMPOSE,
            "tcb_info": serde_json::json!({ "app_compose": app_compose.to_string() }).to_string(),
        }});
        let cfg = hex::decode("0330e600fcadc56d7614dc9050808ab88261ccd6209f1f8ee9f0310687d827eb6f000000000000000000000000000000").unwrap();
        (
            Identity {
                raw,
                quote: Vec::new(),
            },
            cfg,
        )
    }

    #[test]
    fn a_dstack_06_mr_config_id_binds_the_compose_hash() {
        let (id, cfg) = live();
        let what = mr_config_binds(&cfg, &id, COMPOSE).unwrap();
        assert!(what.contains(COMPOSE), "{what}");
    }

    #[test]
    fn another_compose_hash_is_refused() {
        let (id, cfg) = live();
        let other = COMPOSE.replace("ee93", "ee94");
        assert!(mr_config_binds(&cfg, &id, &other).is_err());
    }

    #[test]
    fn a_dstack_05_mr_config_id_is_the_compose_hash() {
        let (id, _) = live();
        let mut cfg = vec![0u8; 48];
        cfg[0] = 1;
        cfg[1..33].copy_from_slice(&hex::decode(COMPOSE).unwrap());
        assert!(mr_config_binds(&cfg, &id, COMPOSE).is_ok());
        cfg[1] ^= 1;
        assert!(mr_config_binds(&cfg, &id, COMPOSE).is_err());
    }
}
