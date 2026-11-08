//! What the enclave refuses, whatever the origin.
//!
//! Every origin goes through the same pipeline in `attest::attest_with`, so its checks are
//! tested once here against a stand-in chain, with no light-client fixtures. Each check exists
//! because its absence was once a critical: taking "where to look" from whoever was asking,
//! then attesting something else.

use std::sync::Mutex;

use alloy_primitives::B256;
use hyperlane_types::{get_tree_root, insert_leaf, MerkleTree};
use serde_json::{json, Value};
use tee_node::attest::{attest_with, AttestRequest, PROTOCOL_VERSION};
use tee_node::origin::{Chain, Head, Origin, Tree};
use tee_node::state::{AttestedUpdate, IsmState, ATTESTED_UPDATE_BYTES};

const HOOK: [u8; 32] = [7u8; 32];
const TRUSTED_ROOT: [u8; 32] = [1u8; 32];
const NEW_ROOT: [u8; 32] = [2u8; 32];
const DOMAIN: u32 = 4242;

/// A chain whose head is always `NEW_ROOT` and whose tree there has two leaves. Records every
/// root it was asked to read a tree under.
struct StandIn {
    tree_at: [u8; 32],
    asked: Mutex<Vec<B256>>,
}

fn head_tree() -> MerkleTree {
    let mut t = MerkleTree::default();
    insert_leaf(&mut t, [10u8; 32]).unwrap();
    insert_leaf(&mut t, [11u8; 32]).unwrap();
    t
}

impl Origin for StandIn {
    fn verify(&self, _input: Value, _trusted: &IsmState) -> anyhow::Result<Head> {
        Ok(Head {
            root: NEW_ROOT.into(),
            height: 20,
            timestamp: 2000,
            store_commit: [9u8; 32],
            attested_at: 2100,
        })
    }

    fn merkle_tree(&self, _proof: Value, root: B256) -> anyhow::Result<Tree> {
        self.asked.lock().unwrap().push(root);
        anyhow::ensure!(root.0 == NEW_ROOT, "no tree under {root}");
        Ok(Tree {
            address: self.tree_at,
            tree: head_tree(),
        })
    }
}

fn chain(tree_at: [u8; 32]) -> Chain {
    on(Box::leak(Box::new(StandIn {
        tree_at,
        asked: Mutex::new(Vec::new()),
    })))
}

fn on(origin: &'static StandIn) -> Chain {
    Chain {
        name: "stand-in",
        domain: DOMAIN,
        origin,
    }
}

fn trusted() -> IsmState {
    IsmState {
        state_root: TRUSTED_ROOT,
        origin_domain: DOMAIN,
        height: 10,
        timestamp: 1000,
        lc_store_commit: [3u8; 32],
        identity_digest: [4u8; 32],
    }
}

fn request(trusted: IsmState) -> AttestRequest {
    serde_json::from_value(json!({
        "protocol": PROTOCOL_VERSION,
        "trusted_state": hex::encode(trusted.encode()),
        "chain": "stand-in",
        "input": {},
        "tree": {},
        "merkle_tree_address": HOOK,
    }))
    .unwrap()
}

#[test]
fn a_valid_step_moves_the_state_to_the_verified_head() {
    let (update, payload) = attest_with(&chain(HOOK), request(trusted())).unwrap();
    let next = update.new_state;
    assert_eq!(next.state_root, NEW_ROOT);
    assert_eq!((next.height, next.timestamp), (20, 2000));
    assert_eq!(next.lc_store_commit, [9u8; 32]);
    assert_eq!(next.origin_domain, DOMAIN);
    assert_eq!(
        next.identity_digest, [4u8; 32],
        "the identity is carried, never chosen"
    );
    assert_eq!(update.attested_at, 2100);
    assert_eq!(update.prev_state, trusted());
    assert_eq!(update.merkle_tree_address, HOOK);
    assert_eq!(payload.len(), ATTESTED_UPDATE_BYTES);
    assert_eq!(AttestedUpdate::decode(&payload).unwrap(), update);
}

