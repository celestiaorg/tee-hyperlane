//! The ISMs: a TeeDcapIsm on each EVM chain admitting Celestia-origin messages (`isms evm`),
//! and an x/teeism ISM on Celestia per EVM origin behind one routing ISM (`isms celestia`).
//!
//! On an identity change each ISM is re-pinned in place by its owner, keeping its address, its
//! trusted state and the routers that point at it. Only an ISM this key does not own, or an
//! EVM contract revision change, gets a replacement, which starts from the old ISM's last state
//! (so nothing in flight is lost) and never falls back to the chain head. Only a first deploy
//! anchors at the head.

use alloy::primitives::{Address, Bytes, FixedBytes, U256};
use alloy::sol;
use alloy::sol_types::{SolCall, SolValue};
use anyhow::{bail, Context, Result};
use base64::Engine;
use std::str::FromStr;

use crate::celestia::{self, Appd};
use crate::chains::{self, EvmChain};
use crate::cmd::Incomplete;
use crate::config;
use crate::ctx::Ctx;
use crate::enclaves;
use crate::evm::{self, Artifact, Chain};
use crate::out;
use crate::run::Cmd;

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
const MAX_QUOTE_SKEW: u64 = 86400;
const STATE_BYTES: usize = 116;
const IDENTITY_OFFSET: usize = 84;

pub async fn pinned_measurements(rpc: &str, ism: Address) -> Result<String> {
    let r = evm::reader(rpc)?;
    let m = ITeeDcapIsm::new(ism, &r).enclaveMeasurements().call().await.with_context(|| format!("reading what {ism} pins"))?;
    Ok(format!("{m}"))
}

/// The TeeDcapIsm revision this checkout deploys, read from its source.
fn revision(ctx: &Ctx) -> Result<u8> {
    let src = std::fs::read_to_string(ctx.contracts_dir().join("src/TeeDcapIsm.sol"))?;
    src.lines()
        .find_map(|l| l.trim().strip_prefix("uint8 public constant VERSION = ").map(|v| v.trim_end_matches(';').trim().parse()))
        .context("no `uint8 public constant VERSION` in TeeDcapIsm.sol")?
        .context("TeeDcapIsm.sol's VERSION is not a number")
}

fn b32(s: &str) -> Result<FixedBytes<32>> {
    let s = if s.starts_with("0x") { s.to_string() } else { format!("0x{s}") };
    FixedBytes::from_str(&s).with_context(|| format!("{s} is not 32 bytes of hex"))
}

/// The genesis state a new ISM starts from, at the origin's current head.
fn genesis_at_head(ctx: &Ctx, origin: &str, identity: &str) -> Result<Vec<u8>> {
    let out = Cmd::new(format!("bootstrapping a trusted {origin} checkpoint"), ctx.coprocessor_bin())
        .arg("--config")
        .arg(ctx.coprocessor_config())
        .args(["genesis", "--chain", origin, "--identity", identity])
        .output()?;
    let state = hex::decode(out.trim().trim_start_matches("0x")).with_context(|| format!("the genesis printed for {origin} is not hex: {out}"))?;
    if state.len() != STATE_BYTES {
        bail!("the genesis for {origin} is {} bytes, not {STATE_BYTES}", state.len());
    }
    Ok(state)
}

/// An old ISM's last state with only the identity swapped: the route resumes where it stopped.
fn resume_from(old: &[u8], identity: &str) -> Result<Vec<u8>> {
    if old.len() != STATE_BYTES {
        bail!("the old ISM's state is {} bytes, not {STATE_BYTES}; refusing to anchor at the head and lose messages", old.len());
    }
    let mut s = old[..IDENTITY_OFFSET].to_vec();
    s.extend_from_slice(b32(identity)?.as_slice());
    Ok(s)
}

fn celestia_enclave(ctx: &Ctx) -> Result<(String, String)> {
    let d = ctx.deployment()?;
    let e = d.enclaves.chains.get("celestia").context("no celestia enclave; run `teeism enclaves up`")?;
    let m = e.measurements.clone().context("the celestia enclave's measurements are not recorded; run `teeism enclaves up`")?;
    let i = e.identity_digest.clone().context("the celestia enclave's identity is not recorded; run `teeism enclaves up`")?;
    Ok((m, i))
}

