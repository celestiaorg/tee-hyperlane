//! Gathers what the enclave needs to verify a voyager chain: its sequencer's latest signed
//! header, and its bridge lane's tree at both ends of the step. All of it from the node's own
//! `/bridge` API, which is untrusted: the enclave rehashes every tree under a signed root.

use alloy_primitives::B256;
use anyhow::{Context, Result};
use async_trait::async_trait;
use hyperlane_types::{MerkleTree, TREE_DEPTH};
use serde::Deserialize;
use serde_json::Value;
use tee_node::chains::l2::voyager::{SignedHeader, TreeProof, MERKLE_TREE_ADDRESS};
use tee_node::origin::Chain;
use tee_node::state::IsmState;

use crate::origin::{self, Indexer, Message, Step};

/// `[chains.<name>]` for a voyager chain.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub domain: u32,
    /// The enclave chain that attests it: `voyager-a` or `voyager-b`.
    pub enclave_chain: String,
    /// The node's HTTP API.
    pub rpc: String,
}

pub struct Voyager {
    chain: &'static Chain,
    node: Node,
}

impl Voyager {
    pub fn new(config: Config) -> Result<Self> {
        let chain = tee_node::origin::find(&config.enclave_chain)
            .with_context(|| format!("this build attests no `{}`", config.enclave_chain))?;
        anyhow::ensure!(
            chain.domain == config.domain,
            "`{}` is domain {}, not {}",
            chain.name,
            chain.domain,
            config.domain
        );
        Ok(Self {
            chain,
            node: Node::new(&config.rpc),
        })
    }

    /// The tree as of `height`, with the header fields its root also covers.
    async fn tree_proof(&self, height: u64, timestamp: u64) -> Result<Value> {
        let tree = self.node.tree(height).await?;
        Ok(serde_json::to_value(TreeProof {
            height,
            timestamp,
            tree,
        })?)
    }
}

#[async_trait]
impl Indexer for Voyager {
    async fn gather(&self, trusted: &IsmState) -> Result<Step> {
        let header = self.node.latest_header().await?;
        if header.height <= trusted.height {
            return Ok(Step::idle(trusted.height));
        }
        let root = B256::from(header.root);
        let tree = self.tree_proof(header.height, header.timestamp).await?;
        // The ISM's own height and timestamp are what its root hashed, so they rebuild it.
        let snapshot = self.tree_proof(trusted.height, trusted.timestamp).await?;
        let leaves = origin::leaves(self.chain, &snapshot, trusted.state_root, &tree, root)?;
        Ok(Step {
            head: header.height,
            state_root: root,
            leaves,
            chain: self.chain.name,
            input: serde_json::to_value(&header)?,
            tree,
            tree_snapshot: snapshot,
            tree_address: MERKLE_TREE_ADDRESS,
        })
    }

    async fn index(&self, from: u64, to: u64) -> Result<Vec<Message>> {
        Ok(self
            .node
            .messages(from, to)
            .await?
            .into_iter()
            .map(|(_, message)| message)
            .collect())
    }

    async fn bootstrap(&self, identity: [u8; 32], height: Option<u64>) -> Result<IsmState> {
        let header = match height {
            Some(height) => self.node.header(height).await?,
            None => self.node.latest_header().await?,
        };
        Ok(IsmState {
            state_root: header.root,
            origin_domain: self.chain.domain,
            height: header.height,
            timestamp: header.timestamp,
            lc_store_commit: [0; 32],
            identity_digest: identity,
        })
    }
}

/// A voyager node's `/bridge` API.
#[derive(Clone)]
pub struct Node {
    url: String,
    http: reqwest::Client,
}

impl Node {
    pub fn new(url: &str) -> Self {
        Self {
            url: url.trim_end_matches('/').to_string(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .expect("http client"),
        }
    }

    pub async fn get(&self, path: &str) -> Result<Value> {
        let response = self
            .http
            .get(format!("{}{path}", self.url))
            .send()
            .await
            .with_context(|| format!("GET {path} from the voyager node"))?;
        let status = response.status();
        let text = response.text().await?;
        anyhow::ensure!(
            status.is_success(),
            "GET {path}: HTTP {status}: {}",
            crate::brief(&text)
        );
        Ok(serde_json::from_str(&text)?)
    }

    pub async fn post(&self, path: &str, body: &Value) -> Result<Value> {
        let response = self
            .http
            .post(format!("{}{path}", self.url))
            .json(body)
            .send()
            .await
            .with_context(|| format!("POST {path} to the voyager node"))?;
        let status = response.status();
        let text = response.text().await?;
        anyhow::ensure!(
            status.is_success(),
            "POST {path}: HTTP {status}: {}",
            crate::brief(&text)
        );
        Ok(serde_json::from_str(&text)?)
    }

    pub async fn latest_header(&self) -> Result<SignedHeader> {
        Ok(serde_json::from_value(
            self.get("/bridge/header/latest").await?,
        )?)
    }

    pub async fn header(&self, height: u64) -> Result<SignedHeader> {
        Ok(serde_json::from_value(
            self.get(&format!("/bridge/header/{height}")).await?,
        )?)
    }

    pub async fn tree(&self, height: u64) -> Result<MerkleTree> {
        let reply = self.get(&format!("/bridge/tree/{height}")).await?;
        let count = reply["count"].as_u64().context("count")? as u32;
        let nodes = reply["branch"].as_array().context("branch")?;
        anyhow::ensure!(
            nodes.len() == TREE_DEPTH,
            "branch has {} nodes",
            nodes.len()
        );
        let mut branch = [[0u8; 32]; TREE_DEPTH];
        for (slot, node) in branch.iter_mut().zip(nodes) {
            *slot = hex::decode(node.as_str().context("branch node")?)?
                .try_into()
                .map_err(|_| anyhow::anyhow!("branch node is not 32 bytes"))?;
        }
        Ok(MerkleTree { branch, count })
    }

    /// Messages dispatched in slots `from + 1 ..= to`, with their heights, in nonce order.
    pub async fn messages(&self, from: u64, to: u64) -> Result<Vec<(u64, Message)>> {
        let reply = self
            .get(&format!("/bridge/messages?from={from}&to={to}"))
            .await?;
        reply["messages"]
            .as_array()
            .context("messages")?
            .iter()
            .map(|m| {
                let bytes = hex::decode(m["message"].as_str().context("message")?)?;
                let id: [u8; 32] = hex::decode(m["id"].as_str().context("id")?)?
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("id is not 32 bytes"))?;
                anyhow::ensure!(
                    alloy_primitives::keccak256(&bytes).0 == id,
                    "the node's id does not hash its message"
                );
                Ok((
                    m["height"].as_u64().context("height")?,
                    Message { id, bytes },
                ))
            })
            .collect()
    }

    pub async fn ism(&self, origin: u32) -> Result<IsmState> {
        let reply = self.get(&format!("/bridge/ism/{origin}")).await?;
        let state = hex::decode(reply["state"].as_str().context("state")?)?;
        Ok(IsmState::decode(&state)?)
    }

    pub async fn delivered(&self, id: &str) -> Result<bool> {
        let reply = self
            .get(&format!(
                "/bridge/delivered/{}",
                id.trim_start_matches("0x")
            ))
            .await?;
        Ok(reply["delivered"].as_bool() == Some(true))
    }

    pub async fn head(&self) -> Result<u64> {
        Ok(self.latest_header().await?.height)
    }
}
