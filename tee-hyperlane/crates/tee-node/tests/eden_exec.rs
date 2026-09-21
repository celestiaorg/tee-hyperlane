//! Re-execution against blocks taken off Eden itself.
//!
//! Both fixtures are real: the transaction in each is one of this bridge's own warp
//! transfers, with the witness `debug_executionWitness` returned for that block. A test that
//! passes here is the executor agreeing with ev-reth on a state root, which is the only
//! standard worth holding it to.

use alloy_consensus::Header;
use alloy_primitives::{Bytes, B256};
use alloy_rlp::Encodable;
use tee_node::evm::{execute_block, BlockExec};

struct Fixture {
    parent_number: u64,
    parent_state_root: B256,
    state_root: B256,
    input: BlockExec,
}

fn load(name: &str) -> Fixture {
    let raw = std::fs::read_to_string(format!(
        "{}/testdata/evolve/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .expect("fixture");
    let v: serde_json::Value = serde_json::from_str(&raw).expect("json");

    let header: Header = serde_json::from_value(v["block"].clone()).expect("header");
    // The fixture is only usable if the header round-trips, because the enclave is handed
    // RLP and the coprocessor builds that RLP from exactly this JSON.
    let mut encoded = Vec::new();
    header.encode(&mut encoded);
    let hash: B256 = serde_json::from_value(v["block"]["hash"].clone()).unwrap();
    assert_eq!(
        alloy_primitives::keccak256(&encoded),
        hash,
        "re-encoded header does not hash to the block hash"
    );

    let bytes = |k: &str| -> Vec<Bytes> {
        v[k].as_array()
            .unwrap()
            .iter()
            .map(|x| serde_json::from_value(x.clone()).unwrap())
            .collect()
    };

    Fixture {
        parent_number: v["parentNumber"].as_u64().unwrap(),
        parent_state_root: serde_json::from_value(v["parentStateRoot"].clone()).unwrap(),
        state_root: serde_json::from_value(v["stateRoot"].clone()).unwrap(),
        input: BlockExec {
            header: encoded.into(),
            transactions: bytes("transactions"),
            state: bytes("state"),
            codes: bytes("codes"),
            ancestors: bytes("headers"),
        },
    }
}

const FIXTURES: [&str; 3] = [
    "eden_block_266047380.json",
    "eden_block_266001325.json",
    "eden_block_265995327.json",
];

#[test]
fn reproduces_edens_state_root() {
    for name in FIXTURES {
        let f = load(name);
        let header = execute_block(f.parent_number, f.parent_state_root, &f.input)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(header.state_root, f.state_root, "{name}");
    }
}

#[test]
fn rejects_a_tampered_state_root() {
    for name in FIXTURES {
        let mut f = load(name);
        let mut header: Header = alloy_rlp::Decodable::decode(&mut f.input.header.as_ref()).unwrap();
        header.state_root = B256::repeat_byte(9);
        let mut encoded = Vec::new();
        header.encode(&mut encoded);
        f.input.header = encoded.into();
        let err = execute_block(f.parent_number, f.parent_state_root, &f.input)
            .expect_err("a forged root must not verify");
        assert!(format!("{err}").contains("state root"), "{name}: {err}");
    }
}

#[test]
fn rejects_an_added_transaction() {
    // The transactions are bound to the header by its own transactions root, so slipping one
    // in is caught before anything is executed.
    let mut f = load(FIXTURES[0]);
    let extra = f.input.transactions[0].clone();
    f.input.transactions.push(extra);
    let err = execute_block(f.parent_number, f.parent_state_root, &f.input)
        .expect_err("an extra transaction must not verify");
    assert!(format!("{err}").contains("transactions root"), "{err}");
}

#[test]
fn rejects_a_wrong_parent_state() {
    let f = load(FIXTURES[0]);
    let err = execute_block(f.parent_number, B256::repeat_byte(3), &f.input)
        .expect_err("executing against the wrong pre-state must not verify");
    let text = format!("{err}");
    assert!(
        text.contains("witness has no node") || text.contains("state root"),
        "{text}"
    );
}

#[test]
fn rejects_a_block_that_does_not_advance() {
    let f = load(FIXTURES[0]);
    let err = execute_block(f.parent_number + 5, f.parent_state_root, &f.input)
        .expect_err("a block at or below its parent must not verify");
    assert!(format!("{err}").contains("numbered below"), "{err}");
}
