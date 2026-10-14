//! EVM chains: signing, sending, deploying, and reading contracts.
//!
//! Every transaction is simulated first, so a revert is reported with its reason before any
//! gas is spent, and then sent and waited on until its receipt says it succeeded. Transactions
//! go out one at a time with the nonce tracked here, because public endpoints behind load
//! balancers hand out stale nonces and a "replacement transaction underpriced" reads like a
//! rejection rather than the collision it is.

use alloy::network::{EthereumWallet, TransactionBuilder};
use alloy::primitives::{address, keccak256, Address, Bytes, B256, U256};
use alloy::providers::{DynProvider, Provider, ProviderBuilder};
use alloy::rpc::types::{TransactionReceipt, TransactionRequest};
use anyhow::{bail, Context, Result};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use crate::shared::chains::EvmChain;
use crate::shared::print;
use crate::shared::settings::Ctx;

/// The deterministic deployment proxy every chain here has: CREATE2 with a caller-chosen salt.
pub const CREATE2_DEPLOYER: Address = address!("4e59b44847b379578588920cA78FbF26c0B4956C");

/// Who signs EVM owner transactions: EVM_PRIVATE_KEY, or a Ledger with EVM_SIGNER=ledger.
pub async fn signer(ctx: &Ctx) -> Result<(EthereumWallet, Address)> {
    match ctx.var_or("EVM_SIGNER", "key").as_str() {
        "key" => {
            let key = ctx.require(
                "EVM_PRIVATE_KEY",
                "it signs EVM deployments and owner transactions",
            )?;
            let s: alloy::signers::local::PrivateKeySigner = key
                .trim()
                .trim_start_matches("0x")
                .parse()
                .context("EVM_PRIVATE_KEY is not a valid secp256k1 private key")?;
            let addr = s.address();
            Ok((EthereumWallet::from(s), addr))
        }
        "ledger" => ledger(ctx).await,
        other => bail!("EVM_SIGNER must be `key` or `ledger`, not `{other}`"),
    }
}

#[cfg(feature = "ledger")]
async fn ledger(ctx: &Ctx) -> Result<(EthereumWallet, Address)> {
    use alloy::signers::ledger::{HDPath, LedgerSigner};
    use alloy::signers::Signer;
    let path = match ctx.var("EVM_LEDGER_PATH") {
        Some(p) => HDPath::Other(p),
        None => HDPath::LedgerLive(0),
    };
    let s = LedgerSigner::new(path, None).await.context(
        "could not open the Ledger; is it unlocked, with the Ethereum app open and blind signing on?",
    )?;
    let addr = s.address();
    Ok((EthereumWallet::from(s), addr))
}

#[cfg(not(feature = "ledger"))]
async fn ledger(_ctx: &Ctx) -> Result<(EthereumWallet, Address)> {
    bail!("EVM_SIGNER=ledger needs a build with Ledger support: cargo install --locked --path cli --features ledger")
}

/// The address EVM_PRIVATE_KEY controls: the relayer, and the account that seeds collateral.
pub fn relayer_key_address(ctx: &Ctx) -> Result<Address> {
    let key = ctx.require("EVM_PRIVATE_KEY", "the relayer's EVM key")?;
    let s: alloy::signers::local::PrivateKeySigner = key
        .trim()
        .trim_start_matches("0x")
        .parse()
        .context("EVM_PRIVATE_KEY is not a valid private key")?;
    Ok(s.address())
}

/// A read-only connection.
pub fn reader(rpc: &str) -> Result<DynProvider> {
    let url = rpc.parse().with_context(|| format!("`{rpc}` is not a URL"))?;
    Ok(ProviderBuilder::new().connect_http(url).erased())
}

/// A signing connection to one chain.
pub struct Chain {
    pub chain: &'static EvmChain,
    pub rpc: String,
    pub p: DynProvider,
    pub from: Address,
    nonce: Mutex<Option<u64>>,
}

impl Chain {
    pub async fn connect(ctx: &Ctx, chain: &'static EvmChain) -> Result<Self> {
        let (wallet, from) = signer(ctx).await?;
        Self::with_wallet(chain, chain.rpc(ctx), wallet, from).await
    }

