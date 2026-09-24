//! Every chain's full pipeline against live endpoints, minus the quote: anchor a genesis a few
//! hours back, gather the step to today's head, and run the enclave's own `verify` and
//! `merkle_tree` on it. If this passes, the coprocessor and the enclave agree about that chain.
//!
//! Ignored by default because it needs the network. Run with:
//!
//! ```text
//! cargo test -p tee-coprocessor --test live -- --ignored --nocapture
//! ```
//!
//! Base needs an archive endpoint (`BASE_ARCHIVE_RPC`), Eden a celestia-node DA endpoint
//! (`EDEN_DA_RPC`); each is skipped without it.

use tee_coprocessor::config::Config;
use tee_node::origin::Chain;

const BEACON: &str = "https://ethereum-sepolia-beacon-api.publicnode.com";

fn config() -> Config {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../deploy/coprocessor.toml.example"
    );
    let mut config = Config::load(path).unwrap();
    config.proof_dir = tempfile::tempdir().unwrap().keep().display().to_string();
    config
}

/// A finalized checkpoint about `epochs` back, so the step after genesis has something to cover.
async fn checkpoint(epochs: u64) -> String {
    let get = |path: String| async move {
        reqwest::get(format!("{BEACON}{path}"))
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap()
    };
    let head = get("/eth/v1/beacon/headers/finalized".into()).await;
    let slot: u64 = head["data"]["header"]["message"]["slot"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    for back in epochs.. {
        let root = get(format!(
            "/eth/v1/beacon/blocks/{}/root",
            (slot / 32 - back) * 32
        ))
        .await;
        if let Some(root) = root["data"]["root"].as_str() {
            return root.to_string();
        }
    }
    unreachable!()
}

async fn run(config: &Config, name: &str, chain: &Chain) {
    let indexer = config.indexer(name).unwrap();
    let genesis = indexer.bootstrap([7; 32], None).await.expect("genesis");
    let step = indexer.gather(&genesis).await.expect("gather");
    if step.chain.is_empty() {
        eprintln!("{name}: no newer head than genesis yet; nothing to check");
        return;
    }
    let head = chain
        .origin
        .verify(step.input, &genesis)
        .expect("the enclave verifies the gathered head");
    assert_eq!(head.height, step.head);
    let before = chain
        .origin
        .merkle_tree(step.tree_snapshot, genesis.state_root.into())
        .expect("snapshot");
    let after = chain
        .origin
        .merkle_tree(step.tree, head.root)
        .expect("tree");
    assert_eq!(before.tree.count..after.tree.count, step.leaves);
    assert_eq!(after.address, step.tree_address);
    let messages = indexer
        .index(genesis.height, step.head)
        .await
        .expect("index");
    assert_eq!(
        messages.len(),
        step.leaves.len(),
        "the index agrees with the tree"
    );
    eprintln!(
        "{name}: {} -> {}, {} leaves",
        genesis.height,
        head.height,
        messages.len()
    );
}

fn anchored(checkpoint: String) -> Config {
    let mut config = config();
    config
        .chains
        .get_mut("sepolia")
        .unwrap()
        .insert("checkpoint".into(), checkpoint.into());
    config
}

#[tokio::test]
#[ignore]
async fn sepolia() {
    run(
        &anchored(checkpoint(8).await),
        "sepolia",
        &tee_node::ethereum::ETHEREUM,
    )
    .await;
}

#[tokio::test]
#[ignore]
async fn arbitrum() {
    run(
        &anchored(checkpoint(64).await),
        "arbitrum",
        &tee_node::ethereum::arbitrum::ARBITRUM,
    )
    .await;
}

#[tokio::test]
#[ignore]
async fn base() {
    let Ok(archive) = std::env::var("BASE_ARCHIVE_RPC") else {
        return eprintln!("set BASE_ARCHIVE_RPC to run");
    };
    let mut config = anchored(checkpoint(64).await);
    config
        .chains
        .get_mut("base")
        .unwrap()
        .insert("rpc".into(), archive.into());
    run(&config, "base", &tee_node::ethereum::base::BASE).await;
}

#[tokio::test]
#[ignore]
async fn eden() {
    let Ok(da) = std::env::var("EDEN_DA_RPC") else {
        return eprintln!("set EDEN_DA_RPC to run");
    };
    let mut config = config();
    config
        .chains
        .get_mut("eden")
        .unwrap()
        .insert("da_rpc".into(), da.into());
    run(&config, "eden", &tee_node::celestia::eden::EDEN).await;
}
