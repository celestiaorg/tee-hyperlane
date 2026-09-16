//! Submitting to a destination chain.
//!
//! Both chains run the same protocol - advance the state, authorise the batch, deliver each
//! message - so this is one shape with two backends. Signing is delegated to `cast` and
//! `celestia-appd` rather than reimplemented: they are already required to deploy the bridge,
//! and a relayer key is the least sensitive thing in the system. It pays gas and can stall
//! the bridge; it cannot make either chain accept a message the enclave did not attest.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};
use base64::Engine as _;
use tracing::info;

use crate::config::ChainConfig;

pub struct Destination {
    chain: ChainConfig,
    ism: String,
    /// The destination verifies the enclave's quote itself, so no proof is involved and the
    /// direct submit script is the right one.
    direct: bool,
}

impl Destination {
    pub fn new(chain: ChainConfig, ism: String, direct: bool) -> Self {
        Self { chain, ism, direct }
    }

    /// Run one proved batch to completion: update, authorise, deliver.
    ///
    /// Ordering is forced by the ISM: `submitMessages` checks the batch against the *current*
    /// root, so the state has to move first.
    pub fn submit(&self, proved: &Path) -> Result<()> {
        let (script, mut command) = self.script_command();
        command.arg(proved);
        info!(script, ism = %self.ism, "submitting");
        let status = command
            .status()
            .with_context(|| format!("running {script}"))?;
        anyhow::ensure!(status.success(), "{script} exited with {status}");
        Ok(())
    }

    /// Check the relayer can actually pay to submit, before anything expensive happens.
    ///
    /// Proving is hours of CPU and submission is the step after it, so a route whose fee
    /// account is empty burns those hours and then fails - repeatedly, because the next tick
    /// starts over. Three routes here did exactly that for a day. The question is cheap to
    /// ask and the answer does not change mid-proof, so ask it first.
    pub fn preflight(&self) -> Result<()> {
        let (script, mut command) = self.script_command();
        command.arg("--preflight");
        let output = command
            .output()
            .with_context(|| format!("running {script} --preflight"))?;
        if output.status.success() {
            return Ok(());
        }
        let reason = String::from_utf8_lossy(&output.stderr);
        let reason = reason.trim();
        // An older script that does not know the flag must not block the route.
        if reason.contains("usage:") || reason.is_empty() {
            return Ok(());
        }
        anyhow::bail!("{reason}")
    }

    fn script_command(&self) -> (&'static str, Command) {
        let script = match &self.chain {
            ChainConfig::Celestia { ism_module, .. } if ism_module == "teeism" => {
                "submit-celestia-teeism.sh"
            }
            ChainConfig::Celestia { .. } => "submit-celestia.sh",
            // An EVM destination has both shapes too. Picking by `direct` rather than by
            // chain kind is what was missing: every EVM route got the proof-carrying script,
            // which reads a vkey and a Groth16 proof that a direct route never produces.
            _ if self.direct => "submit-evm-teeism.sh",
            _ => "submit-evm.sh",
        };
        let path = deploy_dir().join(script);

        let mut command = Command::new(&path);
        command
            .env("TEE_ISM", &self.ism)
            .env("CELESTIA_ISM", &self.ism);

        // Which chain to send to comes from the route, so a second EVM destination is config.
        match &self.chain {
            ChainConfig::Ethereum {
                execution_rpc,
                mailbox,
                domain,
                ..
            } => {
                command
                    .env("EVM_RPC", execution_rpc)
                    .env("MAILBOX", mailbox)
                    .env("LOCAL_DOMAIN", domain.to_string());
            }
            ChainConfig::EthereumL2 {
                l2_rpc,
                mailbox,
                domain,
                ..
            } => {
                command
                    .env("EVM_RPC", l2_rpc)
                    .env("MAILBOX", mailbox)
                    .env("LOCAL_DOMAIN", domain.to_string());
            }
            ChainConfig::Celestia {
                rpc,
                mailbox_id,
                domain,
                ism_module,
                ..
            } => {
                command
                    .env("CELESTIA_RPC", rpc)
                    .env("CELESTIA_MAILBOX", mailbox_id)
                    .env("CELESTIA_DOMAIN", domain.to_string())
                    .env("CELESTIA_ISM_MODULE", ism_module);
            }
        }

        (script, command)
    }
}

fn deploy_dir() -> std::path::PathBuf {
    std::env::var("TEE_HYPERLANE_DEPLOY_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("deploy"))
}

/// Read the ISM's current trusted state, hex-encoded.
///
/// This is the only progress marker the relayer has, and it lives on chain — which is what
/// makes restarting the same as continuing.
pub fn read_ism_state(chain: &ChainConfig, ism: &str) -> Result<String> {
    let output = match chain {
        ChainConfig::Celestia {
            rpc, ism_module, ..
        } => Command::new(celestia_appd())
            .args([
                "query", ism_module, "ism", ism, "--node", rpc, "-o", "json",
            ])
            .output()
            .with_context(|| format!("celestia-appd query {ism_module} ism"))?,
        ChainConfig::Ethereum { execution_rpc, .. }
        | ChainConfig::EthereumL2 {
            l2_rpc: execution_rpc,
            ..
        } => Command::new("cast")
            .args(["call", ism, "state()(bytes)", "--rpc-url", execution_rpc])
            .output()
            .context("cast call state()")?,
    };
    anyhow::ensure!(
        output.status.success(),
        "reading ISM state failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    let text = String::from_utf8(output.stdout)?;

    Ok(match chain {
        ChainConfig::Celestia { .. } => {
            let parsed: serde_json::Value = serde_json::from_str(&text)?;
            let encoded = parsed["ism"]["state"]
                .as_str()
                .context("ism.state missing from query output")?;
            // celestia-appd renders `bytes` fields as base64; everything downstream expects
            // hex, and the two are easy to mistake for one another.
            let raw = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .context("ism.state is not base64")?;
            format!("0x{}", hex::encode(raw))
        }
        _ => text.trim().to_string(),
    })
}

fn celestia_appd() -> String {
    std::env::var("APPD").unwrap_or_else(|_| "celestia-appd".to_string())
}