    pub async fn with_wallet(
        chain: &'static EvmChain,
        rpc: String,
        wallet: EthereumWallet,
        from: Address,
    ) -> Result<Self> {
        let url = rpc.parse().with_context(|| format!("`{rpc}` is not a URL"))?;
        let p = ProviderBuilder::new().wallet(wallet).connect_http(url).erased();
        let id = p
            .get_chain_id()
            .await
            .with_context(|| format!("could not reach {} at {rpc}", chain.label))?;
        if id != chain.chain_id {
            bail!(
                "{rpc} serves chain id {id}, not {} ({})",
                chain.chain_id,
                chain.label
            );
        }
        Ok(Self {
            chain,
            rpc,
            p,
            from,
            nonce: Mutex::new(None),
        })
    }

    pub fn name(&self) -> &'static str {
        self.chain.name
    }

    /// Call `to` with `data`, simulate, send, wait. `what` names the transaction in errors.
    pub async fn send(&self, what: &str, to: Address, data: Bytes) -> Result<TransactionReceipt> {
        self.send_value(what, to, data, U256::ZERO).await
    }

    pub async fn send_value(
        &self,
        what: &str,
        to: Address,
        data: Bytes,
        value: U256,
    ) -> Result<TransactionReceipt> {
        let tx = TransactionRequest::default()
            .with_from(self.from)
            .with_to(to)
            .with_input(data)
            .with_value(value);
        self.submit(what, tx).await
    }

    /// Deploy `initcode` with a plain CREATE. Returns the new contract's address, once it has
    /// code there.
    pub async fn deploy(&self, what: &str, initcode: Bytes) -> Result<Address> {
        let tx = TransactionRequest::default()
            .with_from(self.from)
            .with_deploy_code(initcode);
        let receipt = self.submit(what, tx).await?;
        let addr = receipt.contract_address.with_context(|| {
            format!(
                "{what}: receipt {} names no contract address",
                receipt.transaction_hash
            )
        })?;
        self.wait_for_code(what, addr).await?;
        Ok(addr)
    }

    /// Deploy through the CREATE2 deployer. The address is fixed by `salt` and `initcode`, so
    /// one that already has code is the same contract and is left alone.
    pub async fn create2(&self, what: &str, salt: B256, initcode: &[u8]) -> Result<Address> {
        let addr = CREATE2_DEPLOYER.create2_from_code(salt, initcode);
        if self.has_code(addr).await? {
            print::info(format!("{what}: {addr} (already deployed)"));
            return Ok(addr);
        }
        if !self.has_code(CREATE2_DEPLOYER).await? {
            bail!(
                "{} has no CREATE2 deployer at {CREATE2_DEPLOYER}",
                self.chain.label
            );
        }
        let mut data = salt.to_vec();
        data.extend_from_slice(initcode);
        self.send(what, CREATE2_DEPLOYER, data.into()).await?;
        self.wait_for_code(what, addr).await?;
        print::info(format!("{what}: {addr}"));
        Ok(addr)
    }

    async fn submit(&self, what: &str, tx: TransactionRequest) -> Result<TransactionReceipt> {
        let label = self.chain.label;
        // Simulate first: a revert costs nothing here and comes back with its reason.
        self.p.call(tx.clone()).await.map_err(|e| {
            anyhow::anyhow!(
                "{what} on {label} would revert: {}",
                explain_revert(&e.to_string())
            )
        })?;

        let pending = self
            .p
            .get_transaction_count(self.from)
            .pending()
            .await
            .with_context(|| format!("{what}: reading {}'s nonce on {label}", self.from))?;
        let nonce = {
            let local = *self.nonce.lock().expect("nonce lock");
            local.map_or(pending, |n| n.max(pending))
        };
        let sent = self.p.send_transaction(tx.with_nonce(nonce)).await.map_err(|e| {
            anyhow::anyhow!(
                "{what} on {label}: not accepted by {}: {}",
                self.rpc,
                explain_revert(&e.to_string())
            )
        })?;
        let hash = *sent.tx_hash();
        *self.nonce.lock().expect("nonce lock") = Some(nonce + 1);
        let receipt = sent
            .with_timeout(Some(Duration::from_secs(600)))
            .get_receipt()
            .await
            .with_context(|| {
                format!("{what} on {label}: tx {hash} sent, but no receipt within 10 minutes")
            })?;
        if !receipt.status() {
            bail!(
                "{what} on {label}: tx {hash} reverted in block {:?}",
                receipt.block_number
            );
        }
        Ok(receipt)
    }

    pub async fn has_code(&self, addr: Address) -> Result<bool> {
        let code = self
            .p
            .get_code_at(addr)
            .await
            .with_context(|| format!("reading the code at {addr} on {}", self.chain.label))?;
        Ok(!code.is_empty())
    }

    /// A node behind a load balancer can answer the next read from a replica that has not seen
    /// the block yet, so a fresh deployment is given a little time to appear.
    pub async fn wait_for_code(&self, what: &str, addr: Address) -> Result<()> {
        for _ in 0..20 {
            if self.has_code(addr).await? {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
        }
        bail!(
            "{what}: no code at {addr} on {} a minute after it was deployed",
            self.chain.label
        )
    }
}

