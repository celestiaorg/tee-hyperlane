//! Hyperlane wire formats, the incremental merkle tree, the ISM state the enclave attests to
//! and dstack's event log: written once and shared by the enclave, the coprocessor, the CLI and
//! voyager's bridge lane.

pub mod dstack;
pub mod ism;
pub mod merkle;
pub mod message;
pub mod voyager;

pub use ism::{AttestedUpdate, DecodeError, IsmState, ATTESTED_UPDATE_BYTES, ISM_STATE_BYTES};
pub use merkle::{
    branch_proof, build_tree_from_slots, get_branch_root, get_tree_root, insert_leaf, keccak_pair,
    zero_hashes, DeliveryMetadata, LayoutError, MalformedMetadata, MerkleError, MerkleTree,
    MerkleTreeSlots, ProofError, METADATA_BYTES, SEPOLIA_MERKLE_TREE_BASE_SLOT, TREE_DEPTH,
};
pub use message::{
    decode_hyperlane_message, decode_token_message_body, encode_hyperlane_message,
    encode_token_message_body, get_message_id, HyperlaneMessage, MessageTooShort, TokenMessage,
};

pub fn keccak256(data: &[u8]) -> [u8; 32] {
    use tiny_keccak::{Hasher, Keccak};
    let mut k = Keccak::v256();
    let mut out = [0u8; 32];
    k.update(data);
    k.finalize(&mut out);
    out
}
