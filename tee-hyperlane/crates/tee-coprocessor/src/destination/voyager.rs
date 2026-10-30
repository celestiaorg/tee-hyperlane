//! Delivers to a voyager chain: hands its bridge lane the attestation with fresh Intel
//! collateral, as for Celestia, then each message with the merkle branch that proves it against
//! the attested root, as `TeeDcapIsm.verify` takes it.
//!
//! The node checks the quote at admission and the lane checks the transition, in a later slot.
//! Neither answers with the result, so this waits on the ISM's state and the delivered set.

use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::json;
use tee_node::state::IsmState;
use tracing::debug;

use super::{Batch, Delivery, Destination, Outcome, Submitted, Wallet};
use crate::origin::l2::voyager::Node;

/// How long the lane may take to execute what was submitted. Slots come every few
/// milliseconds under load and at least every header interval when idle.
const LANDING_TIMEOUT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(250);

pub struct Voyager {
    pub node: Node,
    /// This chain's domain.
    pub domain: u32,
    /// The origin domain whose ISM this route advances.
    pub origin: u32,
}

impl Voyager {
    async fn process(&self, id: &str, message: &[u8], metadata: &[u8]) -> Result<Outcome> {
        let submitted = self
            .node
            .post(
                "/bridge/process",
                &json!({
                    "message": hex::encode(message),
                    "metadata": hex::encode(metadata),
                }),
            )
            .await?;
        let tx = submitted["tx_hashes"][0]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let deadline = tokio::time::Instant::now() + LANDING_TIMEOUT;
        while tokio::time::Instant::now() < deadline {
            if self.node.delivered(id).await? {
                return Ok(Outcome::Delivered { tx });
            }
            if let Ok(outcome) = self.node.get(&format!("/bridge/txs/{tx}")).await {
                if outcome["status"] == "rejected" {
                    return Ok(Outcome::Refused {
                        tx: Some(tx),
                        reason: outcome["reason"].as_str().unwrap_or("refused").to_string(),
                    });
                }
            }
            tokio::time::sleep(POLL).await;
        }
        anyhow::bail!("message {id} was accepted but not delivered within {LANDING_TIMEOUT:?}")
    }
}

#[async_trait]
impl Destination for Voyager {
    async fn state(&self) -> Result<IsmState> {
        self.node.ism(self.origin).await
    }

    async fn submit(&self, batch: &Batch) -> Result<Submitted> {
        let state = self.node.ism(self.origin).await?.encode();
        let mut attestation_tx = None;
        if !batch.advanced(&state)? {
            let quote = hex::decode(batch.quote.trim_start_matches("0x"))?;
            let collateral = dcap_qvl::collateral::CollateralClient::from_env()?
                .fetch(&quote)
                .await
                .context("fetching Intel collateral for the quote")?;
            let submitted = self
                .node
                .post(
                    "/bridge/attest",
                    &json!({
                        "payload": hex::encode(&batch.payload),
                        "quote": batch.quote,
                        "event_log": batch.event_log,
                        "collateral": collateral,
                    }),
                )
                .await?;
            let tx = submitted["tx_hashes"][0]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let deadline = tokio::time::Instant::now() + LANDING_TIMEOUT;
            loop {
                if self.node.ism(self.origin).await?.encode() == batch.new_state() {
                    break;
                }
                if let Ok(outcome) = self.node.get(&format!("/bridge/txs/{tx}")).await {
                    anyhow::ensure!(
                        outcome["status"] != "rejected",
                        "the bridge lane refused the attestation: {}",
                        outcome["reason"]
                    );
                }
                anyhow::ensure!(
                    tokio::time::Instant::now() < deadline,
                    "the attestation was accepted but the ISM did not move within {LANDING_TIMEOUT:?}"
                );
                tokio::time::sleep(POLL).await;
            }
            attestation_tx = Some(tx);
        }
        let mut deliveries = Vec::new();
        for (id, message, metadata) in batch.for_domain(self.domain) {
            let outcome = if self.node.delivered(&id).await? {
                Outcome::AlreadyDelivered
            } else {
                debug!(id, "delivering");
                self.process(&id, message, metadata).await?
            };
            deliveries.push(Delivery {
                id,
                message: message.clone(),
                metadata: metadata.clone(),
                outcome,
            });
        }
        Ok(Submitted {
            attestation_tx,
            deliveries,
        })
    }

    async fn deliver(&self, message: &[u8], metadata: &[u8]) -> Result<Outcome> {
        let id = hex::encode(alloy_primitives::keccak256(message));
        if self.node.delivered(&id).await? {
            return Ok(Outcome::AlreadyDelivered);
        }
        self.process(&id, message, metadata).await
    }

    async fn delivered(&self, id: &str) -> Result<bool> {
        self.node.delivered(id).await
    }

    /// Delivery on voyager costs nothing: attestations and messages need no signer.
    async fn wallet(&self) -> Result<Wallet> {
        Ok(Wallet {
            address: "none (voyager delivery needs no gas)".into(),
            balance: 0,
        })
    }
}