/// What the attestation vouches for: the tree's root and size at the verified head, exactly as
/// the hook's storage proves them.
#[test]
fn the_update_carries_the_trees_root_and_count() {
    let (update, _) = attest_with(&chain(HOOK), request(trusted())).unwrap();
    assert_eq!(update.tree_root, get_tree_root(&head_tree()));
    assert_eq!(update.tree_count, 2);
}

/// The merkle-address hole: read one hook, attest another. The destination cannot catch it -
/// it only knows the address it pinned, which is exactly what an attacker would claim.
#[test]
fn a_tree_proven_at_another_hook_is_refused() {
    let err = attest_with(&chain([0xde; 32]), request(trusted())).unwrap_err();
    assert!(err.to_string().contains("was attested"), "{err}");
}

/// One origin's attestation must not advance another origin's ISM.
#[test]
fn an_ism_for_another_domain_is_refused() {
    let mut other = trusted();
    other.origin_domain = DOMAIN + 1;
    let err = attest_with(&chain(HOOK), request(other)).unwrap_err();
    assert!(err.to_string().contains("domain"), "{err}");
}

/// The tree is read under the root the origin verified, and under nothing else: whatever the
/// caller proves has to sit in state the chain itself commits to.
#[test]
fn the_tree_is_read_under_the_verified_root() {
    static RECORDING: StandIn = StandIn {
        tree_at: HOOK,
        asked: Mutex::new(Vec::new()),
    };
    attest_with(&on(&RECORDING), request(trusted())).unwrap();
    let asked = RECORDING.asked.lock().unwrap().clone();
    assert_eq!(asked, vec![B256::from(NEW_ROOT)]);
}

/// What stops a stale caller: a request that does not speak this protocol is refused rather
/// than having fields silently dropped. A request in the old shape, which carried the message
/// ids and a tree snapshot, is refused on its unknown fields as well as its version.
#[test]
fn a_request_for_another_protocol_is_refused() {
    let mut req = request(trusted());
    req.protocol = PROTOCOL_VERSION + 1;
    assert!(attest_with(&chain(HOOK), req).is_err());
    assert!(serde_json::from_value::<AttestRequest>(json!({ "trusted_state": "00" })).is_err());
    assert!(
        serde_json::from_value::<AttestRequest>(json!({
            "protocol": 1,
            "trusted_state": hex::encode(trusted().encode()),
            "chain": "stand-in",
            "input": {},
            "tree": {},
            "tree_snapshot": {},
            "message_ids": [],
            "merkle_tree_address": HOOK,
        }))
        .is_err(),
        "the old shape"
    );
    assert_eq!(
        PROTOCOL_VERSION, 2,
        "bump this when the request shape changes"
    );
}

#[test]
fn an_unknown_chain_is_refused() {
    let mut req = request(trusted());
    req.chain = "nowhere".into();
    assert!(tee_node::attest::build_attested_update(req).is_err());
}

/// Names are how requests and configs find a chain, domains are what ISMs pin. A duplicate of
/// either would make one of two chains unreachable, or reachable under the other's name.
#[test]
fn every_chain_has_a_unique_name_and_domain() {
    let all = tee_node::origin::chains();
    let mut names: Vec<_> = all.iter().map(|c| c.name).collect();
    let mut domains: Vec<_> = all.iter().map(|c| c.domain).collect();
    names.sort();
    domains.sort();
    names.dedup();
    domains.dedup();
    assert_eq!(names.len(), all.len());
    assert_eq!(domains.len(), all.len());
    assert_eq!(names, ["arbitrum", "base", "celestia", "eden", "ethereum"]);
}

/// An origin where nothing was dispatched still attests: the root is the same, the state moves
/// to the new head. Whether that is too soon is the ISMs' call, which know the root they hold.
#[test]
fn a_quiet_origin_attests_the_unchanged_root() {
    let (update, _) = attest_with(&chain(HOOK), request(trusted())).unwrap();
    let (again, _) = attest_with(&chain(HOOK), request(trusted())).unwrap();
    assert_eq!(update.tree_root, again.tree_root);
    assert_eq!(update.new_state.state_root, NEW_ROOT);
}

