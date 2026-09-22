//! Attesting an evolve chain: Eden, whose headers live in Celestia blobs.
//!
//! The shape mirrors `ethereum_l2`, with Celestia where Ethereum would be. What differs is
//! where the root comes from. An optimistic rollup writes a commitment into L1 *storage*, so
//! the root is an MPT proof away. An evolve chain writes a signed header into a Celestia
//! *blob*, so the root is a namespace proof plus a signature away.

use anyhow::{Context, Result};
use celestia_types::nmt::Namespace;
use tracing::{debug, info, warn};

use super::evm_tree_input;
use crate::celestia::CelestiaReader;
use crate::celestia_da::DaClient;
use crate::enclave::EnclaveClient;
use crate::ethereum::ExecutionReader;

/// How far back to look for the Celestia light block that reproduces the ISM's commitment,
/// when the recorded height is missing. A route only needs this after losing its state
/// directory; the recorded height covers every ordinary tick.
const MAX_STORE_SEARCH: u64 = 400;

/// How many refused header requests in a row mean the endpoint is down rather than pruned.
const UNREACHABLE_AFTER: u32 = 5;

/// How far back to look for a Celestia block carrying Eden. Eden posts about once a minute,
/// so this is slack for a sequencer that has paused: about two hours at six second blocks.
const MAX_DA_WALK: u64 = 1200;

/// Celestia heights already known to carry nothing of Eden's, so a paused sequencer costs one
/// walk rather than one per tick. In memory only: losing it costs one slow tick.
static EMPTY_DA_HEIGHTS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<u64>>> =
    std::sync::LazyLock::new(Default::default);

/// How many captured proofs to keep. Ten blocks a second and a minute of DA lag, so a few
/// hundred is minutes of history and a few megabytes.
const TREE_CACHE_KEEP: usize = 400;

/// How many candidate heights to weigh per tick. One Celestia block carries hundreds of Eden
/// headers and each candidate costs a bisection, so bound the work; the next tick retries.
const MAX_TARGET_TRIES: usize = 8;

