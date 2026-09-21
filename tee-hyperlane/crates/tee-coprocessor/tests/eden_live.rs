//! The evolve path end to end against live mocha and live Eden.
//!
//! Ignored by default: it needs the mocha light node and reaches the public internet.
//!
//!   cargo test -p tee-coprocessor --test eden_live -- --ignored --nocapture
//!
//! What it proves is the join nothing else covers: that a blob and its namespace proofs,
//! fetched from a DA node, actually place a signed Eden header inside a Celestia block whose
//! header a light client verified, and that the state root that falls out is the one Eden's
//! own RPC reports.

use tee_coprocessor::celestia::CelestiaReader;
use tee_coprocessor::celestia_da::DaClient;
use tee_node::origins::celestia_l2::{verify_evolve_root, EvolveChain, EvolveHeaderProof};

fn da_url() -> String {
    std::env::var("MOCHA_DA_RPC").unwrap_or_else(|_| "http://localhost:26658".into())
}
fn consensus_url() -> String {
    std::env::var("MOCHA_RPC").unwrap_or_else(|_| "https://rpc-mocha.pops.one".into())
}
fn eden_url() -> String {
    std::env::var("EDEN_RPC").unwrap_or_else(|_| "https://rpc.testnet.eden.gateway.fm/".into())
}

#[tokio::test]
#[ignore]
async fn a_live_eden_header_is_provably_inside_a_celestia_block() {
    let da = DaClient::new(da_url());
    let ns = {
        let n = EvolveChain::EDEN.namespace;
        celestia_types::nmt::Namespace::new_v0(&n[18..]).unwrap()
    };

    let head = da.head().await.expect("DA head");
    println!("mocha DA head {head}");

    // Eden posts roughly once a minute, so a short walk back finds a block carrying it.
    let mut found = None;
    for h in (head.saturating_sub(40)..=head).rev() {
        let Ok(data) = da.namespace_data(h, &ns).await else {
            continue;
        };
        // A row with no shares is an absence proof: the namespace fits in that row's range
        // but nothing of ours is in it. Eden posts about every eleventh Celestia block.
        let shares: usize = data.rows().iter().map(|r| r.shares.len()).sum();
        if shares > 0 {
            println!("celestia {h} carries {} rows, {shares} shares", data.rows().len());
            found = Some((h, data));
            break;
        }
    }
    let (height, data) = found.expect("no eden namespace data in the last 40 celestia blocks");
    let dah = da.dah(height).await.expect("dah");

    // The header a light client would have verified for that block.
    let reader = CelestiaReader::new(&consensus_url()).unwrap();
    let light = reader.light_block(height).await.expect("light block");
    let header = light.signed_header.header.clone();

    // Attest the newest header this block carries, which is what the relayer does once it
    // holds a captured proof for that height.
    let target = tee_node::origins::celestia_l2::newest_signed_header(&data, &EvolveChain::EDEN)
        .expect("a signed eden header")
        .height;
    let proof = EvolveHeaderProof {
        dah,
        data,
        target_height: target,
    };
    let root = verify_evolve_root(&header, &EvolveChain::EDEN, &proof)
        .expect("the namespace data must prove into this celestia block");
    println!(
        "attested eden height {} root 0x{}",
        root.height,
        hex::encode(root.state_root)
    );

    // And the root must be the one Eden itself reports for that height.
    let body = serde_json::json!({
        "jsonrpc":"2.0","id":1,"method":"eth_getBlockByNumber",
        "params":[format!("0x{:x}", root.height), false]
    });
    let res: serde_json::Value = reqwest::Client::new()
        .post(eden_url())
        .json(&body)
        .send()
        .await
        .expect("eden rpc")
        .json()
        .await
        .expect("eden json");
    let reported = res["result"]["stateRoot"].as_str().expect("stateRoot");
    assert_eq!(
        reported.trim_start_matches("0x"),
        hex::encode(root.state_root),
        "the attested root must be the one Eden reports"
    );
    println!("matches eden's own stateRoot");
}
