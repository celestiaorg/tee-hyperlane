//! A devnet faucet: a fixed grant of TIA, once per address.
//!
//! It signs with `celestia-appd` for the same reason the relayer does - the binary already
//! knows the transaction format and the keyring, and reimplementing either here would be a
//! second thing to keep correct.
//!
//! The funding key is deliberately **not** the relayer's. A faucet is the one endpoint on
//! this deployment that a stranger can spend from, so it draws on an account whose whole
//! balance is what it is allowed to give away. Draining it stops the faucet and nothing else;
//! draining the relayer would stop the bridge.
//!
//! One claim per address, ever. Addresses are free to mint, so this bounds a careless user
//! rather than a determined one - the real bound is the funding account's balance.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// 1000 TIA, in utia. Celestia's denom has six decimals.
const CLAIM_UTIA: u64 = 1_000_000_000;
pub const CLAIM_TIA: u64 = CLAIM_UTIA / 1_000_000;

/// A Celestia bech32 account address: the prefix, then 38 data characters.
const ADDRESS_LEN: usize = 47;
const ADDRESS_PREFIX: &str = "celestia1";
/// bech32's alphabet: no 1, b, i or o, so a transcription slip is rejected rather than paid.
const BECH32_CHARSET: &str = "qpzry9x8gf2tvdw0s3jn54khce6mua7l";

#[derive(Debug, Serialize)]
pub struct Claim {
    pub address: String,
    pub amount_tia: u64,
    pub tx_hash: String,
}

#[derive(Debug, Deserialize)]
pub struct ClaimRequest {
    pub address: String,
}

#[derive(Debug, thiserror::Error)]
pub enum FaucetError {
    #[error("this address has already claimed")]
    AlreadyClaimed,
    #[error("not a Celestia address")]
    BadAddress,
    #[error("the faucet is not configured on this deployment")]
    NotConfigured,
    #[error("the faucet could not send: {0}")]
    SendFailed(String),
}

#[derive(Clone)]
pub struct Faucet {
    claims: PathBuf,
    appd: String,
    home: String,
    chain_id: String,
    node: String,
    key: String,
}

impl Faucet {
    /// Configured from the same environment the relayer unit already sets. Absent `CELHOME`
    /// there is no keyring to sign with, so the endpoint reports itself unconfigured rather
    /// than failing per request.
    pub fn from_env(proof_dir: &Path) -> Option<Self> {
        let home = std::env::var("CELHOME").ok()?;
        Some(Self {
            claims: proof_dir.join(".faucet"),
            appd: std::env::var("APPD").unwrap_or_else(|_| "celestia-appd".into()),
            home,
            chain_id: std::env::var("CELESTIA_CHAIN_ID").unwrap_or_else(|_| "teeism-local".into()),
            node: std::env::var("CELESTIA_RPC").unwrap_or_else(|_| "http://localhost:26657".into()),
            key: std::env::var("FAUCET_KEY").unwrap_or_else(|_| "faucet".into()),
        })
    }

    pub fn already_claimed(&self, address: &str) -> bool {
        self.claims.join(address).exists()
    }