/// How many times to re-check DA while bootstrapping, fifteen seconds apart. Eden posts
/// about once a minute, so this is a few minutes of patience.
const BOOTSTRAP_DA_TRIES: usize = 20;

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
    let mut consecutive_failures = 0u32;
    for back in 0..MAX_STORE_SEARCH {
        let h = head.saturating_sub(back);
        let block = match reader.light_block(h).await {
            Ok(block) => {
                consecutive_failures = 0;
                block
            }
            Err(e) => {
                // Four hundred sequential requests to a dead endpoint take ten minutes and
                // report nothing. A run of failures means the endpoint is down, not that the
                // blocks are pruned, so say so instead of finishing the walk.
                consecutive_failures += 1;
                if consecutive_failures >= UNREACHABLE_AFTER {
                    anyhow::bail!(
                        "the celestia endpoint answered none of the last \
                         {UNREACHABLE_AFTER} header requests: {e}"
                    );
                }
                continue;
            }
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


/// Where captured tree proofs live, keyed by the Eden height they were taken at.
fn tree_dir(out: Option<&str>) -> Option<std::path::PathBuf> {
    let dir = std::path::Path::new(out?).parent()?.parent()?.join("trees");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Keep a proof so it can be used once DA catches up to its height.
///
/// Eden's node answers `eth_getProof` for `latest` alone, and the height the bridge wants to
/// attest is always about a minute old because that is the DA posting lag. So proofs are
/// taken now and spent later. An MPT proof is a static object; it does not go stale, it only
/// has to be captured while the node can still produce it.
fn cache_tree(
    out: Option<&str>,
    height: u64,
    proof: &tee_node::hyperlane_state::EvmTreeProof,
    pinned: u64,
) {
    let Some(dir) = tree_dir(out) else { return };
    if let Ok(bytes) = serde_json::to_vec(proof) {
        let _ = std::fs::write(dir.join(format!("{height}.json")), bytes);
    }
    // Ten blocks a second would grow this without bound, so keep the newest few hundred.
    // `pinned` is the ISM's trusted height, whose proof every attestation reads as the
    // snapshot; it is the oldest one still in use, so exempt it from eviction.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut heights: Vec<u64> = entries
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().to_str()?.strip_suffix(".json")?.parse().ok())
            .filter(|h| *h != pinned)
            .collect();
        heights.sort_unstable();
        let keep = heights.len().saturating_sub(TREE_CACHE_KEEP);
        for h in &heights[..keep] {
            let _ = std::fs::remove_file(dir.join(format!("{h}.json")));
        }
    }
}

fn cached_tree(
    out: Option<&str>,
    height: u64,
) -> Option<tee_node::hyperlane_state::EvmTreeProof> {
    let dir = tree_dir(out)?;
    let bytes = std::fs::read(dir.join(format!("{height}.json"))).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn cached_heights(out: Option<&str>) -> Vec<u64> {
    let Some(dir) = tree_dir(out) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut h: Vec<u64> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".json")?.parse().ok())
        .collect();
    h.sort_unstable();
    h
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

    // Capture a proof at whatever block is current, before looking at DA at all. Eden's node
    // serves eth_getProof for `latest` only, so the tree has to be proven ahead of the
    // header that will eventually justify attesting it.
    let eden = ExecutionReader::new(eden_rpc).with_logs_rpc(logs_rpc);
    let hook: alloy_primitives::Address = merkle_tree_hook.parse()?;
    match eden.merkle_tree_proof_at_latest(hook, base_slot).await {
        Ok((h, proof)) => {
            debug!(height = h, "captured an eden tree proof");
            cache_tree(out.as_deref(), h, &proof, trusted.height);
        }
        Err(e) => warn!(error = %e, "could not capture an eden tree proof this tick"),
    }

    // The snapshot has to be a proof we already hold: the trusted height is minutes old and
    // nothing can prove it now.
    let snapshot_proof = cached_tree(out.as_deref(), trusted.height).ok_or_else(|| {
        anyhow::anyhow!(
            "no captured tree proof at the trusted height {}; the route needs re-bootstrapping",
            trusted.height
        )
    })?;

    // Now find a height we hold a proof for that DA has caught up to. Newest first, since
    // that moves the ISM furthest in one batch.
    let usable: Vec<u64> = cached_heights(out.as_deref())
        .into_iter()
        .filter(|h| *h > trusted.height)
        .collect();
    if usable.is_empty() {
        anyhow::bail!("nothing to attest; no captured proof past the trusted height");
    }

    let mut chosen = None;
    let mut scanned = 0u32;
    for h in (target.saturating_sub(MAX_DA_WALK)..=target).rev() {
        if EMPTY_DA_HEIGHTS.lock().is_ok_and(|seen| seen.contains(&h)) {
            continue;
        }
        let Ok(data) = da.namespace_data(h, &ns).await else {
            continue;
        };
        scanned += 1;
        if !data.rows().iter().any(|r| !r.shares.is_empty()) {
            if let Ok(mut seen) = EMPTY_DA_HEIGHTS.lock() {
                seen.insert(h);
            }
            continue;
        }
        let mut carried: Vec<_> = tee_node::origins::celestia_l2::signed_headers(
            &data,
            &tee_node::origins::celestia_l2::EvolveChain::EDEN,
        )
        .into_iter()
        .filter(|hd| usable.contains(&hd.height))
        .collect();
        if carried.is_empty() {
            continue;
        }
        carried.sort_by_key(|hd| std::cmp::Reverse(hd.height));
        chosen = Some((h, data, carried));
        break;
    }
    if scanned > 0 {
        debug!(scanned, from = target, "walked celestia for eden posts");
    }
    let (target, data, candidates) = chosen.ok_or_else(|| {
        // Deliberately not "nothing to attest", which the loop treats as an idle tick and
        // logs nothing for. Two hours of silence is an outage worth surfacing.
        anyhow::anyhow!(
            "no celestia block in the last {MAX_DA_WALK} carries an eden header at a height \
             we hold a proof for (holding {}..{}); eden may have stopped posting to celestia",
            usable.first().copied().unwrap_or_default(),
            usable.last().copied().unwrap_or_default()
        )
    })?;
    // The furthest height whose re-execution fits in one attestation, so a burst of traffic
    // makes the route step through the backlog rather than refuse to move.
    let mut picked = None;
    for candidate in candidates.iter().take(MAX_TARGET_TRIES) {
        match eden
            .state_changing_blocks(trusted.height, candidate.height)
            .await
        {
            Ok(changed) if !changed.is_empty() => {
                picked = Some((*candidate, changed));
                break;
            }
            Ok(_) => continue,
            Err(e) => debug!(height = candidate.height, error = %e, "cannot reach this height"),
        }
    }
    let (header, changed) = picked.ok_or_else(|| {
        anyhow::anyhow!(
            "nothing to attest; nothing between the trusted height {} and {} changed eden's \
             state, or the span is too long to re-execute in one step",
            trusted.height,
            candidates.first().map(|c| c.height).unwrap_or_default()
        )
    })?;
    super::record_attestable_head(out.as_deref(), header.height);
    info!(celestia = target, eden = header.height, "attesting eden");

    let tree_proof = cached_tree(out.as_deref(), header.height)
        .context("the chosen height lost its captured proof")?;

    let mailbox_address: alloy_primitives::Address = mailbox.parse()?;

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

    // The blocks the enclave re-executes to reach the signed root from the trusted state.
    // Only state-changing ones: executions chain by state root, and anything left out is
    // caught by the final root not matching.
    let mut chain = Vec::with_capacity(changed.len());
    for number in &changed {
        chain.push(
            eden.block_for_execution(*number)
                .await
                .with_context(|| format!("preparing eden block {number} for re-execution"))?,
        );
    }
    info!(
        blocks = chain.len(),
        from = trusted.height,
        to = header.height,
        "re-executing eden"
    );

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
            "proof": {
                "dah": da.dah(target).await?,
                "data": data,
                "chain": chain,
                "target_height": header.height,
            },
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

/// Anchor an Eden ISM to a Celestia block that carries Eden, and the header inside it.
///
/// Two heights are in play and they are not interchangeable. The ISM's `height` is Eden's,
/// because that is what the Hyperlane tree is proven against. Its `lc_store_commit` is
/// Celestia's, because that is the light client the enclave actually runs. The Celestia
/// height is written beside the route so the next tick is a lookup rather than a walk.
#[allow(clippy::too_many_arguments)]
pub async fn bootstrap_eden(
    celestia_rpc: &str,
    da_rpc: &str,
    eden_rpc: &str,
    merkle_tree_hook: &str,
    base_slot: u64,
    lag: u64,
    height: Option<u64>,
    identity_digest: &str,
    out: Option<String>,
) -> Result<()> {
    use tee_node::origins::celestia::{commit_celestia_store, CelestiaStore};
    use tee_node::origins::celestia_l2::{signed_headers, EvolveChain};

    let reader = CelestiaReader::new(celestia_rpc)?;
    let da = DaClient::new(da_rpc);
    let ns = eden_namespace();

    // Anchor at a height we can actually prove. Eden's node serves `eth_getProof` for
    // `latest` only, so the ISM has to start from a height whose proof we hold; otherwise the
    // first attestation would have no snapshot to replay onto and the route would be dead on
    // arrival.
    let eden = ExecutionReader::new(eden_rpc);
    let hook: alloy_primitives::Address = merkle_tree_hook.parse()?;
    let (anchor_height, proof) = eden
        .merkle_tree_proof_at_latest(hook, base_slot)
        .await
        .context("capturing a tree proof at eden's latest block")?;
    // Pinned to itself: this proof is about to become the ISM's trusted height, so it is the
    // one that must outlive every later capture.
    cache_tree(out.as_deref(), anchor_height, &proof, anchor_height);
    info!(eden = anchor_height, "captured the anchor tree proof; waiting for DA");

    // Then wait for the sequencer to publish that height. Every Eden block gets a signed
    // header, contiguously, so this is a matter of the posting interval rather than luck.
    let mut found = None;
    for _ in 0..BOOTSTRAP_DA_TRIES {
        let head = match height {
            Some(h) => h,
            None => da.head().await?.saturating_sub(lag),
        };
        for h in (head.saturating_sub(MAX_DA_WALK)..=head).rev() {
            let Ok(data) = da.namespace_data(h, &ns).await else {
                continue;
            };
            if let Some(hd) = signed_headers(&data, &EvolveChain::EDEN)
                .into_iter()
                .find(|hd| hd.height == anchor_height)
            {
                found = Some((h, hd));
                break;
            }
        }
        if found.is_some() || height.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(15)).await;
    }
    let (celestia_height, header) = found.ok_or_else(|| {
        anyhow::anyhow!("DA did not publish eden height {anchor_height} in time")
    })?;

    let trusted = reader.light_block(celestia_height).await?;
    let store = CelestiaStore { trusted };

    let digest = hex::decode(identity_digest.trim_start_matches("0x"))?;
    let state = tee_attestation::IsmState {
        state_root: header.state_root,
        origin_domain: EvolveChain::EDEN.domain,
        height: header.height,
        timestamp: header.time_ns / 1_000_000_000,
        lc_store_commit: commit_celestia_store(&store),
        identity_digest: digest
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("identity digest must be 32 bytes"))?,
    };

    super::record_da_height(out.as_deref(), celestia_height);

    println!("celestia height  {celestia_height}");
    println!("eden height      {}", state.height);
    println!("timestamp        {}", state.timestamp);
    println!("state root       0x{}", hex::encode(state.state_root));
    println!("lc store commit  0x{}", hex::encode(state.lc_store_commit));
    println!();
    println!(
        "genesis state    0x{}",
        hex::encode(tee_attestation::encode_ism_state(&state))
    );
    Ok(())
}