/// Writes the vectors `x/teeism` and `TeeDcapIsm.sol` pin their decoders to. Run by hand:
/// `TEEISM_VECTORS_OUT=/path/vectors.json cargo test -p tee-node --test attest_request -- --ignored`.
#[test]
#[ignore = "writes the cross-implementation test vectors; set TEEISM_VECTORS_OUT"]
fn write_test_vectors() {
    use hyperlane_types::{branch_proof, get_branch_root};
    use sha2::{Digest, Sha256};

    let out = std::env::var("TEEISM_VECTORS_OUT").expect("TEEISM_VECTORS_OUT");
    let state = |n: u8, height: u64, timestamp: u64| IsmState {
        state_root: [n; 32],
        origin_domain: 0x0102_0304,
        height,
        timestamp,
        lc_store_commit: [n + 1; 32],
        identity_digest: [n + 2; 32],
    };
    let updates = [
        AttestedUpdate {
            prev_state: state(1, 10, 1000),
            new_state: state(4, 20, 2000),
            merkle_tree_address: [0xab; 32],
            attested_at: 2100,
            tree_root: [0xcd; 32],
            tree_count: 0x0a0b_0c0d,
        },
        AttestedUpdate {
            prev_state: state(0x10, 5, 1700000000),
            new_state: state(0x20, 6, 1700000012),
            merkle_tree_address: [0; 32],
            attested_at: 1700000012,
            tree_root: [0; 32],
            tree_count: 0,
        },
    ];
    let hex32 = |b: &[u8]| format!("0x{}", hex::encode(b));
    let payloads: Vec<Value> = updates
        .iter()
        .map(|u| {
            let encoded = u.encode();
            json!({
                "prev_state": hex32(&u.prev_state.encode()),
                "new_state": hex32(&u.new_state.encode()),
                "merkle_tree_address": hex32(&u.merkle_tree_address),
                "attested_at": u.attested_at,
                "tree_root": hex32(&u.tree_root),
                "tree_count": u.tree_count,
                "encoded_hex": hex32(&encoded),
                "sha256_hex": hex32(&Sha256::digest(&encoded)),
            })
        })
        .collect();

    // A seven-leaf tree, every leaf proven against its root, plus the frontier after three
    // leaves so a frontier-based proof generator can be checked too.
    let leaves: Vec<[u8; 32]> = (0u32..7)
        .map(|i| hyperlane_types::keccak256(&i.to_be_bytes()))
        .collect();
    let mut tree = MerkleTree::default();
    let mut frontier = None;
    for (i, l) in leaves.iter().enumerate() {
        if i == 3 {
            frontier = Some(tree);
        }
        insert_leaf(&mut tree, *l).unwrap();
    }
    let frontier = frontier.unwrap();
    let root = get_tree_root(&tree);
    let empty = MerkleTree::default();
    let proofs: Vec<Value> = (0..leaves.len() as u32)
        .map(|i| {
            let proof = branch_proof(&empty, &leaves, i).unwrap();
            assert_eq!(get_branch_root(leaves[i as usize], &proof, i), root);
            if i >= 3 {
                assert_eq!(branch_proof(&frontier, &leaves[3..], i).unwrap(), proof);
            }
            json!({ "index": i, "proof": proof.iter().map(|p| hex32(p)).collect::<Vec<_>>() })
        })
        .collect();

    let vectors = json!({
        "payloads": payloads,
        "tree": {
            "leaves": leaves.iter().map(|l| hex32(l)).collect::<Vec<_>>(),
            "root_hex": hex32(&root),
            "count": tree.count,
            "proofs": proofs,
        },
        "frontier": {
            "count": frontier.count,
            "branch": frontier.branch.iter().map(|b| hex32(b)).collect::<Vec<_>>(),
        },
    });
    std::fs::write(out, serde_json::to_string_pretty(&vectors).unwrap()).unwrap();
}
