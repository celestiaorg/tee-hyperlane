//! Reading Celestia, so the enclave can verify it.
//!
//! Nothing gathered here is trusted. Light blocks are re-verified inside the enclave against
//! the header the ISM already trusts, and the merkle tree is re-proven against the app hash
//! that verification produces. A lying RPC yields a rejected attestation, not a wrong root.

use anyhow::{Context, Result};
use tee_node::state_proofs::{get_merkle_tree_hook_key, StoreProofOp};
use tendermint::block::Height;
use tendermint_light_client_verifier::types::LightBlock;
use tendermint_rpc::query::Query;
use tendermint_rpc::{Client, HttpClient, Order, Paging};
use tracing::debug;

/// One Hyperlane message as the origin chain recorded it, in tree-insert order.
/// Heights per `tx_search` call. Generous, because the query below is answered from the
/// indexer and returns almost nothing; the window exists only so a very slow index cannot
/// turn one catch-up into one enormous request.
const HEIGHT_WINDOW: u64 = 50_000;

/// The event a merkle tree hook emits for every leaf it inserts.
///
/// Asking the indexer for transactions carrying this, rather than for every transaction in a
/// height range, is what makes catching up cheap. It is also exact: a leaf that did not emit
/// this event is not a leaf, so unlike filtering on a message type it cannot miss one. If a
/// node turns out not to index it the query returns nothing, which would be indistinguishable
/// from a quiet range - the caller compares the result against the tree's own growth before
/// trusting it, so that case fails loudly instead.
const INSERT_EVENT: &str = "hyperlane.core.post_dispatch.v1.EventInsertedIntoTree";

/// Attempts per RPC call before a tick gives up.
const RPC_ATTEMPTS: u32 = 5;

