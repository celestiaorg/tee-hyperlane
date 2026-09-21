//! A route counts insertions into its own tree and nobody else's.
//!
//! The scan asks the indexer for transactions carrying `EventInsertedIntoTree`, which is the
//! question "was a leaf inserted anywhere" rather than "was a leaf inserted into my tree".
//! While a chain had exactly one merkle tree hook those were the same question. Standing up a
//! second one made them different, and three routes then reported seventy-nine leaves where
//! their own tree had grown by one, refused the batch, and backed off.
//!
//! The event names the hook it belongs to, so the answer is to read it.

use tee_coprocessor::celestia::inserts_for_test as inserts_in;
use tendermint::abci::{Event, EventAttribute};

const OURS: [u8; 32] = [0x11; 32];
const THEIRS: [u8; 32] = [0x22; 32];

fn insert_event(hook: [u8; 32], index: u32, message_id: [u8; 32]) -> Event {
    let attr = |k: &str, v: String| EventAttribute::from((k, v, true));
    Event {
        kind: "hyperlane.core.post_dispatch.v1.EventInsertedIntoTree".to_string(),
        attributes: vec![
            attr("index", index.to_string()),
            attr("merkle_tree_hook_id", format!("\"0x{}\"", hex::encode(hook))),
            attr("message_id", format!("\"0x{}\"", hex::encode(message_id))),
        ],
    }
}

#[test]
fn an_insert_into_our_tree_counts() {
    let events = vec![insert_event(OURS, 7, [0xaa; 32])];
    let found = inserts_in(100, &events, OURS).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].tree_index, 7);
}

#[test]
fn an_insert_into_another_tree_does_not() {
    // The live failure: a second deployment's hook on the same chain.
    let events = vec![insert_event(THEIRS, 7, [0xbb; 32])];
    assert!(inserts_in(100, &events, OURS).unwrap().is_empty());
}

#[test]
fn a_block_carrying_both_counts_only_ours() {
    let events = vec![
        insert_event(THEIRS, 1, [0xb1; 32]),
        insert_event(OURS, 2, [0xa1; 32]),
        insert_event(THEIRS, 3, [0xb2; 32]),
    ];
    let found = inserts_in(100, &events, OURS).unwrap();
    assert_eq!(found.len(), 1, "only the leaf in our own tree");
    assert_eq!(found[0].tree_index, 2);
}