/// `teeism isms evm`
pub async fn evm(ctx: &Ctx, selection: &[String]) -> Result<()> {
    let rev = revision(ctx)?;
    // An EVM destination verifies a Celestia-origin attestation, so these pin the Celestia enclave.
    let (measurements, identity) = celestia_enclave(ctx)?;
    out::kv("measurements", &measurements);
    out::kv("identity", &identity);
    let hook = ctx.deployment()?.celestia.merkle_hook.context("no Celestia merkle tree hook; run `teeism celestia core`")?;
    config::write(ctx)?;
    let mut head_genesis: Option<Vec<u8>> = None;

    for chain in chains::evm_selection(selection)? {
        out::step(format!("TeeDcapIsm on {}", chain.label));
        let c = Chain::connect(ctx, chain).await?;
        let mut d = ctx.deployment()?;
        let entry = crate::pccs::pccs_for(&d, chain)?.entrypoint;
        let mailbox = mailbox_of(&d, chain)?;
        let mut genesis = None;

        if let Some(existing) = d.evm(chain.name).and_then(|e| e.ism) {
            let ism = ITeeDcapIsm::new(existing, &c.p);
            let cur = format!("{}", ism.enclaveMeasurements().call().await.with_context(|| format!("reading {existing}"))?);
            // The first deployments have no VERSION(), and the call reverts: revision 1.
            let cur_rev = ism.VERSION().call().await.unwrap_or(1);
            // The verifier is fixed at construction, so a new PCCS deployment needs a new ISM.
            let cur_dcap = ism.dcap().call().await.with_context(|| format!("reading {existing}'s verifier"))?;
            if cur == measurements && cur_rev == rev && cur_dcap == entry {
                out::info(format!("{existing} already pins this enclave"));
                continue;
            }
            if cur_dcap != entry {
                out::info(format!("{existing} verifies through {cur_dcap}, not this PCCS's entrypoint {entry}; replacing it"));
            } else if cur_rev == rev && rev >= 3 {
                let owner = ism.owner().call().await.with_context(|| format!("reading {existing}'s owner"))?;
                if owner == c.from {
                    out::info(format!("re-pinning {existing}: {cur} -> {measurements}"));
                    let data = ITeeDcapIsm::setEnclaveCall { _enclaveMeasurements: b32(&measurements)?, _identityDigest: b32(&identity)? }.abi_encode();
                    c.send("setEnclave", existing, data.into()).await?;
                    let got = format!("{}", ism.enclaveMeasurements().call().await?);
                    if got != measurements {
                        bail!("{existing} pins {got} after the re-pin, expected {measurements}");
                    }
                    out::ok(format!("{existing} re-pinned"));
                    continue;
                }
                out::warn(format!("{existing} is owned by {owner}, not {}; replacing it (its owner should re-pin it instead)", c.from));
            } else {
                out::info(format!("{existing} is contract revision {cur_rev}; replacing it with revision {rev}"));
            }
            let old = ism.state().call().await.with_context(|| format!("reading {existing}'s state"))?;
            genesis = Some(resume_from(&old, &identity)?);
            out::info(format!("resuming from {existing}'s last state"));
        }
        let genesis = match genesis {
            Some(g) => g,
            None => {
                if head_genesis.is_none() {
                    head_genesis = Some(genesis_at_head(ctx, "celestia", &identity)?);
                }
                head_genesis.clone().unwrap_or_default()
            }
        };

        let art = Artifact::load(&ctx.contracts_dir(), "TeeDcapIsm", "TeeDcapIsm")?;
        let args = (entry, b32(&measurements)?, b32(&identity)?, b32(&hook)?, mailbox, Bytes::from(genesis), U256::from(MAX_QUOTE_SKEW)).abi_encode_params();
        let ism = c.deploy("deploying TeeDcapIsm", art.initcode(&args).into()).await?;
        let got = pinned_measurements(&c.rpc, ism).await?;
        if got != measurements {
            bail!("the new ISM {ism} pins {got}, expected {measurements}");
        }
        d.evm_mut(chain.name).ism = Some(ism);
        ctx.save(&d)?;
        out::ok(format!("{ism}"));

        // A mailbox of our own (Eden) takes the first ISM as its default.
        if chain.canonical.is_none() {
            let mb = IMailbox::new(mailbox, &c.p);
            if mb.defaultIsm().call().await.with_context(|| format!("reading {mailbox}'s default ISM"))? == Address::ZERO {
                let hook_addr = d.evm(chain.name).and_then(|e| e.merkle_hook).context("no merkle tree hook recorded")?;
                let data = IMailbox::initializeCall { _owner: c.from, _defaultIsm: ism, _defaultHook: hook_addr, _requiredHook: hook_addr }.abi_encode();
                c.send("initializing the mailbox", mailbox, data.into()).await?;
                out::info(format!("mailbox {mailbox} initialized: default ISM {ism}, hooks {hook_addr}"));
            }
        }
    }
    Ok(())
}

