//! Attesting an evolve chain: Eden, whose headers live in Celestia blobs.
//!
//! The shape mirrors `ethereum_l2`, with Celestia where Ethereum would be. What differs is
//! where the root comes from. An optimistic rollup writes a commitment into L1 *storage*, so
//! the root is an MPT proof away. An evolve chain writes a signed header into a Celestia
//! *blob*, so the root is a namespace proof plus a signature away.

use anyhow::{Context, Result};
use celestia_types::nmt::Namespace;
use tracing::{debug, info};

use super::evm_tree_input;
use crate::celestia::CelestiaReader;
use crate::celestia_da::DaClient;
use crate::enclave::EnclaveClient;
use crate::ethereum::ExecutionReader;

/// How far back to look for the Celestia light block that reproduces the ISM's commitment,
/// when the recorded height is missing. A route only needs this after losing its state
/// directory; the recorded height covers every ordinary tick.
const MAX_STORE_SEARCH: u64 = 400;

fn eden_namespace() -> Namespace {
    let ns = tee_node::origins::celestia_l2::EvolveChain::EDEN.namespace;
    Namespace::new_v0(&ns[18..]).expect("pinned namespace")
}

/// The Celestia light block whose store hashes to what the ISM committed to.
///
/// Eden's ISM records *Eden's* height in its state, so unlike a Celestia-origin route there
/// is nothing in the state that says which Celestia header the store was at. The coprocessor
/// remembers it instead, and falls back to a bounded walk when that memory is gone.
async fn mocha_store_at(
    reader: &CelestiaReader,
    want_commit: [u8; 32],
    recorded: Option<u64>,
    head: u64,
) -> Result<(u64, tendermint_light_client_verifier::types::LightBlock)> {
    use tee_node::origins::celestia::{commit_celestia_store, CelestiaStore};

    if let Some(h) = recorded {
        if let Ok(block) = reader.light_block(h).await {
            let store = CelestiaStore {
                trusted: block.clone(),
            };
            if commit_celestia_store(&store) == want_commit {
                return Ok((h, block));
            }
            debug!(height = h, "recorded celestia height no longer reproduces the store");
        }
    }
    for back in 0..MAX_STORE_SEARCH {
        let h = head.saturating_sub(back);
        let Ok(block) = reader.light_block(h).await else {
            continue;
        };
        let store = CelestiaStore {
            trusted: block.clone(),
        };
        if commit_celestia_store(&store) == want_commit {
            return Ok((h, block));
        }
    }
    anyhow::bail!(
        "no celestia header in the last {MAX_STORE_SEARCH} reproduces this ISM's light-client \
         store; the route needs re-bootstrapping"
    )
}

/// Pick the newest Eden header in a Celestia block that advances past the trusted height.
fn newest_header(
    blobs: &[celestia_types::Blob],
    above: u64,
) -> Option<(usize, tee_node::origins::celestia_l2::EvolveHeader)> {
    use tee_node::origins::celestia_l2::{decode_evolve_header, EvolveChain};
    let mut best: Option<(usize, _)> = None;
    for (i, blob) in blobs.iter().enumerate() {
        let Ok(header) = decode_evolve_header(&blob.data, EvolveChain::EDEN.chain_id) else {
            continue;
        };
        if header.height <= above {
            continue;
        }
        if best.as_ref().map(|(_, b): &(usize, tee_node::origins::celestia_l2::EvolveHeader)| header.height > b.height).unwrap_or(true) {
            best = Some((i, header));
        }
    }
    best
}

