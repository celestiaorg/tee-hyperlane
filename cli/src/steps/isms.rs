//! The ISMs that admit attested messages: `teeism isms evm` (a TeeDcapIsm per EVM chain, pinning
//! the Celestia enclave) and `teeism isms celestia` (an x/teeism ISM per EVM origin behind one
//! routing ISM).
//!
//! On an identity change each ISM is re-pinned in place by its owner, keeping its address, its
//! trusted state and the routers that point at it. Only an ISM this key does not own, an EVM
//! contract revision change, or a new PCCS gets a replacement, which starts from the old ISM's
//! last state and never falls back to the chain head. Only a first deploy anchors at the head.

use std::str::FromStr;

use alloy::primitives::{Address, Bytes, FixedBytes, U256};
use alloy::sol;
use alloy::sol_types::{SolCall, SolValue};
use anyhow::{bail, Context, Result};
use base64::Engine;

use crate::shared::chains::EvmChain;
use crate::shared::evm::{Artifact, Chain};
use crate::shared::print::Incomplete;
use crate::shared::settings::Ctx;
use crate::shared::shell::Cmd;
use crate::shared::{chains, print};
use crate::steps::celestia::appd::{self, Appd};
use crate::steps::enclaves;
use crate::steps::services::write_config;

sol! {
    #[sol(rpc)]
    interface ITeeDcapIsm {
        function enclaves() external view returns (bytes32[] memory measurements, bytes32[] memory identities);
        function dcap() external view returns (address);
        function identityDigest() external view returns (bytes32);
        function state() external view returns (bytes memory);
        function owner() external view returns (address);
        function VERSION() external view returns (uint8);
        function mailbox() external view returns (address);
        function merkleTreeAddress() external view returns (bytes32);
        function setEnclaves(bytes32[] calldata measurements, bytes32[] calldata identities) external;
        function transferOwnership(address newOwner) external;
    }

    #[sol(rpc)]
    interface IMailbox {
        function initialize(address _owner, address _defaultIsm, address _defaultHook, address _requiredHook) external;
        function defaultIsm() external view returns (address);
        function setDefaultIsm(address _module) external;
    }
}

/// How far a quote's timestamp may sit from the block it is checked in.
pub(super) const MAX_QUOTE_SKEW: u64 = 86400;
const STATE_BYTES: usize = 116;
const IDENTITY_OFFSET: usize = 84;

/// The measurements a TeeDcapIsm pins, one per enclave.
pub async fn pinned_measurements(rpc: &str, ism: Address) -> Result<Vec<String>> {
    let r = crate::shared::evm::reader(rpc)?;
    let pinned = ITeeDcapIsm::new(ism, &r)
        .enclaves()
        .call()
        .await
        .with_context(|| format!("reading what {ism} pins"))?;
    Ok(pinned.measurements.iter().map(|m| format!("{m}")).collect())
}

/// The most enclaves one ISM pins, as both ISMs bound it.
const MAX_ENCLAVES: usize = 8;

/// The enclaves serving one chain, as the ISMs for its routes pin them: identity digests
/// ascending, each with its TeeDcapIsm measurements and its identity file, and the digest the
/// set is named by. Two CVMs of the same image share an identity and count once.
pub(super) struct EnclaveSet {
    pub identities: Vec<FixedBytes<32>>,
    pub measurements: Vec<FixedBytes<32>>,
    pub files: Vec<std::path::PathBuf>,
    pub digest: FixedBytes<32>,
}

