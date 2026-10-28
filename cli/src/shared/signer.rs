//! Who signs EVM transactions. The only place EVM_PRIVATE_KEY and EVM_SIGNER are read.
//!
//! Nothing here signs by itself: a `Signer` is handed to alloy as a wallet, and alloy signs
//! each transaction as it sends it.

use alloy::network::EthereumWallet;
use alloy::primitives::Address;
use alloy::signers::local::PrivateKeySigner;
use anyhow::{bail, Context, Result};

use crate::shared::settings::Ctx;

pub enum Signer {
    /// EVM_PRIVATE_KEY: the hot key on the host.
    Key(PrivateKeySigner),
    /// A Ledger (EVM_SIGNER=ledger), for owner transactions.
    #[cfg(feature = "ledger")]
    Ledger(alloy::signers::ledger::LedgerSigner),
}

impl Signer {
    /// Who owns the contracts and signs owner transactions: the hot key, or a Ledger with
    /// EVM_SIGNER=ledger.
    pub async fn owner(ctx: &Ctx) -> Result<Self> {
        match ctx.var_or("EVM_SIGNER", "key").as_str() {
            "key" => Self::hot_key(ctx),
            "ledger" => ledger(ctx).await,
            other => bail!("EVM_SIGNER must be `key` or `ledger`, not `{other}`"),
        }
    }

    /// The hot key, whatever EVM_SIGNER says: it relays and seeds collateral, which run
    /// unattended even when a Ledger owns the contracts.
    pub fn hot_key(ctx: &Ctx) -> Result<Self> {
        let key = ctx.require(
            "EVM_PRIVATE_KEY",
            "it relays, seeds collateral, and signs owner transactions unless EVM_SIGNER=ledger",
        )?;
        let s: PrivateKeySigner = key
            .trim()
            .trim_start_matches("0x")
            .parse()
            .context("EVM_PRIVATE_KEY is not a valid secp256k1 private key")?;
        Ok(Self::Key(s))
    }

    pub fn address(&self) -> Address {
        match self {
            Self::Key(s) => s.address(),
            #[cfg(feature = "ledger")]
            Self::Ledger(s) => alloy::signers::Signer::address(s),
        }
    }

    /// What alloy signs with when a transaction is sent.
    pub fn into_wallet(self) -> EthereumWallet {
        match self {
            Self::Key(s) => EthereumWallet::from(s),
            #[cfg(feature = "ledger")]
            Self::Ledger(s) => EthereumWallet::from(s),
        }
    }
}

#[cfg(feature = "ledger")]
async fn ledger(ctx: &Ctx) -> Result<Signer> {
    use alloy::signers::ledger::{HDPath, LedgerSigner};
    let path = match ctx.var("EVM_LEDGER_PATH") {
        Some(p) => HDPath::Other(p),
        None => HDPath::LedgerLive(0),
    };
    let s = LedgerSigner::new(path, None).await.context(
        "could not open the Ledger; is it unlocked, with the Ethereum app open and blind signing on?",
    )?;
    Ok(Signer::Ledger(s))
}

#[cfg(not(feature = "ledger"))]
async fn ledger(_ctx: &Ctx) -> Result<Signer> {
    bail!("EVM_SIGNER=ledger needs a build with Ledger support: cargo install --locked --path cli --features ledger")
}