#[allow(clippy::too_many_arguments)]
pub async fn attest_eden(
    celestia_rpc: &str,
    da_rpc: &str,
    eden_rpc: &str,
    logs_rpc: Option<&str>,
    enclave_url: &str,
    trusted_state_hex: &str,
    destination_domain: u32,
    routers: &[String],
    mailbox: &str,
    merkle_tree_hook: &str,
    base_slot: u64,
    lag: u64,
    out: Option<String>,
) -> Result<()> {
    let trusted_raw = hex::decode(trusted_state_hex.trim_start_matches("0x"))?;
    let trusted = tee_attestation::decode_ism_state(&trusted_raw)?;

    let reader = CelestiaReader::new(celestia_rpc)?;
    let da = DaClient::new(da_rpc);
    let ns = eden_namespace();

    // The DA node is the limit, not the consensus head: a blob is only readable once the node
    // has sampled the block it is in.
    let da_head = da.head().await.context("celestia DA node head")?;
    let target = da_head.saturating_sub(lag);

    let (store_height, trusted_block) = mocha_store_at(
        &reader,
        trusted.lc_store_commit,
        super::recorded_da_height(out.as_deref()),
        target,
    )
    .await?;
    anyhow::ensure!(
        target > store_height,
        "celestia has not advanced past the store ({target} vs {store_height})"
    );

    let blobs = da.blobs(target, &ns).await?;
    if blobs.is_empty() {
        super::record_attestable_head(out.as_deref(), trusted.height);
        anyhow::bail!("nothing to attest; no eden blob in celestia block {target}");
    }
    let (index, header) = newest_header(&blobs, trusted.height).ok_or_else(|| {
        anyhow::anyhow!("nothing to attest; no eden header past {} in this block", trusted.height)
    })?;
    super::record_attestable_head(out.as_deref(), header.height);
    info!(
        celestia = target,
        eden = header.height,
        blobs = blobs.len(),
        "attesting eden"
    );

    let blob = blobs[index].clone();
    let proofs = da
        .proof(target, &ns, blob.commitment.hash())
        .await
        .context("blob.GetProof from the DA node")?;

    // The Hyperlane tree, proven under the state root the sequencer signed for that height.
    let eden = ExecutionReader::new(eden_rpc).with_logs_rpc(logs_rpc);
    let hook: alloy_primitives::Address = merkle_tree_hook.parse()?;
    let mailbox_address: alloy_primitives::Address = mailbox.parse()?;

    let tree_proof = eden.merkle_tree_proof(hook, base_slot, header.height).await?;
    let snapshot_proof = eden
        .merkle_tree_proof(hook, base_slot, trusted.height)
        .await
        .context("reading the merkle tree at the trusted height; eden must serve archive state")?;

    let dispatched = eden
        .dispatched_messages(mailbox_address, hook, trusted.height + 1, header.height)
        .await?;
    super::check_scan(
        super::claimed_count(&snapshot_proof)?,
        super::claimed_count(&tree_proof)?,
        dispatched.len(),
    )?;
    anyhow::ensure!(!dispatched.is_empty(), "nothing to attest");
    let ours: Vec<Vec<u8>> = dispatched.iter().map(|d| d.message.clone()).collect();
    if !super::any_for_route(&ours, destination_domain, routers)
        && !super::heartbeat_due(out.as_deref())
    {
        anyhow::bail!("nothing to attest; no messages for our routes on domain {destination_domain}");
    }

    let mut tree_address = [0u8; 32];
    tree_address[12..].copy_from_slice(hook.as_slice());

    let request = serde_json::json!({
        "protocol": tee_node::attest::PROTOCOL_VERSION,
        "trusted_state": hex::encode(&trusted_raw),
        "origin": {
            "chain": "eden",
            "celestia": {
                "chain": "celestia",
                "store": { "trusted": trusted_block },
                "updates": [reader.light_block(target).await?],
            },
            "proof": { "dah": da.dah(target).await?, "blob": blob, "proofs": proofs },
        },
        "tree": evm_tree_input(&tree_proof)?,
        "tree_snapshot": evm_tree_input(&snapshot_proof)?,
        "message_ids": dispatched.iter().map(|d| d.message_id).collect::<Vec<_>>(),
        "merkle_tree_address": tree_address,
    });

    let attestation = EnclaveClient::new(enclave_url).attest(&request).await?;
    info!(messages = attestation.message_ids.len(), "enclave attested");
    super::record_advanced(out.as_deref());
    // Which Celestia header the ISM is about to commit to, so the next tick is a lookup
    // rather than a walk back through Celestia headers.
    super::record_da_height(out.as_deref(), target);

    if let Some(path) = out {
        let record = serde_json::json!({
            "attestation": {
                "quote": attestation.quote,
                "event_log": attestation.event_log,
                "payload": attestation.payload,
                "new_state": attestation.new_state,
            },
            "messages": dispatched.iter().map(|d| hex::encode(&d.message)).collect::<Vec<_>>(),
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&record)?)?;
        debug!(path, "wrote attestation");
    }
    Ok(())
}