pub(super) fn enclave_set(ctx: &Ctx, enclave: &str) -> Result<EnclaveSet> {
    let d = ctx.deployment()?;
    let list = d
        .enclaves
        .chains
        .get(enclave)
        .filter(|l| !l.is_empty())
        .with_context(|| format!("no {enclave} enclave; run `teeism enclaves up`"))?;
    let mut entries = Vec::new();
    for e in list {
        let what = |field: &str| {
            format!(
                "the {enclave} enclave in {}: its {field} is not recorded; run `teeism enclaves identity --chain {enclave}`",
                e.target
            )
        };
        let id = b32(e.identity_digest.as_deref().with_context(|| what("identity"))?)?;
        let m = b32(e.measurements.as_deref().with_context(|| what("measurements"))?)?;
        entries.push((id, m, enclaves::quote::identity_file(ctx, enclave, &e.target)));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries.dedup_by(|a, b| a.0 == b.0);
    if entries.len() > MAX_ENCLAVES {
        bail!(
            "{} distinct {enclave} enclaves recorded; an ISM pins at most {MAX_ENCLAVES}. Retire some (`teeism enclaves retire <slot>`)",
            entries.len()
        );
    }
    let identities: Vec<_> = entries.iter().map(|e| e.0).collect();
    Ok(EnclaveSet {
        digest: identity_set_digest(&identities),
        measurements: entries.iter().map(|e| e.1).collect(),
        files: entries.into_iter().map(|e| e.2).collect(),
        identities,
    })
}

/// The digest a set of enclave identities is named by, identical to TeeDcapIsm's and
/// x/teeism's: the one digest for a set of one, otherwise
/// sha256("teeism/identity-set/v1" ++ u8(n) ++ the digests ascending). `ids` must be ascending.
pub fn identity_set_digest(ids: &[FixedBytes<32>]) -> FixedBytes<32> {
    use sha2::{Digest, Sha256};
    if ids.len() == 1 {
        return ids[0];
    }
    let mut h = Sha256::new();
    h.update(b"teeism/identity-set/v1");
    h.update([ids.len() as u8]);
    for id in ids {
        h.update(id.as_slice());
    }
    FixedBytes::from_slice(&h.finalize())
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
            .with_context(|| format!("{} has no mailbox yet; run `teeism mailbox`", chain.label)),
    }
}

pub fn merkle_hook_of(d: &crate::shared::record::Deployment, chain: &EvmChain) -> Result<Address> {
    match chain.canonical {
        Some(h) => Ok(h.merkle_hook),
        None => d.evm(chain.name).and_then(|e| e.merkle_hook).with_context(|| {
            format!(
                "{} has no merkle tree hook yet; run `teeism mailbox`",
                chain.label
            )
        }),
    }
}

// ---------------------------------------------------------------- teeism isms evm

