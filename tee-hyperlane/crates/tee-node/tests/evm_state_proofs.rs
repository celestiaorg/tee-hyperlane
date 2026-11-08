//! End-to-end over live Sepolia state: block stateRoot -> account proof -> 33 storage
//! proofs -> Hyperlane merkle root, all verified rather than trusted.

use alloy_primitives::{Address, Bytes, B256, U256};
use hyperlane_types::{
    branch_proof, get_branch_root, get_message_id, get_tree_root, insert_leaf, HyperlaneMessage,
    MerkleTree,
};
use tee_node::evm::*;

#[derive(serde::Deserialize)]
struct Fixture {
    block_number: u64,
    state_root: B256,
    address: Address,
    base_slot: u64,
    account: ClaimedAccount,
    account_proof: Vec<Bytes>,
    storage_proof: Vec<ClaimedSlot>,
    expected_root: B256,
    expected_count: u32,
}

fn fixture() -> Fixture {
    serde_json::from_str(include_str!("../testdata/sepolia_hook_proof.json")).unwrap()
}

fn tree_proof(f: &Fixture) -> TreeProof {
    TreeProof {
        merkle_tree_hook: f.address,
        account: f.account,
        account_proof: f.account_proof.clone(),
        storage_proof: f.storage_proof.clone(),
    }
}

/// Read a tree the way the chains do: through `read_tree`, with the proof as JSON.
fn read(p: &TreeProof, root: B256, slot: u64) -> anyhow::Result<MerkleTree> {
    Ok(read_tree(serde_json::to_value(p).unwrap(), root, slot)?.tree)
}

fn refused(result: anyhow::Result<MerkleTree>, why: &str) {
    let err = result.expect_err("must be refused").to_string();
    assert!(err.contains(why), "expected `{why}`, got `{err}`");
}

#[test]
fn live_sepolia_state_root_yields_the_hyperlane_merkle_root() {
    let f = fixture();
    let tree = read(&tree_proof(&f), f.state_root, f.base_slot).expect("proof must verify");
    assert_eq!(tree.count, f.expected_count);
    assert_eq!(
        B256::from(get_tree_root(&tree)),
        f.expected_root,
        "reconstructed root disagrees with the hook's root() at block {}",
        f.block_number
    );
}

/// Twelve of the 33 slots are zero, so this path is exercised by the fixture itself:
/// Ethereum stores no zeros, so an unset branch level is proven by exclusion.
#[test]
fn zero_valued_branch_slots_are_proven_by_exclusion() {
    let f = fixture();
    let zeros = f.storage_proof.iter().filter(|s| s.value.is_zero()).count();
    assert!(zeros > 0, "fixture should contain unset branch levels");
    let storage_root =
        verify_account_proof(f.state_root, f.address, &f.account, &f.account_proof).unwrap();
    for s in f.storage_proof.iter().filter(|s| s.value.is_zero()) {
        verify_storage_proof(storage_root, s.slot, s.value, &s.proof)
            .expect("exclusion proof must verify");
    }
}

#[test]
fn a_wrong_state_root_is_rejected() {
    let f = fixture();
    refused(
        read(&tree_proof(&f), B256::repeat_byte(0xab), f.base_slot),
        "does not prove against state root",
    );
}

#[test]
fn a_tampered_slot_value_is_rejected() {
    let f = fixture();
    let mut p = tree_proof(&f);
    p.storage_proof[0].value = U256::from(1);
    refused(
        read(&p, f.state_root, f.base_slot),
        "does not prove against storage root",
    );
}

/// Claiming a real value for the wrong slot must not slip through.
#[test]
fn slots_must_arrive_in_the_expected_order() {
    let f = fixture();
    let mut p = tree_proof(&f);
    p.storage_proof.swap(0, 1);
    refused(
        read(&p, f.state_root, f.base_slot),
        "storage proof 0 is for slot",
    );
}

/// 151 is the L2 deployments' layout and wrong for Hyperlane's Sepolia hook. The slot is
/// pinned per origin rather than sent, so this is what a mis-pinned constant would do, not
/// what a caller could choose.
#[test]
fn a_wrong_base_slot_is_rejected() {
    let f = fixture();
    refused(
        read(&tree_proof(&f), f.state_root, 151),
        "storage proof 0 is for slot",
    );
}

/// Each origin pins its own slot, and it is the chain module's constant that decides, never
/// the request. Read through the chains themselves: Sepolia's pins 103 and reads the live
/// hook, an L2 pins 151 and must refuse the same proof.
#[test]
fn the_base_slot_is_pinned_per_origin() {
    use tee_node::chains::l1::ethereum::ETHEREUM;
    use tee_node::chains::l2::arbitrum::ARBITRUM;
    let f = fixture();
    let proof = serde_json::to_value(tree_proof(&f)).unwrap();
    let read = ETHEREUM
        .origin
        .merkle_tree(proof.clone(), f.state_root)
        .unwrap();
    assert_eq!(read.tree.count, f.expected_count);
    assert_eq!(read.address, padded(f.address));
    assert!(ARBITRUM.origin.merkle_tree(proof, f.state_root).is_err());
}