/// Retry one public-node request, and name it if it still fails.
///
/// Every call in this file goes to a public Celestia node that answers most of the time. A
/// route catching up makes hundreds of them per tick, so "most of the time" is not good
/// enough: one refused connection used to fail the whole tick, and a route far enough behind
/// then never finished a sweep at all. Retrying here rather than at the call sites also means
/// the error that does escape says which call it was, instead of a bare "HTTP error".
async fn retrying<T, F, Fut>(what: &str, call: F) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = std::result::Result<T, tendermint_rpc::Error>>,
{
    let mut attempt = 1;
    loop {
        match call().await {
            Ok(value) => return Ok(value),
            Err(e) if attempt < RPC_ATTEMPTS => {
                tokio::time::sleep(std::time::Duration::from_millis(250 * u64::from(attempt)))
                    .await;
                debug!(call = what, attempt, error = %e, "retrying celestia rpc");
                attempt += 1;
            }
            Err(e) => return Err(e).with_context(|| what.to_string()),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DispatchedMessage {
    pub height: u64,
    pub tree_index: u32,
    pub message_id: [u8; 32],
    /// The raw Hyperlane message, needed to deliver it on the destination.
    pub message: Vec<u8>,
}

pub struct CelestiaReader {
    rpc: HttpClient,
}

impl CelestiaReader {
    pub fn new(rpc_url: &str) -> Result<Self> {
        Ok(Self {
            rpc: HttpClient::new(rpc_url).context("celestia rpc")?,
        })
    }

    pub async fn latest_height(&self) -> Result<u64> {
        let status = retrying("status", || self.rpc.status()).await?;
        Ok(status.sync_info.latest_block_height.value())
    }

    /// Assemble the light block at `height`.
    ///
    /// Carries the validator set that signed this header *and* the one expected to sign the
    /// next, because that pair is what lets a light client step forward from here.
    pub async fn light_block(&self, height: u64) -> Result<LightBlock> {
        let h = Height::try_from(height)?;
        let next_h = Height::try_from(height + 1)?;
        let commit = retrying(&format!("commit at {height}"), || self.rpc.commit(h)).await?;
        let validators = retrying(&format!("validators at {height}"), || {
            self.rpc.validators(h, Paging::All)
        })
        .await?;
        let next = retrying(&format!("validators at {}", height + 1), || {
            self.rpc.validators(next_h, Paging::All)
        })
        .await?;
        let peer_id = retrying("status", || self.rpc.status()).await?.node_info.id;
        Ok(LightBlock::new(
            commit.signed_header,
            tendermint::validator::Set::new(validators.validators, None),
            tendermint::validator::Set::new(next.validators, None),
            peer_id,
        ))
    }

    /// Prove the merkle tree hook out of application state at `height`.
    ///
    /// The app hash committing to this lives in the header at `height + 1`, which is the
    /// header the enclave verifies.
    pub async fn merkle_tree_hook_proof(
        &self,
        hook_id: [u8; 32],
        height: u64,
    ) -> Result<(Vec<u8>, Vec<StoreProofOp>)> {
        let key = get_merkle_tree_hook_key(hook_id);
        let at = Height::try_from(height)?;
        let response = retrying(
            &format!("abci_query for the merkle tree hook at {height}"),
            || {
                self.rpc.abci_query(
                    Some("/store/hyperlane/key".to_string()),
                    key.clone(),
                    Some(at),
                    true,
                )
            },
        )
        .await?;

        anyhow::ensure!(
            response.height.value() == height,
            "node answered at height {} rather than {height}",
            response.height
        );
        let ops = response
            .proof
            .context("node returned no proof; it may not have `prove` enabled")?
            .ops
            .into_iter()
            .map(|op| StoreProofOp {
                proof_type: op.field_type,
                key: op.key,
                data: op.data,
            })
            .collect();
        Ok((response.value, ops))
    }

    /// Find the messages inserted into the merkle tree between two heights.
    ///
    /// Uses the tx indexer rather than `block_results`: most public Celestia nodes run with
    /// `discard_abci_responses = true` and refuse the latter outright.
    ///
    /// Insert order matters, not dispatch order. The tree is replayed leaf by leaf, so a
    /// message hyperlane-cosmos inserted twice must be reported twice, or the replayed
    /// branch will not match the chain's.
    pub async fn dispatched_messages(
        &self,
        from_height: u64,
        to_height: u64,
        hook_id: [u8; 32],
    ) -> Result<Vec<DispatchedMessage>> {
        // Scanned in windows, because the height range is unbounded in practice.
        //
        // The height bounds alone match every transaction on the chain in that span, not just
        // Hyperlane's: over one route's 114,000-block backlog that was 217,070 transactions,
        // or 2,171 pages, and fourteen seconds to fetch the first of them. Windowing made each
        // request finish but left the total proportional to the chain's whole traffic, so a
        // route far enough behind still could not catch up before the next tick.
        //
        // Constraining the query to the insert event instead asks the indexer the question we
        // actually have. The same backlog comes back as one transaction in a single call.
        let mut out = Vec::new();
        let mut start = from_height;
        while start <= to_height {
            let end = (start + HEIGHT_WINDOW - 1).min(to_height);
            let query: Query =
                format!("tx.height >= {start} AND tx.height <= {end} AND {INSERT_EVENT}.index EXISTS")
                    .parse()?;
            let mut page = 1u32;
            loop {
                // Retried per window rather than per sweep. Catching up across days is over a
                // hundred requests to a public node, and one flaky answer used to discard the
                // whole sweep and start again next tick - which, for a route far enough
                // behind, means it never finishes at all.
                let results = retrying(&format!("tx_search over {start}..={end}"), || {
                    self.rpc
                        .tx_search(query.clone(), false, page, 100, Order::Ascending)
                })
                .await?;
                if results.txs.is_empty() {
                    break;
                }
                for tx in &results.txs {
                    out.extend(inserts_in(tx.height.value(), &tx.tx_result.events, hook_id)?);
                }
                if results.txs.len() < 100 {
                    break;
                }
                page += 1;
            }
            start = end + 1;
        }
        out.sort_by_key(|m| m.tree_index);
        Ok(out)
    }
}

/// Pair each tree insertion with the dispatch it came from, matching on message id.
/// Exposed so the hook filter can be tested against a block carrying two trees' insertions,
/// which is the shape of the failure that motivated it.
pub fn inserts_for_test(
    height: u64,
    events: &[tendermint::abci::Event],
    hook_id: [u8; 32],
) -> Result<Vec<DispatchedMessage>> {
    inserts_in(height, events, hook_id)
}

fn inserts_in(
    height: u64,
    events: &[tendermint::abci::Event],
    hook_id: [u8; 32],
) -> Result<Vec<DispatchedMessage>> {
    let mut dispatched: Vec<Vec<u8>> = Vec::new();
    let mut inserts: Vec<(u32, [u8; 32])> = Vec::new();

    for ev in events {
        match ev.kind.as_str() {
            "hyperlane.core.v1.EventDispatch" => {
                if let Some(raw) = attribute(ev, "message") {
                    dispatched.push(decode_hex(&raw)?);
                }
            }
            "hyperlane.core.post_dispatch.v1.EventInsertedIntoTree" => {
                // Only this route's tree. A chain can carry more than one merkle tree hook,
                // and the indexed query asks for the event rather than for the hook, so
                // another deployment's insertions arrive here too. Counting them made a
                // route report seventy-nine leaves where its own tree had grown by one.
                let into = attribute(ev, "merkle_tree_hook_id")
                    .map(|v| decode_hex(&v))
                    .transpose()?;
                if into.is_some_and(|id| id.as_slice() != hook_id.as_slice()) {
                    continue;
                }
                let index: u32 = attribute(ev, "index")
                    .context("insert event without index")?
                    .parse()?;
                let id =
                    decode_hex(&attribute(ev, "message_id").context("insert event without id")?)?;
                inserts.push((
                    index,
                    id.as_slice().try_into().context("message id length")?,
                ));
            }
            _ => {}
        }
    }

    inserts
        .into_iter()
        .map(|(tree_index, message_id)| {
            let message = dispatched
                .iter()
                .find(|m| {
                    hyperlane_types::decode_hyperlane_message(m)
                        .map(|d| hyperlane_types::get_message_id(&d) == message_id)
                        .unwrap_or(false)
                })
                .cloned()
                .unwrap_or_default();
            DispatchedMessage {
                height,
                tree_index,
                message_id,
                message,
            }
        })
        .map(Ok)
        .collect()
}

fn attribute(ev: &tendermint::abci::Event, key: &str) -> Option<String> {
    ev.attributes.iter().find_map(|a| {
        (a.key_str().ok()? == key)
            .then(|| a.value_str().ok().map(|v| v.trim_matches('"').to_string()))?
    })
}

fn decode_hex(s: &str) -> Result<Vec<u8>> {
    Ok(hex::decode(s.trim_start_matches("0x"))?)
}
