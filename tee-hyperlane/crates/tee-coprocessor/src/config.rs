//! One TOML file describes every route the coprocessor drives.
//!
//! A route is one direction: messages leaving an origin chain and arriving at a destination
//! chain. Adding a direction is a config block, not code.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// How often to look for work. Interval-driven rather than event-driven, so behaviour
    /// is the same whether the chain is busy or idle.
    #[serde(default = "default_tick_secs")]
    pub tick_secs: u64,
    /// How far behind a Celestia head to attest, in blocks.
    ///
    /// One is the structural floor: the app hash for height H lives in the header at H+1, so
    /// the enclave always reads one block ahead of the state it proves. Anything above that
    /// is margin against an RPC reporting a head it cannot yet serve proofs for - not against
    /// reorgs, which Tendermint does not have.
    #[serde(default = "default_celestia_lag")]
    pub celestia_lag: u64,
    /// Where finished proofs are kept, so a crash after proving does not discard the work.
    #[serde(default = "default_proof_dir")]
    pub proof_dir: String,
    pub routes: Vec<RouteConfig>,
}

fn default_l2_base_slot() -> u64 {
    151
}

fn default_ism_module() -> String {
    "zkism".to_string()
}

fn default_celestia_lag() -> u64 {
    2
}

fn default_tick_secs() -> u64 {
    60
}

fn default_proof_dir() -> String {
    "~/.tee-hyperlane/proofs".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteConfig {
    /// Human-readable, used in logs and as the proof-store subdirectory.
    pub name: String,
    pub origin: ChainConfig,
    pub destination: ChainConfig,
    /// The enclave that attests this origin.
    pub tee_node_url: String,
    /// ISM to advance on the destination chain.
    pub ism_id: String,
    /// Origin merkle tree hook, 32 bytes hex.
    pub merkle_tree_address: String,
    /// Optional override for an Ethereum origin's light-client checkpoint. Left unset, it
    /// is derived from the ISM's trusted state each tick, so the route needs no memory of
    /// its own.
    #[serde(default)]
    pub checkpoint: Option<String>,
    /// Our warp routers on the destination, 32-byte hex, as a Hyperlane message addresses
    /// them.
    ///
    /// What makes a message worth proving for. The destination domain alone is too coarse:
    /// an origin's tree is shared, so on Sepolia's canonical mailbox every other bridge's
    /// Celestia-bound traffic reads as ours and starts a proof we have no message in. Naming
    /// the recipients we actually serve narrows the trigger to our own transfers.
    ///
    /// Left empty the route falls back to matching on destination domain, which is the older
    /// and looser behaviour - wasteful, never unsafe.
    #[serde(default)]
    pub routers: Vec<String>,
    /// Skip proving and submit the enclave's quote as it stands.
    ///
    /// A TEE ISM verifies the attestation itself, so wrapping it in a Groth16 proof would
    /// only re-prove what the destination is about to check anyway. The scanner, the
    /// finality gate and the trigger are unchanged: all that is removed is the proof.
    #[serde(default)]
    pub attest_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChainConfig {
    Ethereum {
        domain: u32,
        execution_rpc: String,
        /// Reads at the ISM's trusted height go here when set. A public node serves state
        /// proofs for roughly the last 128 blocks; if the relayer is down longer than that,
        /// the snapshot it must prove is already pruned and the route cannot resume without
        /// an archive node.
        #[serde(default)]
        archive_rpc: Option<String>,
        mailbox: String,
        /// The three fields below describe how to read this chain's outbox, so they are
        /// needed only when it is a route's *origin*. As a destination the coprocessor just
        /// calls the mailbox and the ISM.
        #[serde(default)]
        beacon_rpc: Option<String>,
        #[serde(default)]
        merkle_tree_hook: Option<String>,
        /// Base storage slot of the hook's incremental tree. Deployment-specific: Hyperlane's
        /// canonical Sepolia hook uses 103, celestia-zkevm's testnet deployment uses 151.
        #[serde(default)]
        merkle_tree_base_slot: Option<u64>,
    },
    Celestia {
        domain: u32,
        rpc: String,
        /// Historical state proofs and tx search at the ISM's trusted height. See
        /// `archive_rpc` above; Celestia RPCs prune both.
        #[serde(default)]
        archive_rpc: Option<String>,
        grpc: String,
        mailbox_id: String,
        merkle_tree_hook_id: String,
        /// Which ISM module on the destination holds this route's ISM.
        ///
        /// `zkism` verifies a pair of Groth16 proofs; `teeism` verifies the enclave's quote
        /// directly and needs only one transaction, because there is only ever one quote and
        /// the second proof existed solely to project it through a second public-value shape.
        #[serde(default = "default_ism_module")]
        ism_module: String,
    },
    /// Rides on an Ethereum light client rather than its own.
    EthereumL2 {
        domain: u32,
        /// Must serve state at the *confirmed* L2 block, which is thousands of blocks behind
        /// head. Public nodes have pruned it, so this is an archive endpoint.
        l2_rpc: String,
        /// Where to read this L2's dispatch logs, when `l2_rpc` will not serve the span.
        ///
        /// The archive endpoints available to us on a free plan cap `eth_getLogs` at ten
        /// blocks, and a confirmed L2 head moves thousands at a time, so the two reads want
        /// different providers. Both are untrusted, so mixing them costs nothing.
        #[serde(default)]
        logs_rpc: Option<String>,
        /// The Ethereum chain whose light client secures this one.
        l1: Box<ChainConfig>,
        /// Which rollup this is: `arbitrum` or `base`. They prove different things.
        rollup: String,
        /// Arbitrum's BoLD RollupCore, or Base's AnchorStateRegistry.
        l1_anchor_contract: String,
        mailbox: String,
        merkle_tree_hook: String,
        /// Both L2s' hooks use 151; Sepolia's canonical one uses 103.
        #[serde(default = "default_l2_base_slot")]
        merkle_tree_base_slot: u64,
    },
}

impl ChainConfig {
    pub fn domain(&self) -> u32 {
        match self {
            ChainConfig::Ethereum { domain, .. }
            | ChainConfig::Celestia { domain, .. }
            | ChainConfig::EthereumL2 { domain, .. } => *domain,
        }
    }
}

impl Config {
    pub fn load(path: &str) -> anyhow::Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&raw)?)
    }
}
