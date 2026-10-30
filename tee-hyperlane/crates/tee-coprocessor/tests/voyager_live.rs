//! Against a running voyager node: gather a step the way the route does, put it through the
//! enclave's own attest code in-process, and build each message's delivery branch against the
//! root it attests. Proves the node's `/bridge` API, the enclave and the relayer's proofs agree
//! on headers, trees and message ids, without a TEE.
//!
//! ```sh
//! VOYAGER_RPC=http://127.0.0.1:3101 VOYAGER_CHAIN=voyager-a VOYAGER_FROM=5 \
//!   cargo test -p tee-coprocessor --test voyager_live -- --ignored --nocapture
//! ```
//!
//! `VOYAGER_FROM` is the height to bootstrap the ISM at: one before the messages to attest.

use hyperlane_types::get_branch_root;
use serde_json::json;
use tee_coprocessor::origin::l2::voyager::{Config, Voyager};
use tee_coprocessor::origin::Indexer;
use tee_coprocessor::proofs::ProofBuilder;

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
        "merkle_tree_address": step.tree_address,
    });
    let (update, payload) =
        tee_node::attest::build_attested_update(serde_json::from_value(request).unwrap()).unwrap();
    assert_eq!(update.new_state.height, step.head);
    assert_eq!(update.tree_count, step.leaves.end);
    println!(
        "attested payload: {} bytes, tree root 0x{} at {} leaves",
        payload.len(),
        hex::encode(update.tree_root),
        update.tree_count
    );

    // Each message's branch, built as the route builds it: from the tree at the trusted height
    // and the indexed leaves, folding to the root the enclave attested.
    let chain_def = tee_node::origin::find(&chain).unwrap();
    let frontier = chain_def
        .origin
        .merkle_tree(step.tree_snapshot.clone(), trusted.state_root.into())
        .unwrap()
        .tree;
    let ids: Vec<[u8; 32]> = messages.iter().map(|m| m.id).collect();
    let mut builder = ProofBuilder::new(&frontier, &ids).unwrap();
    for (i, id) in ids.iter().enumerate() {
        let index = frontier.count + i as u32;
        let proof = builder.proof(index).unwrap();
        assert_eq!(get_branch_root(*id, &proof, index), update.tree_root);
        // A forged message does not fold to the attested root.
        assert_ne!(get_branch_root([0; 32], &proof, index), update.tree_root);
    }
}