pub fn mailbox_of(d: &crate::state::Deployment, chain: &EvmChain) -> Result<Address> {
    match chain.canonical {
        Some(h) => Ok(h.mailbox),
        None => d
            .evm(chain.name)
            .and_then(|e| e.mailbox)
            .with_context(|| format!("{} has no mailbox yet; run `teeism eden core`", chain.label)),
    }
}

pub fn merkle_hook_of(d: &crate::state::Deployment, chain: &EvmChain) -> Result<Address> {
    match chain.canonical {
        Some(h) => Ok(h.merkle_hook),
        None => d
            .evm(chain.name)
            .and_then(|e| e.merkle_hook)
            .with_context(|| format!("{} has no merkle tree hook yet; run `teeism eden core`", chain.label)),
    }
}

struct CelestiaIsm {
    state: Vec<u8>,
    owner: String,
}

fn query_ism(appd: &Appd, id: &str) -> Result<CelestiaIsm> {
    let v = appd.query(&format!("reading ISM {id}"), &["teeism", "ism", id])?;
    let state = base64::engine::general_purpose::STANDARD
        .decode(v["ism"]["state"].as_str().with_context(|| format!("ISM {id} has no state: {v}"))?)
        .with_context(|| format!("ISM {id}'s state is not base64"))?;
    Ok(CelestiaIsm { state, owner: v["ism"]["owner"].as_str().unwrap_or("").to_string() })
}