/// Make a revert legible: decode `Error(string)` and name the errors an operator can act on.
pub fn explain_revert(raw: &str) -> String {
    let hints = [
        (
            "0x82b42900",
            "Unauthorized: the sending key lacks the role or ownership this needs",
        ),
        (
            "0x118cdaa7",
            "OwnableUnauthorizedAccount: the sender is not the owner",
        ),
        (
            "FORBIDDEN",
            "the storage contract does not trust this DAO: its owner must grantDao it",
        ),
        (
            "xpired",
            "an Intel artifact or its issuer is outside its validity window",
        ),
        ("insufficient funds", "the signing account cannot pay for gas"),
        (
            "nonce too low",
            "another transaction from this account landed first; re-run",
        ),
    ];
    let mut out = match crate::shared::revert_names::name_in(raw) {
        Some(name) => format!("{name} ({raw})"),
        None => raw.to_string(),
    };
    if let Some(i) = raw.find("0x08c379a0") {
        let hex_part: String = raw[i + 10..]
            .chars()
            .take_while(|c| c.is_ascii_hexdigit())
            .collect();
        if let Ok(bytes) = hex::decode(&hex_part) {
            if bytes.len() >= 64 {
                let len = U256::from_be_slice(&bytes[32..64])
                    .to::<usize>()
                    .min(bytes.len().saturating_sub(64));
                if let Ok(msg) = std::str::from_utf8(&bytes[64..64 + len]) {
                    out = format!("{msg} ({raw})");
                }
            }
        }
    }
    for (needle, hint) in hints {
        if raw.contains(needle) {
            out.push_str(&format!(" [{hint}]"));
        }
    }
    out
}

/// A compiled contract from a Foundry `out/` directory.
pub struct Artifact {
    pub bytecode: Vec<u8>,
}

impl Artifact {
    /// `out/<file>.sol/<name>.json` under `project`.
    pub fn load(project: &Path, file: &str, name: &str) -> Result<Self> {
        let path = project
            .join("out")
            .join(format!("{file}.sol"))
            .join(format!("{name}.json"));
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("no compiled {name} at {}; run `teeism build`", path.display()))?;
        let v: serde_json::Value =
            serde_json::from_str(&raw).with_context(|| format!("{} is not JSON", path.display()))?;
        let hex_code = v["bytecode"]["object"]
            .as_str()
            .with_context(|| format!("{} has no bytecode", path.display()))?;
        if hex_code.contains("__$") {
            bail!("{name} links an external library, which this deployer does not support");
        }
        let bytecode = hex::decode(hex_code.trim_start_matches("0x"))
            .with_context(|| format!("{name}'s bytecode in {} is not hex", path.display()))?;
        if bytecode.is_empty() {
            bail!(
                "{name} in {} has empty bytecode (an interface or abstract contract?)",
                path.display()
            );
        }
        Ok(Self { bytecode })
    }

    /// Creation code followed by ABI-encoded constructor arguments.
    pub fn initcode(&self, args: &[u8]) -> Vec<u8> {
        let mut code = self.bytecode.clone();
        code.extend_from_slice(args);
        code
    }
}

/// CREATE2 salt for `name` under a namespace: `keccak256("<namespace>:<name>")`, the same
/// scheme Automata's scripts use with their own prefix.
pub fn salt(namespace: &str, name: &str) -> B256 {
    keccak256(format!("{namespace}:{name}"))
}

pub fn fmt_eth(wei: U256) -> String {
    let s = alloy::primitives::utils::format_ether(wei);
    match s.find('.') {
        Some(i) if s.len() > i + 7 => s[..i + 7].to_string(),
        _ => s,
    }
}
