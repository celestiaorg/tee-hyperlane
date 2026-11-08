//! Hyperlane's incremental merkle tree, as implemented by both `MerkleTreeHook.sol` and
//! hyperlane-cosmos.
//!
//! The bridge never inserts into a tree it invented: it adopts the `(branch, count)` proven
//! out of origin-chain state and replays new message ids on top. Because the branch is a
//! function of every leaf ever inserted, a snapshot that is wrong or has gaps cannot
//! reproduce the on-chain branch - which is why the snapshot needs no separate
//! authentication.

use crate::keccak256;

pub const TREE_DEPTH: usize = 32;
pub const MAX_LEAVES: u32 = u32::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MerkleTree {
    #[serde(with = "hex_branch")]
    pub branch: [[u8; 32]; TREE_DEPTH],
    pub count: u32,
}

/// Hex strings on the wire: a tree snapshot is read by humans while debugging a stuck
/// route far more often than it is read by machines.
mod hex_branch {
    use super::TREE_DEPTH;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[[u8; 32]; TREE_DEPTH], s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(v.iter().map(hex::encode))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<[[u8; 32]; TREE_DEPTH], D::Error> {
        let items = Vec::<String>::deserialize(d)?;
        if items.len() != TREE_DEPTH {
            return Err(serde::de::Error::custom(format!(
                "branch must have {TREE_DEPTH} entries, got {}",
                items.len()
            )));
        }
        let mut out = [[0u8; 32]; TREE_DEPTH];
        for (slot, text) in out.iter_mut().zip(items) {
            let raw = hex::decode(text.strip_prefix("0x").unwrap_or(&text))
                .map_err(serde::de::Error::custom)?;
            *slot = raw
                .try_into()
                .map_err(|_| serde::de::Error::custom("branch entry must be 32 bytes"))?;
        }
        Ok(out)
    }
}