#[test]
fn a_short_storage_proof_is_rejected() {
    let f = fixture();
    let mut p = tree_proof(&f);
    p.storage_proof.pop();
    refused(
        read(&p, f.state_root, f.base_slot),
        "expected 33 storage proofs, got 32",
    );
}

// ---- message batch authorisation ----

fn msg(nonce: u32) -> HyperlaneMessage {
    HyperlaneMessage {
        version: 3,
        nonce,
        origin: 11155111,
        sender: [1u8; 32],
        destination: 1297040200,
        recipient: [2u8; 32],
        body: vec![nonce as u8; 4],
    }
}

fn tree_of(n: u32) -> (MerkleTree, Vec<[u8; 32]>) {
    let mut t = MerkleTree::default();
    let mut ids = Vec::new();
    for i in 0..n {
        let id = get_message_id(&msg(i));
        insert_leaf(&mut t, id).unwrap();
        ids.push(id);
    }
    (t, ids)
}

/// What delivery does: the message's id, its branch and its index fold to the attested root.
#[test]
fn a_dispatched_message_proves_itself_against_the_attested_root() {
    let (snapshot, _) = tree_of(5);
    let (onchain, all) = tree_of(9);
    let root = get_tree_root(&onchain);
    for index in 5..9u32 {
        let proof = branch_proof(&snapshot, &all[5..], index).unwrap();
        assert_eq!(get_branch_root(all[index as usize], &proof, index), root);
    }
}

#[test]
fn a_forged_message_id_does_not_fold_to_the_root() {
    let (snapshot, _) = tree_of(5);
    let (onchain, all) = tree_of(9);
    let proof = branch_proof(&snapshot, &all[5..], 6).unwrap();
    assert_ne!(
        get_branch_root([0xde; 32], &proof, 6),
        get_tree_root(&onchain)
    );
}

/// A branch is for one position: the same leaf claimed at another index folds elsewhere.
#[test]
fn a_message_at_the_wrong_index_does_not_fold_to_the_root() {
    let (snapshot, _) = tree_of(5);
    let (onchain, all) = tree_of(9);
    let proof = branch_proof(&snapshot, &all[5..], 6).unwrap();
    assert_ne!(get_branch_root(all[6], &proof, 7), get_tree_root(&onchain));
    assert_ne!(get_branch_root(all[7], &proof, 7), get_tree_root(&onchain));
}

/// A proof is against one root. Against the root of a tree with more leaves it fails, so the
/// relayer must name the root it built the proof for, which the ISM then has to have attested.
#[test]
fn a_proof_is_bound_to_its_root() {
    let (snapshot, _) = tree_of(5);
    let (at_nine, all) = tree_of(9);
    let (at_ten, more) = tree_of(10);
    let proof = branch_proof(&snapshot, &all[5..], 6).unwrap();
    assert_eq!(get_branch_root(all[6], &proof, 6), get_tree_root(&at_nine));
    assert_ne!(get_branch_root(all[6], &proof, 6), get_tree_root(&at_ten));
    let later = branch_proof(&snapshot, &more[5..], 6).unwrap();
    assert_eq!(get_branch_root(all[6], &later, 6), get_tree_root(&at_ten));
}

/// hyperlane-cosmos pre-fills a tree's unused branch levels with the canonical zero hashes;
/// Solidity leaves them zero. Those levels sit above the highest set bit of `count`, so they
/// change neither the root nor the proofs built from the frontier.
///
/// Comparing raw structs would therefore have rejected every Celestia-origin tree while proving
/// nothing - which is exactly what happened against live mocha-5 once.
#[test]
fn unused_branch_levels_may_differ_between_implementations() {
    use hyperlane_types::zero_hashes;

    let (solidity_style, _) = tree_of(3);
    let mut cosmos_style = solidity_style;
    let zeros = zero_hashes();
    // Only levels the tree has not reached yet.
    for level in 2..hyperlane_types::TREE_DEPTH {
        cosmos_style.branch[level] = zeros[level];
    }

    assert_ne!(solidity_style, cosmos_style, "the structs really do differ");
    assert_eq!(
        get_tree_root(&solidity_style),
        get_tree_root(&cosmos_style),
        "but they mean the same tree"
    );

    let (onchain, all) = tree_of(6);
    for index in 3..6u32 {
        assert_eq!(
            branch_proof(&cosmos_style, &all[3..], index).unwrap(),
            branch_proof(&solidity_style, &all[3..], index).unwrap()
        );
        let proof = branch_proof(&cosmos_style, &all[3..], index).unwrap();
        assert_eq!(
            get_branch_root(all[index as usize], &proof, index),
            get_tree_root(&onchain)
        );
    }
}
