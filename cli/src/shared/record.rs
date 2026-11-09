//! The deployment record, devnet/.state/deployment.json: every id this deployment created.
//!
//! One typed document rather than a file per value, so what exists is readable in one place
//! and a missing piece is a `None` with a name, not a missing file.

use std::collections::BTreeMap;

use alloy::primitives::Address;
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Deployment {
    #[serde(default)]
    pub celestia: Celestia,
    #[serde(default)]
    pub enclaves: Enclaves,
    /// Per EVM chain, by name.
    #[serde(default)]
    pub evm: BTreeMap<String, Evm>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Celestia {
    /// bech32 addresses of the genesis keys.
    pub relayer: Option<String>,
    pub user: Option<String>,
    /// Receives the smoke test's transfers back from the EVM chains, and nothing else, so its
    /// balance moves only when one lands.
    pub smoke: Option<String>,
    /// Owns the paymaster and posts its gas prices (the gas oracle signs as this key).
    pub bridge: Option<String>,

    pub noop_ism: Option<String>,
    pub mailbox: Option<String>,
    pub merkle_hook: Option<String>,
    pub noop_hook: Option<String>,
    pub igp: Option<String>,
    pub routing_ism: Option<String>,

    pub tia_token: Option<String>,
    pub teeusd_token: Option<String>,
    /// The ISM for each EVM origin, by origin chain name.
    pub isms: BTreeMap<String, String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Enclaves {
    /// Phala CVMs by slot. A slot is a target (`all` for the shared CVM, or a chain deployed
    /// alone), or `<target>#<n>` for an extra CVM `teeism enclaves up --add` started next to it.
    pub cvms: BTreeMap<String, Cvm>,
    /// Per enclave chain: every enclave serving it, where each answers and the identity ISMs
    /// pin for it. ISMs pin all of them; any one may attest.
    #[serde(deserialize_with = "one_or_many")]
    pub chains: BTreeMap<String, Vec<Enclave>>,
    /// CVMs a newer one replaced. They keep running, and billing, until deleted
    /// (`teeism enclaves delete --retired`), which is done once the new one is verified.
    pub retired: Vec<Cvm>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cvm {
    pub name: String,
    pub app_id: String,
    /// The dstack OS image it runs, which mr_td measures.
    #[serde(default)]
    pub os_image: Option<String>,
    /// The Phala instance type, whose vCPU and memory shape the measurements cover.
    #[serde(default)]
    pub instance_type: Option<String>,
    /// The gateway domain of the node Phala placed it on; its enclaves answer under it.
    #[serde(default)]
    pub gateway: Option<String>,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Enclave {
    pub url: String,
    /// The CVM slot it runs in: a key of `Enclaves::cvms`.
    pub target: String,
    /// What an x/teeism ISM pins: the digest of the identity read from the quote.
    pub identity_digest: Option<String>,
    /// What a TeeDcapIsm pins: keccak(mr_td ++ mr_config_id ++ rtmr0..2).
    pub measurements: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Evm {
    pub pccs: Option<Pccs>,
    /// Only on a chain without canonical Hyperlane (Eden): ours.
    pub mailbox: Option<Address>,
    pub merkle_hook: Option<Address>,

    /// The TeeDcapIsm that admits Celestia-origin messages here.
    pub ism: Option<Address>,
    pub tia_router: Option<Address>,
    pub teeusd_router: Option<Address>,
}

/// Our Automata on-chain PCCS and DCAP verifier on one chain. Every address is CREATE2 from
/// `namespace`, so a new namespace is a new, independent deployment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pccs {
    pub namespace: String,
    pub owner: Address,
    pub p256: Address,
    pub enclave_identity_helper: Address,
    pub fmspc_tcb_helper: Address,
    pub pck_helper: Address,
    pub crl_helper: Address,
    pub tcb_eval_helper: Address,
    pub storage: Address,
    pub crl_helper_v2: Address,
    pub dependency_config: Address,
    pub pcs_dao: Address,
    pub pck_dao: Address,
    pub tcb_eval_dao: Address,
    pub router: Address,
    pub entrypoint: Address,
    pub v4_verifier: Address,
    pub v5_verifier: Address,
    /// Per TCB evaluation number: the QE identity and TCB info contracts registered for it.
    #[serde(default)]
    pub evals: BTreeMap<u32, VersionedDaos>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionedDaos {
    pub qe_identity: Address,
    pub fmspc_tcb: Address,
}

impl Deployment {
    pub fn evm_mut(&mut self, chain: &str) -> &mut Evm {
        self.evm.entry(chain.to_string()).or_default()
    }

    pub fn evm(&self, chain: &str) -> Option<&Evm> {
        self.evm.get(chain)
    }

    pub fn teeusd_denom(&self) -> Option<String> {
        self.celestia
            .teeusd_token
            .as_ref()
            .map(|t| format!("hyperlane/{t}"))
    }
}

/// The target a CVM slot runs: `celestia#2` runs `celestia`.
pub fn slot_target(slot: &str) -> &str {
    slot.split('#').next().unwrap_or(slot)
}

/// A record written when each chain had one enclave stores it bare; read it as a list of one.
fn one_or_many<'de, D: serde::Deserializer<'de>>(d: D) -> Result<BTreeMap<String, Vec<Enclave>>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        Many(Vec<Enclave>),
        One(Enclave),
    }
    let raw = BTreeMap::<String, OneOrMany>::deserialize(d)?;
    Ok(raw
        .into_iter()
        .map(|(k, v)| match v {
            OneOrMany::Many(list) => (k, list),
            OneOrMany::One(e) => (k, vec![e]),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A record from when each chain had one enclave loads as lists of one; a new one as lists.
    #[test]
    fn a_bare_enclave_reads_as_a_list_of_one() {
        let old = r#"{"enclaves":{"chains":{"celestia":{"url":"https://a-8080.g","target":"all"}}}}"#;
        let d: Deployment = serde_json::from_str(old).unwrap();
        assert_eq!(d.enclaves.chains["celestia"].len(), 1);
        assert_eq!(d.enclaves.chains["celestia"][0].url, "https://a-8080.g");

        let new = r#"{"enclaves":{"chains":{"celestia":[{"url":"u1","target":"all"},{"url":"u2","target":"celestia#2"}]}}}"#;
        let d: Deployment = serde_json::from_str(new).unwrap();
        assert_eq!(d.enclaves.chains["celestia"].len(), 2);
        assert_eq!(slot_target("celestia#2"), "celestia");
        assert_eq!(slot_target("all"), "all");
    }
}
