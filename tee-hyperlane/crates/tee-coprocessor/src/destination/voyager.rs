//! Delivers to a voyager chain: hands its bridge lane the attestation with fresh Intel
//! collateral, as for Celestia, then each message with the merkle branch that proves it against
//! the attested root, as `TeeDcapIsm.verify` takes it.
//!
//! The node checks the quote at admission and the lane checks the transition, in a later slot.
//! Neither answers with the result, so this waits on the ISM's state and the delivered set.

use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures::{stream, StreamExt, TryStreamExt};
use serde_json::json;
use tee_node::state::IsmState;
use tracing::debug;

use super::{Batch, Delivery, Destination, Outcome, Submitted, Wallet};
use crate::origin::l2::voyager::Node;

/// How long the lane may take to execute what was submitted. Slots come every few
/// milliseconds under load and at least every header interval when idle.
const LANDING_TIMEOUT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(250);
const PER_MESSAGE: Duration = Duration::from_millis(10);
/// Requests in flight to the node at once, sending or confirming.
const CONCURRENCY: usize = 32;

pub struct Voyager {
    pub node: Node,
    /// This chain's domain.
    pub domain: u32,
    /// The origin domain whose ISM this route advances.
    pub origin: u32,
}

impl Voyager {
    /// Hands the lane one message and its proof; the lane executes it in a later slot.
    async fn send_process(&self, message: &[u8], metadata: &[u8]) -> Result<String> {
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
        Ok(submitted["tx_hashes"][0]
            .as_str()
            .unwrap_or_default()
            .to_string())
    }

    /// Where `(id, tx)` stands: delivered, refused, or not executed yet.
    async fn landed(&self, id: &str, tx: &str) -> Result<Option<Outcome>> {
        if self.node.delivered(id).await? {
            return Ok(Some(Outcome::Delivered { tx: tx.to_string() }));
        }
        if let Ok(outcome) = self.node.get(&format!("/bridge/txs/{tx}")).await {
            if outcome["status"] == "rejected" {
                return Ok(Some(Outcome::Refused {
                    tx: Some(tx.to_string()),
                    reason: outcome["reason"].as_str().unwrap_or("refused").to_string(),
                }));
            }
        }
        Ok(None)
    }

    /// Sends every message, then confirms them together, as the EVM destination does: a batch
    /// of thousands lands in a few slots instead of one poll interval each.
    async fn process_all(&self, pending: Vec<(String, Vec<u8>, Vec<u8>)>) -> Result<Vec<Delivery>> {
        let sent: Vec<(String, Vec<u8>, Vec<u8>, String)> = stream::iter(pending)
            .map(|(id, message, metadata)| async move {
                debug!(id, "delivering");
                let tx = self.send_process(&message, &metadata).await?;
                Ok::<_, anyhow::Error>((id, message, metadata, tx))
            })
            .buffered(CONCURRENCY)
            .try_collect()
            .await?;
        let mut open: Vec<_> = sent.into_iter().map(|d| (d, None)).collect();
        // Confirming is a request per message, so a large batch gets longer.
        let deadline =
            tokio::time::Instant::now() + LANDING_TIMEOUT + PER_MESSAGE * open.len() as u32;
        loop {
            let unsettled: Vec<(usize, String, String)> = open
                .iter()
                .enumerate()
                .filter(|(_, (_, done))| done.is_none())
                .map(|(i, ((id, _, _, tx), _))| (i, id.clone(), tx.clone()))
                .collect();
            let checks: Vec<(usize, Option<Outcome>)> = stream::iter(unsettled)
                .map(|(i, id, tx)| async move {
                    Ok::<_, anyhow::Error>((i, self.landed(&id, &tx).await?))
                })
                .buffered(CONCURRENCY)
                .try_collect()
                .await?;
            for (i, check) in checks {
                open[i].1 = check;
            }
            let waiting = open.iter().filter(|(_, done)| done.is_none()).count();
            if waiting == 0 {
                break;
            }
            anyhow::ensure!(
                tokio::time::Instant::now() < deadline,
                "{waiting} of {} accepted messages were still not delivered",
                open.len()
            );
            tokio::time::sleep(POLL).await;
        }
        Ok(open
            .into_iter()
            .map(|((id, message, metadata, _), outcome)| Delivery {
                id,
                message,
                metadata,
                outcome: outcome.expect("every message settled"),
            })
            .collect())
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
        let mut pending = Vec::new();
        for (id, message, metadata) in batch.for_domain(self.domain) {
            if self.node.delivered(&id).await? {
                deliveries.push(Delivery {
                    id,
                    message: message.clone(),
                    metadata: metadata.clone(),
                    outcome: Outcome::AlreadyDelivered,
                });
            } else {
                pending.push((id, message.clone(), metadata.clone()));
            }
        }
        deliveries.extend(self.process_all(pending).await?);
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
        let mut delivered = self
            .process_all(vec![(id, message.to_vec(), metadata.to_vec())])
            .await?;
        Ok(delivered.remove(0).outcome)
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