    pub fn claim(&self, address: &str) -> Result<Claim, FaucetError> {
        if !is_celestia_address(address) {
            return Err(FaucetError::BadAddress);
        }
        std::fs::create_dir_all(&self.claims).map_err(|e| FaucetError::SendFailed(e.to_string()))?;

        // The marker is written before the send, not after, and `create_new` is what makes
        // that a lock: two requests racing for the same address cannot both create it, so the
        // loser is refused rather than paid a second time. A failed send removes it again.
        let marker = self.claims.join(address);
        let mut file = match std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&marker)
        {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(FaucetError::AlreadyClaimed)
            }
            Err(e) => return Err(FaucetError::SendFailed(e.to_string())),
        };

        match self.send(address) {
            Ok(tx_hash) => {
                let record = serde_json::json!({
                    "address": address,
                    "amount_utia": CLAIM_UTIA,
                    "tx_hash": tx_hash,
                    "at": std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or_default(),
                });
                let _ = file.write_all(record.to_string().as_bytes());
                info!(address, tx_hash, "faucet paid");
                Ok(Claim {
                    address: address.to_string(),
                    amount_tia: CLAIM_TIA,
                    tx_hash,
                })
            }
            Err(e) => {
                // Nothing was paid, so the address must be free to try again.
                drop(file);
                let _ = std::fs::remove_file(&marker);
                warn!(address, error = %e, "faucet send failed");
                Err(FaucetError::SendFailed(e))
            }
        }
    }

    fn send(&self, to: &str) -> Result<String, String> {
        let out = Command::new(&self.appd)
            .args([
                "tx",
                "bank",
                "send",
                &self.key,
                to,
                &format!("{CLAIM_UTIA}utia"),
                "--home",
                &self.home,
                "--keyring-backend",
                "test",
                "--chain-id",
                &self.chain_id,
                "--node",
                &self.node,
                "--fees",
                "200000utia",
                "--gas",
                "200000",
                "--broadcast-mode",
                "sync",
                "-y",
                "-o",
                "json",
            ])
            .output()
            .map_err(|e| format!("could not run {}: {e}", self.appd))?;

        let stdout = String::from_utf8_lossy(&out.stdout);
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(first_line(if stderr.trim().is_empty() {
                &stdout
            } else {
                &stderr
            }));
        }

        let parsed: serde_json::Value =
            serde_json::from_str(stdout.trim()).map_err(|_| first_line(&stdout))?;
        // A non-zero code is a rejected transaction, which `celestia-appd` still exits 0 on.
        match parsed.get("code").and_then(|c| c.as_u64()) {
            Some(0) | None => {}
            Some(code) => {
                let why = parsed
                    .get("raw_log")
                    .and_then(|l| l.as_str())
                    .unwrap_or("no reason given");
                return Err(format!("code {code}: {why}"));
            }
        }
        parsed
            .get("txhash")
            .and_then(|h| h.as_str())
            .map(|h| h.to_string())
            .ok_or_else(|| "no txhash in the response".to_string())
    }
}

fn first_line(text: &str) -> String {
    text.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("no output")
        .chars()
        .take(200)
        .collect()
}

/// Shape-check only. The chain rejects a well-formed address that fails its checksum, so this
/// exists to turn a typo into a clear 400 rather than a confusing send failure.
fn is_celestia_address(address: &str) -> bool {
    address.len() == ADDRESS_LEN
        && address.starts_with(ADDRESS_PREFIX)
        && address[ADDRESS_PREFIX.len()..]
            .chars()
            .all(|c| BECH32_CHARSET.contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_address_is_accepted() {
        assert!(is_celestia_address(
            "celestia1dz8fma3pdgg3s0f3qwr4mdae7kgpxysmnccjdx"
        ));
    }

    #[test]
    fn another_chains_address_is_not() {
        assert!(!is_celestia_address(
            "cosmos1dz8fma3pdgg3s0f3qwr4mdae7kgpxysmxxxxxx"
        ));
        assert!(!is_celestia_address(
            "0x318d22faa1e0f29eac7Ef644A8FaC676F6688d1e"
        ));
    }

    #[test]
    fn characters_outside_bech32_are_rejected() {
        // 'b', 'i' and 'o' are not in the alphabet, so a transcription slip cannot be paid.
        let bad = format!("celestia1{}", "b".repeat(ADDRESS_LEN - ADDRESS_PREFIX.len()));
        assert!(!is_celestia_address(&bad));
    }

    #[test]
    fn length_is_checked() {
        assert!(!is_celestia_address("celestia1"));
        assert!(!is_celestia_address(&format!("celestia1{}", "q".repeat(60))));
    }

    #[test]
    fn the_grant_is_a_thousand_tia() {
        assert_eq!(CLAIM_TIA, 1_000);
        assert_eq!(CLAIM_UTIA, 1_000_000_000);
    }
}