/// `teeism isms celestia`
pub async fn celestia(ctx: &Ctx, selection: &[String]) -> Result<()> {
    celestia::wait_for_chain(ctx).await?;
    let appd = Appd::new(ctx)?;
    config::write(ctx)?;
    let relayer = appd.address("relayer")?;
    let mut incomplete = Vec::new();

    for chain in chains::evm_selection(selection)? {
        let enclave = chains::enclave_for(chain.name);
        out::step(format!("{} origin ({enclave} enclave)", chain.label));
        let mut d = ctx.deployment()?;
        let identity = d
            .enclaves
            .chains
            .get(enclave)
            .and_then(|e| e.identity_digest.clone())
            .with_context(|| format!("no identity for the {enclave} enclave; run `teeism enclaves up`"))?;
        let identity_file = enclaves::identity_file(ctx, enclave);
        let want = hex::encode(b32(&identity)?);
        let mut genesis = None;

        if let Some(existing) = d.celestia.isms.get(chain.name).cloned() {
            let ism = query_ism(&appd, &existing)?;
            let pinned = hex::encode(&ism.state[ism.state.len().saturating_sub(32)..]);
            if pinned == want {
                out::info(format!("{existing} already pins this enclave"));
                continue;
            }
            if ism.owner == relayer {
                out::info(format!("re-pinning {existing}: 0x{pinned} -> 0x{want}"));
                appd.tx(
                    &format!("re-pinning the {} ISM", chain.name),
                    "relayer",
                    &["teeism", "update-identity", &existing, &identity_file.to_string_lossy()],
                )?;
                let after = query_ism(&appd, &existing)?;
                if hex::encode(&after.state[after.state.len() - 32..]) != want {
                    bail!("{existing} does not pin the new identity after the re-pin");
                }
                out::ok(format!("{existing} re-pinned"));
                continue;
            }
            out::warn(format!("{existing} is owned by {}, not the relayer {relayer}; replacing it from its last state", ism.owner));
            genesis = Some(resume_from(&ism.state, &identity)?);
        }

        // A bootstrap that fails (Eden before the mocha light node has synced, Base without an
        // archive key) leaves this origin for the next run, rather than stopping the others.
        let genesis = match genesis {
            Some(g) => g,
            None => match genesis_at_head(ctx, chain.name, &identity) {
                Ok(g) => g,
                Err(e) => {
                    out::warn(format!("{}: {e:#}", chain.name));
                    incomplete.push(format!("{}: could not build its genesis state (the reason is above)", chain.name));
                    continue;
                }
            },
        };
        let identity_json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&identity_file).with_context(|| format!("reading {}", identity_file.display()))?,
        )?;
        let tree = celestia::pad32(&merkle_hook_of(&d, chain)?);
        let origin = serde_json::json!({ "state": format!("0x{}", hex::encode(&genesis)), "merkle_tree_address": tree, "identity": identity_json });
        let file = ctx.state_dir.join("identity").join(format!("ism-{}-origin.json", chain.name));
        std::fs::write(&file, serde_json::to_string_pretty(&origin)?)?;
        let tx = appd.tx(&format!("creating the {} ISM", chain.name), "relayer", &["teeism", "create", &file.to_string_lossy()])?;
        let id = tx
            .find_attr(|t| t.contains("teeism"), "id")
            .with_context(|| format!("tx {} created no teeism ISM id", tx.hash))?;
        d.celestia.isms.insert(chain.name.to_string(), id.clone());
        ctx.save(&d)?;
        out::ok(id);
    }

    // Four origins deliver into one token, and each ISM admits one origin domain, so the tokens
    // and the mailbox point at a routing ISM that sends each domain to its own.
    out::step("routing ISM");
    let mut d = ctx.deployment()?;
    if d.celestia.routing_ism.is_none() {
        let tx = appd.tx("creating the routing ISM", "relayer", &["hyperlane", "ism", "create-routing"])?;
        let id = tx
            .find_attr(|t| t.to_lowercase().contains("routing"), "ism_id")
            .or_else(|| tx.find_attr(|t| t.to_lowercase().contains("routing"), "id"))
            .with_context(|| format!("tx {} created no routing ISM id", tx.hash))?;
        d.celestia.routing_ism = Some(id);
        ctx.save(&d)?;
    }
    let routing = d.celestia.routing_ism.clone().unwrap_or_default();
    out::kv("routing ism", &routing);
    let current = appd.query("reading the routing ISM", &["hyperlane", "ism", "ism", &routing]).unwrap_or_default();
    let current = current.to_string();
    for chain in chains::evm_selection(selection)? {
        let Some(ism) = d.celestia.isms.get(chain.name) else { continue };
        // Already routed: the domain and this ISM id appear together in the routing ISM.
        if current.contains(&ism.to_lowercase()) || current.contains(ism.as_str()) {
            out::info(format!("domain {} -> {ism}", chain.domain));
            continue;
        }
        // Remove, then set: a set on a domain already present has been seen to succeed and
        // change nothing. Removing an absent domain is a no-op.
        appd.tx(&format!("clearing domain {}", chain.domain), "relayer", &["hyperlane", "ism", "remove-routing-ism-domain", &routing, &chain.domain.to_string()])?;
        appd.tx(&format!("routing domain {} to {ism}", chain.domain), "relayer", &["hyperlane", "ism", "set-routing-ism-domain", &routing, &chain.domain.to_string(), ism])?;
        out::info(format!("domain {} -> {ism}", chain.domain));
    }

    out::step("pointing the tokens and the mailbox at the routing ISM");
    for (name, token) in [("TIA", &d.celestia.tia_token), ("teeUSD", &d.celestia.teeusd_token)] {
        let Some(token) = token else { continue };
        let t = appd.query(&format!("reading the {name} token"), &["warp", "token", token])?;
        if t["token"]["ism_id"].as_str() != Some(routing.as_str()) {
            appd.tx(&format!("pointing {name} at the routing ISM"), "relayer", &["warp", "set-token", token, "--ism-id", &routing])?;
        }
        out::info(format!("{name} {token}"));
    }
    let mailbox = d.celestia.mailbox.clone().context("no mailbox; run `teeism celestia core`")?;
    let m = appd.query("reading the mailbox", &["hyperlane", "mailbox", &mailbox])?;
    if m["mailbox"]["default_ism"].as_str() != Some(routing.as_str()) {
        appd.tx("pointing the mailbox at the routing ISM", "relayer", &["hyperlane", "mailbox", "set", &mailbox, "--default-ism", &routing])?;
    }
    out::info(format!("mailbox {mailbox}"));
    if !incomplete.is_empty() {
        return Err(Incomplete(incomplete).into());
    }
    Ok(())
}
