//! Voyager: a header its sequencer signed, committing to the bridge lane's Hyperlane tree.
//!
//! Voyager has no EVM state and no contract. Its bridge lane keeps one incremental tree and
//! signs, for every lane-bearing slot, `keccak(context domain height timestamp
//! keccak(count branch))` with the sequencer's ed25519 key. That root is the ISM's state
//! root, so the tree proof is the tree itself: its count and branch, rehashed under the root.
//! Like Base, the sequencer is trusted and its key pinned here; nothing is re-executed.
//!
//! The byte layout is voyager-turbo's `crates/bridge/src/header.rs`, held to it by
//! `the_root_matches_voyager`.

use crate::state::IsmState;
use alloy_primitives::{hex, B256};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use hyperlane_types::{keccak256, MerkleTree, TREE_DEPTH};
use serde_json::Value;

use crate::origin::{self, Chain, Head, Origin, Tree};

pub static VOYAGER_A: Chain = Chain {
    name: "voyager-a",
    domain: 1_987_016_033,
    origin: &Voyager {
        domain: 1_987_016_033,
        sequencer: hex!("4dc8a5dfc603cd5d6acec6c9745cb8df951ce8dae4fa243f81f14cc3aea45fc7"),
    },
};

pub static VOYAGER_B: Chain = Chain {
    name: "voyager-b",
    domain: 1_987_016_034,
    origin: &Voyager {
        domain: 1_987_016_034,
        sequencer: hex!("0378e71a1235d4ebfff1efbbda13c1a9c20bfe20889e6f83c25248208ed59423"),
    },
};

/// Domain separation for the signed root, as voyager has it.
const HEADER_CONTEXT: &[u8] = b"voyager-bridge header v1";

/// Voyager's one tree, named as the ISM pins it: ASCII `voyager/hyperlane/merkle-tree`,
/// left-padded. Voyager has no contract address to name it by.
pub const MERKLE_TREE_ADDRESS: [u8; 32] =
    hex!("000000766f79616765722f68797065726c616e652f6d65726b6c652d74726565");

/// One voyager chain: its domain, and the sequencer key that speaks for it.
pub struct Voyager {
    pub domain: u32,
    pub sequencer: [u8; 32],
}

/// A header as voyager's `GET /bridge/header/...` serves it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SignedHeader {
    pub domain: u32,
    pub height: u64,
    /// Seconds.
    pub timestamp: u64,
    #[serde(with = "hex32")]
    pub tree_commitment: [u8; 32],
    /// Recomputed, never believed; carried for the relayer's convenience.
    #[serde(with = "hex32")]
    pub root: [u8; 32],
    #[serde(with = "hex64")]
    pub signature: [u8; 64],
}

/// The tree at a height, with the header fields its root also covers.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TreeProof {
    pub height: u64,
    pub timestamp: u64,
    pub tree: MerkleTree,
}

pub fn tree_commitment(tree: &MerkleTree) -> [u8; 32] {
    let mut buf = Vec::with_capacity(4 + 32 * TREE_DEPTH);
    buf.extend_from_slice(&tree.count.to_be_bytes());
    for node in &tree.branch {
        buf.extend_from_slice(node);
    }
    keccak256(&buf)
}

pub fn header_root(
    domain: u32,
    height: u64,
    timestamp: u64,
    tree_commitment: &[u8; 32],
) -> [u8; 32] {
    let mut buf = Vec::with_capacity(HEADER_CONTEXT.len() + 52);
    buf.extend_from_slice(HEADER_CONTEXT);
    buf.extend_from_slice(&domain.to_be_bytes());
    buf.extend_from_slice(&height.to_be_bytes());
    buf.extend_from_slice(&timestamp.to_be_bytes());
    buf.extend_from_slice(tree_commitment);
    keccak256(&buf)
}

impl Origin for Voyager {
    fn verify(&self, input: Value, trusted: &IsmState) -> anyhow::Result<Head> {
        let header: SignedHeader = origin::parse("voyager header", input)?;
        anyhow::ensure!(
            header.domain == self.domain,
            "header is for domain {}, not {}",
            header.domain,
            self.domain
        );
        let root = header_root(
            self.domain,
            header.height,
            header.timestamp,
            &header.tree_commitment,
        );
        let key =
            VerifyingKey::from_bytes(&self.sequencer).expect("the pinned sequencer key is valid");
        key.verify(&root, &Signature::from_bytes(&header.signature))
            .map_err(|_| anyhow::anyhow!("the header is not signed by the pinned sequencer"))?;
        anyhow::ensure!(
            header.height > trusted.height,
            "signed header {} is not past the trusted height {}",
            header.height,
            trusted.height
        );
        Ok(Head {
            root: B256::from(root),
            height: header.height,
            timestamp: header.timestamp,
            // No light client, so nothing to commit to; carried so the state stays as it was.
            store_commit: trusted.lc_store_commit,
            attested_at: header.timestamp,
        })
    }