impl Default for MerkleTree {
    fn default() -> Self {
        Self {
            branch: [[0u8; 32]; TREE_DEPTH],
            count: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MerkleError {
    #[error("merkle tree is full")]
    TreeFull,
}

/// `Z[0] = 0`, `Z[i] = keccak(Z[i-1] || Z[i-1])`.
pub fn zero_hashes() -> [[u8; 32]; TREE_DEPTH] {
    let mut z = [[0u8; 32]; TREE_DEPTH];
    for i in 1..TREE_DEPTH {
        z[i] = keccak_pair(&z[i - 1], &z[i - 1]);
    }
    z
}

pub fn keccak_pair(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut buf = [0u8; 64];
    buf[..32].copy_from_slice(a);
    buf[32..].copy_from_slice(b);
    keccak256(&buf)
}

pub fn insert_leaf(tree: &mut MerkleTree, leaf: [u8; 32]) -> Result<(), MerkleError> {
    if tree.count >= MAX_LEAVES {
        return Err(MerkleError::TreeFull);
    }
    tree.count += 1;
    let mut node = leaf;
    let mut size = tree.count;
    for level in 0..TREE_DEPTH {
        if size & 1 == 1 {
            tree.branch[level] = node;
            return Ok(());
        }
        node = keccak_pair(&tree.branch[level], &node);
        size /= 2;
    }
    unreachable!("count is bounded by 2^TREE_DEPTH")
}

pub fn get_tree_root(tree: &MerkleTree) -> [u8; 32] {
    let zeros = zero_hashes();
    let mut current = [0u8; 32];
    let mut index = tree.count;
    for level in 0..TREE_DEPTH {
        current = if index & 1 == 1 {
            keccak_pair(&tree.branch[level], &current)
        } else {
            keccak_pair(&current, &zeros[level])
        };
        index /= 2;
    }
    current
}

/// Fold a leaf and its 32-node proof up to a root, exactly as `MerkleLib.branchRoot` does.
pub fn get_branch_root(leaf: [u8; 32], proof: &[[u8; 32]; TREE_DEPTH], index: u32) -> [u8; 32] {
    let mut current = leaf;
    for (level, sibling) in proof.iter().enumerate() {
        let bit = (index >> level) & 1;
        current = if bit == 1 {
            keccak_pair(sibling, &current)
        } else {
            keccak_pair(&current, sibling)
        };
    }
    current
}

// ============================================================================
// Where MerkleTreeHook.sol keeps this tree in storage
// ============================================================================

/// Hyperlane's canonical Sepolia `merkleTreeHook`, confirmed on chain.
pub const SEPOLIA_MERKLE_TREE_BASE_SLOT: u64 = 103;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MerkleTreeSlots {
    pub base: u64,
}

impl MerkleTreeSlots {
    pub const fn new(base: u64) -> Self {
        Self { base }
    }

    /// The 33 slots to request, branch first then count, in the order
    /// [`build_tree_from_slots`] expects.
    pub fn storage_keys(&self) -> [[u8; 32]; TREE_DEPTH + 1] {
        let mut keys = [[0u8; 32]; TREE_DEPTH + 1];
        for (i, key) in keys.iter_mut().enumerate() {
            key[24..].copy_from_slice(&(self.base + i as u64).to_be_bytes());
        }
        keys
    }

    pub fn count_slot(&self) -> u64 {
        self.base + TREE_DEPTH as u64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LayoutError {
    #[error("count {0} does not fit in uint32; storage layout is probably wrong")]
    CountNotUint32(u128),
}

/// Assemble a tree from the 33 slot values, in the order [`MerkleTreeSlots::storage_keys`]
/// returns them.
pub fn build_tree_from_slots(
    values: &[[u8; 32]; TREE_DEPTH + 1],
) -> Result<MerkleTree, LayoutError> {
    let mut branch = [[0u8; 32]; TREE_DEPTH];
    branch.copy_from_slice(&values[..TREE_DEPTH]);

    let raw = &values[TREE_DEPTH];
    // Solidity stores count as uint256 but MerkleLib caps it at 2^32; anything above that
    // means we are reading the wrong slot.
    if raw[..28] != [0u8; 28] {
        return Err(LayoutError::CountNotUint32(u128::from_be_bytes(
            raw[16..].try_into().unwrap(),
        )));
    }
    let count = u32::from_be_bytes(raw[28..].try_into().unwrap());
    Ok(MerkleTree { branch, count })
}

// ============================================================================
// Proving a leaf against a later root
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProofError {
    #[error("leaf {index} is before the frontier at {count}; its siblings are not derivable")]
    BeforeFrontier { index: u32, count: u32 },
    #[error("leaf {index} is past the tree's {count} leaves")]
    PastTree { index: u32, count: u64 },
    #[error("merkle tree is full")]
    TreeFull,
}

/// The 32 siblings that fold `leaf` at `index` up to the root of the tree `frontier` becomes
/// after `new_leaves` are inserted, as `get_branch_root` and Hyperlane's `branchRoot` expect.
///
/// A frontier (`branch`, `count`) holds the root of every maximal complete subtree of the first
/// `count` leaves. For a leaf inserted after it, every sibling on the path to the root is one of
/// those (a left sibling wholly before the frontier is always maximal, because its parent would
/// otherwise reach past `count`), or is built from `new_leaves` and zero hashes. So the relayer
/// needs the frontier it already proves at the trusted height and the leaves it already indexes,
/// never the whole history of the tree.
pub fn branch_proof(
    frontier: &MerkleTree,
    new_leaves: &[[u8; 32]],
    index: u32,
) -> Result<[[u8; 32]; TREE_DEPTH], ProofError> {
    let c = frontier.count as u64;
    let n = c + new_leaves.len() as u64;
    if n > MAX_LEAVES as u64 {
        return Err(ProofError::TreeFull);
    }
    if (index as u64) < c {
        return Err(ProofError::BeforeFrontier {
            index,
            count: frontier.count,
        });
    }
    if index as u64 >= n {
        return Err(ProofError::PastTree { index, count: n });
    }
    let zeros = zero_hashes();
    let mut proof = [[0u8; 32]; TREE_DEPTH];
    for (level, sibling) in proof.iter_mut().enumerate() {
        let pos = ((index as u64) >> level) ^ 1;
        *sibling = node(frontier, new_leaves, &zeros, n, level, pos);
    }
    Ok(proof)
}

/// The root of the subtree at `level` covering leaves `pos * 2^level ..`, in a tree of `n` leaves
/// whose first `frontier.count` are known only through the frontier.
fn node(
    frontier: &MerkleTree,
    new_leaves: &[[u8; 32]],
    zeros: &[[u8; 32]; TREE_DEPTH],
    n: u64,
    level: usize,
    pos: u64,
) -> [u8; 32] {
    let c = frontier.count as u64;
    let start = pos << level;
    let end = start + (1u64 << level);
    if start >= n {
        return zeros[level];
    }
    if end <= c {
        // Wholly before the frontier: the frontier node at this level, which exists exactly when
        // bit `level` of the count is set and then covers the 2^level leaves ending at the
        // count's prefix. `branch_proof` only asks for leaves past the frontier, whose left
        // siblings are all of this shape.
        let prefix = c >> level;
        debug_assert!(prefix & 1 == 1 && pos == prefix - 1, "not a frontier node");
        return frontier.branch[level];
    }
    if level == 0 {
        return new_leaves[(pos - c) as usize];
    }
    keccak_pair(
        &node(frontier, new_leaves, zeros, n, level - 1, 2 * pos),
        &node(frontier, new_leaves, zeros, n, level - 1, 2 * pos + 1),
    )
}

#[cfg(test)]
mod proof_tests {
    use super::*;

    fn leaf(i: u32) -> [u8; 32] {
        keccak256(&i.to_be_bytes())
    }

    fn tree_of(leaves: &[[u8; 32]]) -> MerkleTree {
        let mut t = MerkleTree::default();
        for l in leaves {
            insert_leaf(&mut t, *l).unwrap();
        }
        t
    }

    /// Every leaf after every frontier, for trees up to 40 leaves, folds to the on-chain root.
    #[test]
    fn a_frontier_based_proof_folds_to_the_root() {
        let all: Vec<[u8; 32]> = (0..40).map(leaf).collect();
        for n in 1..=all.len() {
            let root = get_tree_root(&tree_of(&all[..n]));
            for c in 0..n {
                let frontier = tree_of(&all[..c]);
                for index in c..n {
                    let proof = branch_proof(&frontier, &all[c..n], index as u32).unwrap();
                    assert_eq!(
                        get_branch_root(all[index], &proof, index as u32),
                        root,
                        "n={n} c={c} index={index}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_leaf_outside_the_new_range_is_refused() {
        let all: Vec<[u8; 32]> = (0..6).map(leaf).collect();
        let frontier = tree_of(&all[..3]);
        assert_eq!(
            branch_proof(&frontier, &all[3..], 2),
            Err(ProofError::BeforeFrontier { index: 2, count: 3 })
        );
        assert_eq!(
            branch_proof(&frontier, &all[3..], 6),
            Err(ProofError::PastTree { index: 6, count: 6 })
        );
    }

    /// hyperlane-cosmos pre-fills unused branch levels with zero hashes; the proof must not care.
    #[test]
    fn prefilled_unused_levels_change_nothing() {
        let all: Vec<[u8; 32]> = (0..5).map(leaf).collect();
        let mut frontier = tree_of(&all[..3]);
        let zeros = zero_hashes();
        for level in 2..TREE_DEPTH {
            frontier.branch[level] = zeros[level];
        }
        let root = get_tree_root(&tree_of(&all));
        for index in 3..5 {
            let proof = branch_proof(&frontier, &all[3..], index).unwrap();
            assert_eq!(get_branch_root(all[index as usize], &proof, index), root);
        }
    }
}
