//! What voyager's bridge lane signs and names, written once for the lane that signs it and the
//! enclave that verifies it.
//!
//! Voyager has no EVM state and no contract. Its bridge lane keeps one incremental tree and
//! signs, for every lane-bearing slot, [`header_root`] with the sequencer's ed25519 key. That
//! root is the ISM's state root, so the tree proof is the tree itself: its count and branch,
//! rehashed under the root.

use crate::{keccak256, MerkleTree, TREE_DEPTH};

/// Domain separation for the signed root, so the sequencer key's signature over a header can
/// never be read as one over anything else.
pub const HEADER_CONTEXT: &[u8] = b"voyager-bridge header v1";

/// Voyager's one tree, named as the ISM pins it: ASCII `voyager/hyperlane/merkle-tree`,
/// left-padded. Voyager has no contract address to name it by.
pub const MERKLE_TREE_ADDRESS: [u8; 32] = {
    let name = b"voyager/hyperlane/merkle-tree";
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < name.len() {
        out[32 - name.len() + i] = name[i];
        i += 1;
    }
    out
};

/// What a signed header commits to: `keccak(count(4) branch(32 * 32))`. The whole branch
/// rather than the root, so the relayer can build each new message's merkle branch from
/// exactly this tree and the leaves after it.
pub fn tree_commitment(tree: &MerkleTree) -> [u8; 32] {
    let mut buf = Vec::with_capacity(4 + 32 * TREE_DEPTH);
    buf.extend_from_slice(&tree.count.to_be_bytes());
    for node in &tree.branch {
        buf.extend_from_slice(node);
    }
    keccak256(&buf)
}

/// The root a header commits to:
/// `keccak(context domain(4) height(8) timestamp(8) tree_commitment(32))`.
///
/// Height is in it so every slot's root differs, which is what lets an ISM advance on a slot
/// that dispatched nothing.
pub fn header_root(
    domain: u32,
    height: u64,
    timestamp: u64,
    tree_commitment: &[u8; 32],
) -> [u8; 32] {
    let mut buf = Vec::with_capacity(HEADER_CONTEXT.len() + 4 + 8 + 8 + 32);
    buf.extend_from_slice(HEADER_CONTEXT);
    buf.extend_from_slice(&domain.to_be_bytes());
    buf.extend_from_slice(&height.to_be_bytes());
    buf.extend_from_slice(&timestamp.to_be_bytes());
    buf.extend_from_slice(tree_commitment);
    keccak256(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::insert_leaf;

    #[test]
    fn the_tree_address_is_the_padded_name() {
        assert_eq!(
            hex::encode(MERKLE_TREE_ADDRESS),
            "000000766f79616765722f68797065726c616e652f6d65726b6c652d74726565"
        );
    }

    /// The vector every side of the bridge is held to.
    #[test]
    fn the_root_vector() {
        let mut tree = MerkleTree::default();
        insert_leaf(&mut tree, [7; 32]).unwrap();
        assert_eq!(
            hex::encode(header_root(
                1_987_016_033,
                42,
                1_700_000_000,
                &tree_commitment(&tree)
            )),
            "5cc995bfe94535116124d67e3de04abc1c1d2334e9f7790b5258d4abcb74e00a"
        );
    }
}
