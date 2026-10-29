//! Against a running voyager node: gather a step the way the route does, and put it through the
//! enclave's own attest code in-process. Proves the node's `/bridge` API and the enclave agree
//! on headers, trees and message ids, without a TEE.
//!
//! ```sh
//! VOYAGER_RPC=http://127.0.0.1:3101 VOYAGER_CHAIN=voyager-a VOYAGER_FROM=5 \
//!   cargo test -p tee-coprocessor --test voyager_live -- --ignored --nocapture
//! ```
//!
//! `VOYAGER_FROM` is the height to bootstrap the ISM at: one before the messages to attest.

use serde_json::json;
use tee_coprocessor::origin::l2::voyager::{Config, Voyager};
use tee_coprocessor::origin::Indexer;

#[tokio::test]
#[ignore = "needs a running voyager node"]
async fn a_live_voyager_step_attests() {
    let rpc = std::env::var("VOYAGER_RPC").expect("VOYAGER_RPC");
    let chain = std::env::var("VOYAGER_CHAIN").unwrap_or_else(|_| "voyager-a".into());
    let from: u64 = std::env::var("VOYAGER_FROM")
        .expect("VOYAGER_FROM")
        .parse()
        .unwrap();
    let domain = tee_node::origin::find(&chain).unwrap().domain;
    let origin = Voyager::new(Config {
        domain,
        enclave_chain: chain.clone(),
        rpc,
    })
    .unwrap();

    let trusted = origin.bootstrap([9; 32], Some(from)).await.unwrap();
    let step = origin.gather(&trusted).await.unwrap();
    assert!(
        step.head > trusted.height,
        "the node has not moved past {from}"
    );
    let messages = origin.index(trusted.height, step.head).await.unwrap();
    assert_eq!(
        messages.len(),
        step.leaves.len(),
        "the index disagrees with the tree"
    );
    println!(
        "heights {}..{}: leaves {:?}, {} messages",
        trusted.height,
        step.head,
        step.leaves,
        messages.len()
    );

    // Exactly what `route.rs` sends the enclave.
    let request = json!({
        "protocol": tee_node::attest::PROTOCOL_VERSION,
        "trusted_state": hex::encode(trusted.encode()),
        "chain": step.chain,
        "input": step.input,
        "tree": step.tree,
        "tree_snapshot": step.tree_snapshot,
        "message_ids": messages.iter().map(|m| m.id).collect::<Vec<_>>(),
        "merkle_tree_address": step.tree_address,
    });
    let (update, payload) =
        tee_node::attest::build_attested_update(serde_json::from_value(request).unwrap()).unwrap();
    assert_eq!(update.new_state.height, step.head);
    assert_eq!(update.message_ids.len(), messages.len());
    println!(
        "attested payload: {} bytes, new root 0x{}",
        payload.len(),
        hex::encode(update.new_state.state_root)
    );

    // A forged id list is refused: the enclave replays the ids against the tree.
    if !messages.is_empty() {
        let forged = json!({
            "protocol": tee_node::attest::PROTOCOL_VERSION,
            "trusted_state": hex::encode(trusted.encode()),
            "chain": step.chain,
            "input": step.input,
            "tree": step.tree,
            "tree_snapshot": step.tree_snapshot,
            "message_ids": vec![[0u8; 32]; messages.len()],
            "merkle_tree_address": step.tree_address,
        });
        assert!(
            tee_node::attest::build_attested_update(serde_json::from_value(forged).unwrap())
                .is_err()
        );
    }
}
