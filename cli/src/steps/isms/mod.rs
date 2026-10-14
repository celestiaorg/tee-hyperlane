//! The ISMs, one file per subcommand: `evm` (a TeeDcapIsm on each EVM chain admitting
//! Celestia-origin messages) and `celestia` (an x/teeism ISM per EVM origin behind one routing
//! ISM). This file holds what they share.
//!
//! On an identity change each ISM is re-pinned in place by its owner, keeping its address, its
//! trusted state and the routers that point at it. Only an ISM this key does not own, or an
//! EVM contract revision change, gets a replacement, which starts from the old ISM's last state
//! (so nothing in flight is lost) and never falls back to the chain head. Only a first deploy
//! anchors at the head.

use alloy::primitives::{Address, FixedBytes};
use alloy::sol;
use anyhow::{bail, Context, Result};
use std::str::FromStr;

use crate::shared::chains::EvmChain;
use crate::shared::settings::Ctx;
use crate::shared::shell::Cmd;

pub mod celestia;
pub mod evm;

sol! {
    #[sol(rpc)]
    interface ITeeDcapIsm {
        function enclaveMeasurements() external view returns (bytes32);
        function dcap() external view returns (address);
        function identityDigest() external view returns (bytes32);
        function state() external view returns (bytes memory);
        function owner() external view returns (address);
        function VERSION() external view returns (uint8);
        function setEnclave(bytes32 _enclaveMeasurements, bytes32 _identityDigest) external;
        function transferOwnership(address newOwner) external;
    }

    #[sol(rpc)]
    interface IMailbox {
        function initialize(address _owner, address _defaultIsm, address _defaultHook, address _requiredHook) external;
        function defaultIsm() external view returns (address);
    }
}

/// How far a quote's timestamp may sit from the block it is checked in.
pub(super) const MAX_QUOTE_SKEW: u64 = 86400;
const STATE_BYTES: usize = 116;
const IDENTITY_OFFSET: usize = 84;

pub async fn pinned_measurements(rpc: &str, ism: Address) -> Result<String> {
    let r = crate::shared::evm::reader(rpc)?;
    let m = ITeeDcapIsm::new(ism, &r)
        .enclaveMeasurements()
        .call()
        .await
        .with_context(|| format!("reading what {ism} pins"))?;
    Ok(format!("{m}"))
}

/// The TeeDcapIsm revision this checkout deploys, read from its source.
pub(super) fn revision(ctx: &Ctx) -> Result<u8> {
    let src = std::fs::read_to_string(ctx.contracts_dir().join("src/TeeDcapIsm.sol"))?;
    src.lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("uint8 public constant VERSION = ")
                .map(|v| v.trim_end_matches(';').trim().parse())
        })
        .context("no `uint8 public constant VERSION` in TeeDcapIsm.sol")?
        .context("TeeDcapIsm.sol's VERSION is not a number")
}

pub(super) fn b32(s: &str) -> Result<FixedBytes<32>> {
    let s = if s.starts_with("0x") {
        s.to_string()
    } else {
        format!("0x{s}")
    };
    FixedBytes::from_str(&s).with_context(|| format!("{s} is not 32 bytes of hex"))
}

/// The genesis state a new ISM starts from, at the origin's current head.
pub(super) fn genesis_at_head(ctx: &Ctx, origin: &str, identity: &str) -> Result<Vec<u8>> {
    let out = Cmd::new(
        format!("bootstrapping a trusted {origin} checkpoint"),
        ctx.coprocessor_bin(),
    )
    .arg("--config")
    .arg(ctx.coprocessor_config())
    .args(["genesis", "--chain", origin, "--identity", identity])
    .output()?;
    let state = hex::decode(out.trim().trim_start_matches("0x"))
        .with_context(|| format!("the genesis printed for {origin} is not hex: {out}"))?;
    if state.len() != STATE_BYTES {
        bail!(
            "the genesis for {origin} is {} bytes, not {STATE_BYTES}",
            state.len()
        );
    }
    Ok(state)
}

/// An old ISM's last state with only the identity swapped: the route resumes where it stopped.
pub(super) fn resume_from(old: &[u8], identity: &str) -> Result<Vec<u8>> {
    if old.len() != STATE_BYTES {
        bail!("the old ISM's state is {} bytes, not {STATE_BYTES}; refusing to anchor at the head and lose messages", old.len());
    }
    let mut s = old[..IDENTITY_OFFSET].to_vec();
    s.extend_from_slice(b32(identity)?.as_slice());
    Ok(s)
}

pub fn mailbox_of(d: &crate::shared::record::Deployment, chain: &EvmChain) -> Result<Address> {
    match chain.canonical {
        Some(h) => Ok(h.mailbox),
        None => d
            .evm(chain.name)
            .and_then(|e| e.mailbox)
            .with_context(|| format!("{} has no mailbox yet; run `teeism eden core`", chain.label)),
    }
}

pub fn merkle_hook_of(d: &crate::shared::record::Deployment, chain: &EvmChain) -> Result<Address> {
    match chain.canonical {
        Some(h) => Ok(h.merkle_hook),
        None => d.evm(chain.name).and_then(|e| e.merkle_hook).with_context(|| {
            format!(
                "{} has no merkle tree hook yet; run `teeism eden core`",
                chain.label
            )
        }),
    }
}