    fn merkle_tree(&self, proof: Value, root: B256) -> anyhow::Result<Tree> {
        let proof: TreeProof = origin::parse("voyager tree proof", proof)?;
        let rebuilt = header_root(
            self.domain,
            proof.height,
            proof.timestamp,
            &tree_commitment(&proof.tree),
        );
        anyhow::ensure!(
            rebuilt == root.0,
            "the tree does not hash to the root 0x{}",
            hex::encode(root.0)
        );
        Ok(Tree {
            address: MERKLE_TREE_ADDRESS,
            tree: proof.tree,
        })
    }
}

mod hex32 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let text = String::deserialize(d)?;
        hex::decode(text.trim_start_matches("0x"))
            .map_err(serde::de::Error::custom)?
            .try_into()
            .map_err(|_| serde::de::Error::custom("expected 32 bytes"))
    }
}

mod hex64 {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        let text = String::deserialize(d)?;
        hex::decode(text.trim_start_matches("0x"))
            .map_err(serde::de::Error::custom)?
            .try_into()
            .map_err(|_| serde::de::Error::custom("expected 64 bytes"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use hyperlane_types::insert_leaf;

    /// The same vector voyager-turbo's `header.rs` asserts.
    #[test]
    fn the_root_matches_voyager() {
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

    fn chain(key: &SigningKey) -> Voyager {
        Voyager {
            domain: 7,
            sequencer: key.verifying_key().to_bytes(),
        }
    }

    fn trusted(height: u64) -> IsmState {
        IsmState {
            state_root: [0; 32],
            origin_domain: 7,
            height,
            timestamp: 0,
            lc_store_commit: [5; 32],
            identity_digest: [0; 32],
        }
    }

    fn signed(key: &SigningKey, domain: u32, height: u64, tree: &MerkleTree) -> Value {
        let commitment = tree_commitment(tree);
        let root = header_root(domain, height, 100, &commitment);
        serde_json::to_value(SignedHeader {
            domain,
            height,
            timestamp: 100,
            tree_commitment: commitment,
            root,
            signature: key.sign(&root).to_bytes(),
        })
        .unwrap()
    }

    #[test]
    fn a_signed_header_and_its_tree_verify() {
        let key = SigningKey::from_bytes(&[1; 32]);
        let mut tree = MerkleTree::default();
        insert_leaf(&mut tree, [9; 32]).unwrap();
        let head = chain(&key)
            .verify(signed(&key, 7, 10, &tree), &trusted(9))
            .unwrap();
        assert_eq!(head.height, 10);
        assert_eq!(head.store_commit, [5; 32]);

        let proof = serde_json::to_value(TreeProof {
            height: 10,
            timestamp: 100,
            tree,
        })
        .unwrap();
        let proven = chain(&key).merkle_tree(proof, head.root).unwrap();
        assert_eq!(proven.tree, tree);
        assert_eq!(proven.address, MERKLE_TREE_ADDRESS);

        // Another tree under the same root does not prove.
        let other = serde_json::to_value(TreeProof {
            height: 10,
            timestamp: 100,
            tree: MerkleTree::default(),
        })
        .unwrap();
        assert!(chain(&key).merkle_tree(other, head.root).is_err());
    }

    #[test]
    fn a_stranger_another_domain_or_an_old_height_is_refused() {
        let key = SigningKey::from_bytes(&[1; 32]);
        let stranger = SigningKey::from_bytes(&[2; 32]);
        let tree = MerkleTree::default();
        assert!(chain(&key)
            .verify(signed(&stranger, 7, 10, &tree), &trusted(9))
            .is_err());
        assert!(chain(&key)
            .verify(signed(&key, 8, 10, &tree), &trusted(9))
            .is_err());
        assert!(chain(&key)
            .verify(signed(&key, 7, 9, &tree), &trusted(9))
            .is_err());
    }
}