/// `teeism isms evm`
pub async fn evm(ctx: &Ctx, selection: &[String]) -> Result<()> {
    let rev = revision(ctx)?;
    // An EVM destination verifies a Celestia-origin attestation, so these pin the Celestia enclaves.
    let set = enclave_set(ctx, "celestia")?;
    for (m, i) in set.measurements.iter().zip(&set.identities) {
        print::kv("enclave", format!("measurements {m}, identity {i}"));
    }
    print::kv("set digest", format!("{}", set.digest));
    let identity = format!("{}", set.digest);
    let hook = ctx
        .deployment()?
        .celestia
        .merkle_hook
        .context("no Celestia merkle tree hook; run `teeism celestia core`")?;
    write_config(ctx)?;
    let mut head_genesis: Option<Vec<u8>> = None;

    for chain in chains::evm_selection(selection)? {
        print::step(format!("TeeDcapIsm on {}", chain.label));
        let c = Chain::connect(ctx, chain).await?;
        let mut d = ctx.deployment()?;
        let entry = crate::steps::pccs::pccs_for(&d, chain)?.entrypoint;
        let mailbox = mailbox_of(&d, chain)?;
        let mut genesis = None;

        if let Some(existing) = d.evm(chain.name).and_then(|e| e.ism) {
            let ism = ITeeDcapIsm::new(existing, &c.p);
            let cur_rev = ism
                .VERSION()
                .call()
                .await
                .with_context(|| format!("reading {existing}'s revision"))?;
            // The verifier is fixed at construction, so a new PCCS deployment needs a new ISM.
            let cur_dcap = ism
                .dcap()
                .call()
                .await
                .with_context(|| format!("reading {existing}'s verifier"))?;
            // The mailbox and the Celestia hook are fixed in the ISM. One built against another
            // run's is never kept: it would leave this deployment wired to that run.
            let wired_mailbox = ism
                .mailbox()
                .call()
                .await
                .with_context(|| format!("reading {existing}'s mailbox"))?;
            let wired_tree = ism
                .merkleTreeAddress()
                .call()
                .await
                .with_context(|| format!("reading {existing}'s merkle tree address"))?;
            if wired_mailbox != mailbox || wired_tree != b32(&hook)? {
                bail!(
                    "the recorded ISM {existing} on {} was built for mailbox {wired_mailbox} and Celestia hook {wired_tree}, \
                     but this deployment uses {mailbox} and {hook}: it belongs to an earlier deployment. \
                     Remove evm.{}.ism from devnet/.state/deployment.json to deploy a new one, or start over with `teeism teardown`",
                    chain.label,
                    chain.name
                );
            }
            // Only an ISM of this revision is asked what it pins: another revision may not have
            // `enclaves()` at all, and is replaced whatever it pins.
            let pinned = if cur_rev == rev {
                let p = ism
                    .enclaves()
                    .call()
                    .await
                    .with_context(|| format!("reading what {existing} pins"))?;
                Some((p.measurements, p.identities))
            } else {
                None
            };
            let same_set = pinned
                .as_ref()
                .is_some_and(|(m, i)| *m == set.measurements && *i == set.identities);
            if same_set && cur_dcap == entry {
                print::info(format!("{existing} already pins these enclaves"));
                own_mailbox(&c, &d, chain, mailbox, existing).await?;
                continue;
            }
            if cur_dcap != entry {
                print::info(format!("{existing} verifies through {cur_dcap}, not this PCCS's entrypoint {entry}; replacing it"));
            } else if let Some((_, cur_ids)) = &pinned {
                let owner = ism
                    .owner()
                    .call()
                    .await
                    .with_context(|| format!("reading {existing}'s owner"))?;
                if owner == c.from {
                    print::info(format!(
                        "re-pinning {existing}: {} -> {} enclaves, set {}",
                        cur_ids.len(),
                        set.identities.len(),
                        set.digest
                    ));
                    let data = ITeeDcapIsm::setEnclavesCall {
                        measurements: set.measurements.clone(),
                        identities: set.identities.clone(),
                    }
                    .abi_encode();
                    c.send("setEnclaves", existing, data.into()).await?;
                    let after = ism.enclaves().call().await?;
                    if after.measurements != set.measurements || after.identities != set.identities {
                        bail!("{existing} does not pin the new set after the re-pin");
                    }
                    print::ok(format!("{existing} re-pinned"));
                    own_mailbox(&c, &d, chain, mailbox, existing).await?;
                    continue;
                }
                print::warn(format!("{existing} is owned by {owner}, not {}; replacing it (its owner should re-pin it instead)", c.from));
            } else {
                print::info(format!(
                    "{existing} is contract revision {cur_rev}; replacing it with revision {rev}"
                ));
            }
            let old = ism
                .state()
                .call()
                .await
                .with_context(|| format!("reading {existing}'s state"))?;
            genesis = Some(resume_from(&old, &identity)?);
            print::info(format!("resuming from {existing}'s last state"));
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
        let args = (
            entry,
            set.measurements.clone(),
            set.identities.clone(),
            b32(&hook)?,
            mailbox,
            Bytes::from(genesis),
            U256::from(MAX_QUOTE_SKEW),
        )
            .abi_encode_params();
        let ism = c
            .deploy("deploying TeeDcapIsm", art.initcode(&args).into())
            .await?;
        let got = ITeeDcapIsm::new(ism, &c.p).enclaves().call().await?;
        if got.measurements != set.measurements || got.identities != set.identities {
            bail!("the new ISM {ism} does not pin the recorded set");
        }
        d.evm_mut(chain.name).ism = Some(ism);
        ctx.save(&d)?;
        print::ok(format!("{ism}"));

        own_mailbox(&c, &d, chain, mailbox, ism).await?;
    }
    Ok(())
}

/// A mailbox of our own (Eden) takes this ISM as its default: initialized with it the first
/// time, and pointed at it whenever the ISM changes, so it never keeps an earlier ISM.
async fn own_mailbox(
    c: &Chain,
    d: &crate::shared::record::Deployment,
    chain: &EvmChain,
    mailbox: Address,
    ism: Address,
) -> Result<()> {
    if chain.canonical.is_some() {
        return Ok(());
    }
    let mb = IMailbox::new(mailbox, &c.p);
    let cur = mb
        .defaultIsm()
        .call()
        .await
        .with_context(|| format!("reading {mailbox}'s default ISM"))?;
    if cur == ism {
        return Ok(());
    }
    if cur == Address::ZERO {
        let hook_addr = d
            .evm(chain.name)
            .and_then(|e| e.merkle_hook)
            .context("no merkle tree hook recorded")?;
        let data = IMailbox::initializeCall {
            _owner: c.from,
            _defaultIsm: ism,
            _defaultHook: hook_addr,
            _requiredHook: hook_addr,
        }
        .abi_encode();
        c.send("initializing the mailbox", mailbox, data.into()).await?;
        print::info(format!(
            "mailbox {mailbox} initialized: default ISM {ism}, hooks {hook_addr}"
        ));
    } else {
        print::info(format!("pointing mailbox {mailbox} at {ism}, not {cur}"));
        let data = IMailbox::setDefaultIsmCall { _module: ism }.abi_encode();
        c.send("setting the mailbox's default ISM", mailbox, data.into())
            .await?;
    }
    let after = mb.defaultIsm().call().await?;
    anyhow::ensure!(
        after == ism,
        "{mailbox} reports default ISM {after}, expected {ism}"
    );
    Ok(())
}

// ---------------------------------------------------------------- teeism isms celestia

struct CelestiaIsm {
    state: Vec<u8>,
    owner: String,
}

fn query_ism(appd: &Appd, id: &str) -> Result<CelestiaIsm> {
    let v = appd.query(&format!("reading ISM {id}"), &["teeism", "ism", id])?;
    let state = base64::engine::general_purpose::STANDARD
        .decode(
            v["ism"]["state"]
                .as_str()
                .with_context(|| format!("ISM {id} has no state: {v}"))?,
        )
        .with_context(|| format!("ISM {id}'s state is not base64"))?;
    Ok(CelestiaIsm {
        state,
        owner: v["ism"]["owner"].as_str().unwrap_or("").to_string(),
    })
}

/// `teeism isms celestia`
pub async fn celestia(ctx: &Ctx, selection: &[String]) -> Result<()> {
    appd::wait_for_chain(ctx).await?;
    let appd = Appd::new(ctx)?;
    write_config(ctx)?;
    let relayer = appd.address("relayer")?;
    let mut incomplete = Vec::new();

    for chain in chains::evm_selection(selection)? {
        let enclave = chains::enclave_for(chain.name);
        print::step(format!("{} origin ({enclave} enclave)", chain.label));
        let mut d = ctx.deployment()?;
        let set = enclave_set(ctx, enclave)?;
        let identity = format!("{}", set.digest);
        let want = hex::encode(set.digest);
        let files: Vec<String> = set
            .files
            .iter()
            .map(|f| f.to_string_lossy().to_string())
            .collect();
        let mut genesis = None;

        if let Some(existing) = d.celestia.isms.get(chain.name).cloned() {
            let ism = query_ism(&appd, &existing)?;
            let pinned = hex::encode(&ism.state[ism.state.len().saturating_sub(32)..]);
            if pinned == want {
                print::info(format!("{existing} already pins these enclaves"));
                continue;
            }
            if ism.owner == relayer {
                print::info(format!(
                    "re-pinning {existing}: 0x{pinned} -> 0x{want} ({} enclaves)",
                    files.len()
                ));
                let mut args = vec!["teeism", "set-identities", existing.as_str()];
                args.extend(files.iter().map(String::as_str));
                appd.tx(&format!("re-pinning the {} ISM", chain.name), "relayer", &args)?;
                let after = query_ism(&appd, &existing)?;
                if hex::encode(&after.state[after.state.len() - 32..]) != want {
                    bail!("{existing} does not pin the new set after the re-pin");
                }
                print::ok(format!("{existing} re-pinned"));
                continue;
            }
            print::warn(format!(
                "{existing} is owned by {}, not the relayer {relayer}; replacing it from its last state",
                ism.owner
            ));
            genesis = Some(resume_from(&ism.state, &identity)?);
        }

        // A bootstrap that fails (Eden before the mocha light node has synced, Base without an
        // archive key) leaves this origin for the next run, rather than stopping the others.
        let genesis = match genesis {
            Some(g) => g,
            None => match genesis_at_head(ctx, chain.name, &identity) {
                Ok(g) => g,
                Err(e) => {
                    print::warn(format!("{}: {e:#}", chain.name));
                    incomplete.push(format!(
                        "{}: could not build its genesis state (the reason is above)",
                        chain.name
                    ));
                    continue;
                }
            },
        };
        let mut identities = Vec::new();
        for f in &set.files {
            let raw = std::fs::read_to_string(f).with_context(|| format!("reading {}", f.display()))?;
            identities.push(serde_json::from_str::<serde_json::Value>(&raw)?);
        }
        let tree = appd::pad32(&merkle_hook_of(&d, chain)?);
        let origin = serde_json::json!({ "state": format!("0x{}", hex::encode(&genesis)), "merkle_tree_address": tree, "identities": identities });
        let file = ctx
            .state_dir
            .join("identity")
            .join(format!("ism-{}-origin.json", chain.name));
        std::fs::write(&file, serde_json::to_string_pretty(&origin)?)?;
        let tx = appd.tx(
            &format!("creating the {} ISM", chain.name),
            "relayer",
            &["teeism", "create", &file.to_string_lossy()],
        )?;
        let id = tx
            .find_attr(|t| t.contains("teeism"), "id")
            .with_context(|| format!("tx {} created no teeism ISM id", tx.hash))?;
        d.celestia.isms.insert(chain.name.to_string(), id.clone());
        ctx.save(&d)?;
        print::ok(id);
    }

    // Four origins deliver into one token, and each ISM admits one origin domain, so the tokens
    // and the mailbox point at a routing ISM that sends each domain to its own.
    print::step("routing ISM");
    let mut d = ctx.deployment()?;
    if d.celestia.routing_ism.is_none() {
        let tx = appd.tx(
            "creating the routing ISM",
            "relayer",
            &["hyperlane", "ism", "create-routing"],
        )?;
        let id = tx
            .find_attr(|t| t.to_lowercase().contains("routing"), "ism_id")
            .or_else(|| tx.find_attr(|t| t.to_lowercase().contains("routing"), "id"))
            .with_context(|| format!("tx {} created no routing ISM id", tx.hash))?;
        d.celestia.routing_ism = Some(id);
        ctx.save(&d)?;
    }
    let routing = d.celestia.routing_ism.clone().unwrap_or_default();
    print::kv("routing ism", &routing);
    let current = appd
        .query("reading the routing ISM", &["hyperlane", "ism", "ism", &routing])
        .unwrap_or_default();
    let current = current.to_string();
    for chain in chains::evm_selection(selection)? {
        let Some(ism) = d.celestia.isms.get(chain.name) else {
            continue;
        };
        // Already routed: the domain and this ISM id appear together in the routing ISM.
        if current.contains(&ism.to_lowercase()) || current.contains(ism.as_str()) {
            print::info(format!("domain {} -> {ism}", chain.domain));
            continue;
        }
        // Remove, then set: a set on a domain already present has been seen to succeed and
        // change nothing. Removing an absent domain is a no-op.
        appd.tx(
            &format!("clearing domain {}", chain.domain),
            "relayer",
            &[
                "hyperlane",
                "ism",
                "remove-routing-ism-domain",
                &routing,
                &chain.domain.to_string(),
            ],
        )?;
        appd.tx(
            &format!("routing domain {} to {ism}", chain.domain),
            "relayer",
            &[
                "hyperlane",
                "ism",
                "set-routing-ism-domain",
                &routing,
                &chain.domain.to_string(),
                ism,
            ],
        )?;
        print::info(format!("domain {} -> {ism}", chain.domain));
    }

    print::step("pointing the tokens and the mailbox at the routing ISM");
    for (name, token) in [
        ("TIA", &d.celestia.tia_token),
        ("teeUSD", &d.celestia.teeusd_token),
    ] {
        let Some(token) = token else { continue };
        let t = appd.query(&format!("reading the {name} token"), &["warp", "token", token])?;
        if t["token"]["ism_id"].as_str() != Some(routing.as_str()) {
            appd.tx(
                &format!("pointing {name} at the routing ISM"),
                "relayer",
                &["warp", "set-token", token, "--ism-id", &routing],
            )?;
        }
        print::info(format!("{name} {token}"));
    }
    let mailbox = d
        .celestia
        .mailbox
        .clone()
        .context("no mailbox; run `teeism celestia core`")?;
    let m = appd.query("reading the mailbox", &["hyperlane", "mailbox", &mailbox])?;
    if m["mailbox"]["default_ism"].as_str() != Some(routing.as_str()) {
        appd.tx(
            "pointing the mailbox at the routing ISM",
            "relayer",
            &["hyperlane", "mailbox", "set", &mailbox, "--default-ism", &routing],
        )?;
    }
    print::info(format!("mailbox {mailbox}"));
    if !incomplete.is_empty() {
        return Err(Incomplete(incomplete).into());
    }
    Ok(())
}

#[cfg(test)]
mod set_tests {
    use super::*;

    /// The vectors TeeDcapIsm's and x/teeism's tests pin: all three name a set alike.
    #[test]
    fn the_set_digest_matches_both_isms() {
        let b = |x: u8| FixedBytes::<32>::from([x; 32]);
        assert_eq!(identity_set_digest(&[b(0x11)]), b(0x11));
        assert_eq!(
            format!("{}", identity_set_digest(&[b(0x11), b(0x22)])),
            "0xec1daf6eea2286feaecc70758f647a8e318b73f711043bffd2d6bcb0a5b41807"
        );
        assert_eq!(
            format!("{}", identity_set_digest(&[b(0x11), b(0x22), b(0x33)])),
            "0x2fa9b2f90d7deb2663ea2c8b1bd32985ddac3c536c93656d3cf92ba14676d77e"
        );
    }
}
