//! Merkle branches for the messages of one step, built once per step.
//!
//! `hyperlane_types::branch_proof` derives one leaf's siblings from the frontier and the new
//! leaves by recomputing every subtree it touches, which is fine for one proof and quadratic
//! for a block's worth. An origin's tree is shared by every bridge on it, so a step may carry
//! thousands of leaves that are not ours; this builder remembers every subtree it hashes, so a
//! step costs one pass over its leaves however many proofs it yields.

use std::collections::HashMap;

use hyperlane_types::{keccak_pair, zero_hashes, MerkleTree, ProofError, TREE_DEPTH};

pub struct ProofBuilder<'a> {
    frontier: &'a MerkleTree,
    leaves: &'a [[u8; 32]],
    zeros: [[u8; 32]; TREE_DEPTH],
    /// Leaves in the tree once `leaves` are in.
    n: u64,
    memo: HashMap<(usize, u64), [u8; 32]>,
}

impl<'a> ProofBuilder<'a> {
    /// The tree `frontier` becomes once `leaves` are inserted after it.
    pub fn new(frontier: &'a MerkleTree, leaves: &'a [[u8; 32]]) -> Result<Self, ProofError> {
        let n = frontier.count as u64 + leaves.len() as u64;
        if n > hyperlane_types::merkle::MAX_LEAVES as u64 {
            return Err(ProofError::TreeFull);
        }
        Ok(Self {
            frontier,
            leaves,
            zeros: zero_hashes(),
            n,
            memo: HashMap::new(),
        })
    }

    /// The 32 siblings of the leaf at `index`, which must be one of `leaves`. Same result as
    /// `hyperlane_types::branch_proof`, same errors.
    pub fn proof(&mut self, index: u32) -> Result<[[u8; 32]; TREE_DEPTH], ProofError> {
        let c = self.frontier.count;
        if index < c {
            return Err(ProofError::BeforeFrontier { index, count: c });
        }
        if index as u64 >= self.n {
            return Err(ProofError::PastTree {
                index,
                count: self.n,
            });
        }
        let mut proof = [[0u8; 32]; TREE_DEPTH];
        for (level, sibling) in proof.iter_mut().enumerate() {
            *sibling = self.node(level, ((index as u64) >> level) ^ 1);
        }
        Ok(proof)
    }

    fn node(&mut self, level: usize, pos: u64) -> [u8; 32] {
        let c = self.frontier.count as u64;
        let start = pos << level;
        if start >= self.n {
            return self.zeros[level];
        }
        if start + (1u64 << level) <= c {
            // Wholly before the frontier, so a frontier node: see `branch_proof`.
            return self.frontier.branch[level];
        }
        if level == 0 {
            return self.leaves[(pos - c) as usize];
        }
        if let Some(h) = self.memo.get(&(level, pos)) {
            return *h;
        }
        let h = keccak_pair(
            &self.node(level - 1, 2 * pos),
            &self.node(level - 1, 2 * pos + 1),
        );
        self.memo.insert((level, pos), h);
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyperlane_types::{branch_proof, get_branch_root, get_tree_root, insert_leaf, keccak256};

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

    #[test]
    fn agrees_with_the_one_shot_builder_everywhere() {
        let all: Vec<[u8; 32]> = (0..40).map(leaf).collect();
        for n in 1..=all.len() {
            for c in 0..n {
                let frontier = tree_of(&all[..c]);
                let mut builder = ProofBuilder::new(&frontier, &all[c..n]).unwrap();
                for index in c..n {
                    assert_eq!(
                        builder.proof(index as u32).unwrap(),
                        branch_proof(&frontier, &all[c..n], index as u32).unwrap(),
                        "n={n} c={c} index={index}"
                    );
                }
                assert_eq!(
                    builder.proof(c.saturating_sub(1) as u32).is_err(),
                    c > 0,
                    "before the frontier"
                );
                assert!(builder.proof(n as u32).is_err(), "past the tree");
            }
        }
    }

    /// A block's worth of leaves, every one proven: linear, not quadratic.
    #[test]
    fn a_large_step_is_proven_in_one_pass() {
        let frontier = tree_of(&(0..1000).map(leaf).collect::<Vec<_>>());
        let new: Vec<[u8; 32]> = (1000..21000).map(leaf).collect();
        let mut whole = frontier;
        for l in &new {
            insert_leaf(&mut whole, *l).unwrap();
        }
        let root = get_tree_root(&whole);
        let started = std::time::Instant::now();
        let mut builder = ProofBuilder::new(&frontier, &new).unwrap();
        for (i, l) in new.iter().enumerate() {
            let index = 1000 + i as u32;
            assert_eq!(
                get_branch_root(*l, &builder.proof(index).unwrap(), index),
                root
            );
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "20k proofs took {:?}",
            started.elapsed()
        );
    }
}
