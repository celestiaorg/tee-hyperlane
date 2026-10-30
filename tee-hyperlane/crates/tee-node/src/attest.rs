//! The one thing the enclave does: verify, attest, forget.
//!
//! The same steps for every origin, which is the point of `origin::Origin`:
//!
//! 1. verify the head the request describes, from the state the ISM already trusts
//! 2. read the Hyperlane tree under the new root
//! 3. check the tree was read at the address the ISM pins
//! 4. hand back the update, carrying that tree's root and count, for dstack to sign
//!
//! Messages are not named here. Each one proves itself at delivery with a merkle branch against
//! an attested root, so an origin block of any size is one attestation and nothing the enclave
//! signs grows with traffic.
//!
//! Nothing is written to disk and nothing is kept between requests: the destination chain's
//! ISM state *is* the light client's database.

use crate::state::{AttestedUpdate, IsmState};
use serde::Deserialize;
use serde_json::Value;

use crate::origin;
use hyperlane_types::get_tree_root;

/// The request shape this enclave understands.
///
/// Bumped whenever a field is added, removed, or stops being honoured, so a caller built for
/// an older shape is refused rather than having fields silently ignored.
///
/// 2 attests the tree root: no `tree_snapshot`, no `message_ids`. 1 carried both and the enclave
/// replayed the batch.
pub const PROTOCOL_VERSION: u32 = 2;

/// How the enclave is asked to advance one ISM by one step.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttestRequest {
    /// Must equal `PROTOCOL_VERSION`.
    pub protocol: u32,
    /// The ISM's current state, read from the destination chain, hex-encoded.
    #[serde(with = "hex_ism_state")]
    pub trusted_state: IsmState,
    /// Which origin, as `origin::Chain::name`.
    pub chain: String,
    /// Whatever that origin needs to verify its head. Only the origin parses it.
    pub input: Value,
    /// Proof of the origin's Hyperlane tree at the new head.
    pub tree: Value,
    /// The origin merkle tree hook the ISM pins.
    pub merkle_tree_address: [u8; 32],
}

mod hex_ism_state {
    use crate::state::IsmState;
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<IsmState, D::Error> {
        let text = String::deserialize(d)?;
        let raw = hex::decode(text.strip_prefix("0x").unwrap_or(&text))
            .map_err(serde::de::Error::custom)?;
        IsmState::decode(&raw).map_err(serde::de::Error::custom)
    }
}

/// Verify everything in the request and produce the update to be attested.
///
/// Returns the payload as well, because the caller hashes it into `report_data`.
pub fn build_attested_update(request: AttestRequest) -> anyhow::Result<(AttestedUpdate, Vec<u8>)> {
    let chain = origin::find(&request.chain)
        .ok_or_else(|| anyhow::anyhow!("this enclave does not attest `{}`", request.chain))?;
    attest_with(chain, request)
}

/// Everything after the registry lookup, for one known chain. Separate so the pipeline can be
/// tested against a stand-in chain without light-client fixtures.
pub fn attest_with(
    chain: &origin::Chain,
    request: AttestRequest,
) -> anyhow::Result<(AttestedUpdate, Vec<u8>)> {
    anyhow::ensure!(
        request.protocol == PROTOCOL_VERSION,
        "request protocol {}, but this enclave speaks {PROTOCOL_VERSION}",
        request.protocol
    );
    let trusted = request.trusted_state;
    anyhow::ensure!(
        chain.domain == trusted.origin_domain,
        "{} is domain {}, but the ISM is for domain {}",
        chain.name,
        chain.domain,
        trusted.origin_domain
    );

    let head = chain.origin.verify(request.input, &trusted)?;

    // The tree is read under the root this update moves the ISM to, which the origin has just
    // verified. Whatever is in it is what the origin's chain says was dispatched.
    let at_head = chain.origin.merkle_tree(request.tree, head.root)?;

    // Proving a tree is not enough: anyone can deploy a hook, fill it with ids of their
    // choosing and prove it honestly under the real root. Only the address the ISM pins counts.
    anyhow::ensure!(
        at_head.address == request.merkle_tree_address,
        "tree proven at 0x{} but 0x{} was attested",
        hex::encode(at_head.address),
        hex::encode(request.merkle_tree_address)
    );

    let new_state = IsmState {
        state_root: head.root.0,
        origin_domain: trusted.origin_domain,
        height: head.height,
        timestamp: head.timestamp,
        lc_store_commit: head.store_commit,
        identity_digest: trusted.identity_digest,
    };
    let update = AttestedUpdate {
        prev_state: trusted,
        new_state,
        merkle_tree_address: request.merkle_tree_address,
        attested_at: head.attested_at,
        tree_root: get_tree_root(&at_head.tree),
        tree_count: at_head.tree.count,
    };
    let payload = update.encode();
    Ok((update, payload))
}
