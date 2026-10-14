//! Our Automata on-chain PCCS and DCAP verifier on each EVM chain: deploy, seed with Intel's
//! collateral, report its expiry, and check a quote against it.
//!
//! Deployed from the vendored sources in devnet/automata, through the CREATE2 deployer. Every
//! salt is `keccak256("<namespace>:<NAME>")`, so the addresses follow from the namespace, the
//! owner and the code, and a new namespace is a new, independent deployment. Each piece is
//! checked on chain before it is sent, so a run that stops anywhere is finished by running it
//! again.
//!
//! Layout: the CRL v2 PCS and PCK DAOs (`PcsDaoV2`, `PckDaoV2`, `X509CRLHelperV2`) behind a
//! `PccsDependencyConfig`, the TCB evaluation DAO and the versioned QE identity DAO that read
//! through it, and the versioned TCB info DAO that takes a single `upsertFmspcTcb`. The router
//! the quote verifiers read from points at all of them.

use alloy::primitives::{address, Address, Bytes, FixedBytes, U256};
use alloy::providers::Provider;
use alloy::rpc::types::TransactionRequest;
use alloy::sol;
use alloy::sol_types::{SolCall, SolValue};
use anyhow::{bail, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::chains::EvmChain;
use crate::ctx::Ctx;
use crate::evm::{self, salt, Artifact, Chain};
use crate::out;
use crate::state::{Pccs, VersionedDaos};

pub const DEFAULT_NAMESPACE: &str = "tee-isms/v2";
/// Phala's hosts.
pub const DEFAULT_FMSPC: &str = "20a06f000000";
const ATTESTER_ROLE: u64 = 1;

const RIP7212_P256: Address = address!("0000000000000000000000000000000000000100");
const DAIMO_P256: Address = address!("c2b78104907F722DABAc4C69f826a522B2754De4");

const SGX_API: &str = "https://api.trustedservices.intel.com/sgx/certification/v4";
const TDX_API: &str = "https://api.trustedservices.intel.com/tdx/certification/v4";
const ROOT_CRL_FALLBACK: &str = "https://certificates.trustedservices.intel.com/IntelSGXRootCA.der";

sol! {
    #[sol(rpc)]
    interface IStorage {
        function grantDao(address granted) external;
        function isAuthorizedCaller(address caller) external view returns (bool);
        function setCallerAuthorization(address caller, bool authorized) external;
        function readAttestation(bytes32 attestationId) external view returns (bytes memory);
        function owner() external view returns (address);
    }

    #[sol(rpc)]
    interface IDependencyConfig {
        function initialize(address initialPcsDao, address initialCrlHelper) external;
        function pcsDao() external view returns (address);
        function crlHelper() external view returns (address);
    }

    #[sol(rpc)]
    interface ICrlHelperV2 {
        function authorizedIndexers(address indexer) external view returns (bool);
        function setAuthorizedIndexer(address indexer, bool authorized) external;
    }

    #[sol(rpc)]
    interface IRoles {
        function grantRoles(address user, uint256 roles) external;
        function hasAnyRole(address user, uint256 roles) external view returns (bool);
    }

    #[sol(rpc)]
    interface IVersioned {
        function TCB_EVALUATION_NUMBER() external view returns (uint32);
    }

    #[sol(rpc)]
    interface IRouter {
        function setAuthorized(address caller, bool authorized) external;
        function setQeIdDaoVersionedAddr(uint32 tcbEval, address addr) external;
        function setFmspcTcbDaoVersionedAddr(uint32 tcbEval, address addr) external;
        function qeIdDaoVersionedAddr(uint32 tcbEval) external view returns (address);
        function fmspcTcbDaoVersionedAddr(uint32 tcbEval) external view returns (address);
        function getCertHash(uint8 ca) external view returns (bytes32);
        function owner() external view returns (address);
    }

    #[sol(rpc)]
    interface IEntrypoint {
        function setQuoteVerifier(address verifier) external;
        function quoteVerifiers(uint16 version) external view returns (address);
        function verifyAndAttestOnChain(bytes calldata rawQuote) external payable returns (bool success, bytes memory output);
    }

    struct JsonObj { string body; bytes signature; }

    #[sol(rpc)]
    interface ICollateral {
        function upsertPcsCertificates(uint8 ca, bytes calldata cert) external returns (bytes32);
        function upsertRootCACrl(bytes calldata rootcacrl) external returns (bytes32);
        function upsertPckCrl(uint8 ca, bytes calldata crl) external returns (bytes32);
        function upsertTcbEvaluationData(JsonObj calldata tcbEvalObj) external returns (bytes32);
        function upsertEnclaveIdentity(uint256 id, uint256 version, JsonObj calldata enclaveIdentityObj) external returns (bytes32);
        function upsertFmspcTcb(JsonObj calldata tcbInfoObj) external returns (bytes32);

        function getCollateralValidity(bytes32 key) external view returns (uint64, uint64);
        function PCS_KEY(uint8 ca, bool isCrl) external pure returns (bytes32);
        function TCB_EVAL_KEY(uint8 id) external view returns (bytes32);
        function ENCLAVE_ID_KEY(uint256 id, uint256 version) external view returns (bytes32);
        function FMSPC_TCB_KEY(uint8 tcbType, bytes6 fmspc, uint32 version) external view returns (bytes32);
    }
}

/// CA enum in the PCCS: ROOT=0, PROCESSOR=1, PLATFORM=2, SIGNING=3.
mod ca {
    pub const ROOT: u8 = 0;
    pub const PROCESSOR: u8 = 1;
    pub const PLATFORM: u8 = 2;
    pub const SIGNING: u8 = 3;
}

pub fn pccs_dir(ctx: &Ctx) -> PathBuf {
    ctx.repo.join("devnet/automata/pccs")
}

pub fn dcap_dir(ctx: &Ctx) -> PathBuf {
    ctx.repo.join("devnet/automata/automata/evm")
}

struct Code {
    pccs: PathBuf,
    dcap: PathBuf,
}

impl Code {
    fn pccs(&self, file: &str, name: &str) -> Result<Artifact> {
        Artifact::load(&self.pccs, file, name)
    }
    fn dcap(&self, name: &str) -> Result<Artifact> {
        Artifact::load(&self.dcap, name, name)
    }
}

/// The P-256 verifier the chain offers: the RIP-7212 precompile, else Daimo's contract.
async fn p256(c: &Chain) -> Result<Address> {
    // Automata's own self-test vector: sha256(message), signature, public key.
    let msg = hex::decode("a9b4ac5fb82203536c408b1db1d0338c61fd0064ea2471794d435fc0e03c217f")?;
    let sig = hex::decode("8c6a3bb0346ec08d01b6351eeff099fd7131de48e5e569dbcd9dc3f29e08995692db2eaebd633a52fff4915d274859bbc241967c6ce3a6831e754b88066fc534")?;
    let key = hex::decode("710f9d7cb59f86798aaf92138320831b778016d02cf0f5b416a76917f85edd4d7440615935921eaaa33c66c6cf4b745e70176a391610ab14f845d7ff39b112a3")?;
    use sha2::Digest;
    let mut input = sha2::Sha256::digest(&msg).to_vec();
    input.extend(sig);
    input.extend(key);
    for verifier in [RIP7212_P256, DAIMO_P256] {
        let tx = TransactionRequest::default().to(verifier).input(Bytes::from(input.clone()).into());
        if let Ok(ret) = c.p.call(tx).await {
            if ret.len() == 32 && ret[31] == 1 {
                return Ok(verifier);
            }
        }
    }
    bail!(
        "{} verifies P-256 neither through the RIP-7212 precompile nor Daimo's contract; \
         the DCAP verifier cannot run there (a chain before Osaka has no precompile)",
        c.chain.label
    )
}

/// Whether `dao` may write to `storage`. The storage has no getter for its writers, but
/// `readAttestation` is restricted to them and reverts FORBIDDEN for anyone else.
async fn is_writer(c: &Chain, storage: Address, dao: Address) -> Result<bool> {
    let data = IStorage::readAttestationCall { attestationId: FixedBytes::ZERO }.abi_encode();
    let tx = TransactionRequest::default().from(dao).to(storage).input(Bytes::from(data).into());
    match c.p.call(tx).await {
        Ok(_) => Ok(true),
        Err(e) if e.to_string().contains("FORBIDDEN") => Ok(false),
        Err(e) => Err(anyhow::anyhow!("could not tell whether {dao} may write to the PCCS storage {storage}: {e}")),
    }
}

/// Whether the router serves `caller`. Like the storage, it reverts `Forbidden()` (0xee90c468)
/// for an unauthorized caller and has no getter.
async fn router_serves(c: &Chain, router: Address, caller: Address) -> Result<bool> {
    let data = IRouter::getCertHashCall { ca: ca::ROOT }.abi_encode();
    let tx = TransactionRequest::default().from(caller).to(router).input(Bytes::from(data).into());
    match c.p.call(tx).await {
        Ok(_) => Ok(true),
        Err(e) if e.to_string().contains("ee90c468") => Ok(false),
        // Anything else (a missing certificate, say) means it got past the caller check.
        Err(_) => Ok(true),
    }
}

async fn grant_writer(c: &Chain, storage: Address, dao: Address, name: &str) -> Result<()> {
    if !is_writer(c, storage, dao).await? {
        let data = IStorage::grantDaoCall { granted: dao }.abi_encode();
        c.send(&format!("letting {name} write to the PCCS storage"), storage, data.into()).await?;
    }
    Ok(())
}

async fn grant_attester(c: &Chain, dao: Address, who: Address, name: &str) -> Result<()> {
    let roles = IRoles::new(dao, &c.p);
    let has = roles
        .hasAnyRole(who, U256::from(ATTESTER_ROLE))
        .call()
        .await
        .with_context(|| format!("reading {who}'s role on {name} {dao}"))?;
    if !has {
        let data = IRoles::grantRolesCall { user: who, roles: U256::from(ATTESTER_ROLE) }.abi_encode();
        c.send(&format!("granting {who} ATTESTER_ROLE on {name}"), dao, data.into()).await?;
    }
    Ok(())
}

/// The quote verifier salt Automata derives per version: `keccak256(abi.encodePacked("<ns>:QUOTE_VERIFIER_V", uint16))`.
fn verifier_salt(namespace: &str, version: u16) -> FixedBytes<32> {
    let mut packed = format!("{namespace}:QUOTE_VERIFIER_V").into_bytes();
    packed.extend_from_slice(&version.to_be_bytes());
    alloy::primitives::keccak256(packed)
}

/// Deploy, or finish deploying, the PCCS and DCAP verifier on one chain, with TCB info and QE
/// identity contracts for each of `evals`. `seeder` is granted the role that uploads
/// collateral (it needs nothing else).
pub async fn deploy(
    ctx: &Ctx,
    c: &Chain,
    namespace: &str,
    evals: &BTreeSet<u32>,
    seeder: Address,
) -> Result<Pccs> {
    let code = Code { pccs: pccs_dir(ctx), dcap: dcap_dir(ctx) };
    let ns = namespace;
    let owner = c.from;
    let p256 = p256(c).await?;
    out::info(format!("P-256 verifier {p256}, owner {owner}, namespace {ns}"));

    let helper = |file: &'static str, salt_name: &'static str| {
        let code = &code;
        async move {
            let a = code.pccs(file, file)?;
            c.create2(file, salt(ns, salt_name), &a.initcode(&[])).await
        }
    };
    let enclave_identity_helper = helper("EnclaveIdentityHelper", "ENCLAVE_IDENTITY_HELPER_SALT").await?;
    let fmspc_tcb_helper = helper("FmspcTcbHelper", "FMSPC_TCB_HELPER_SALT").await?;
    let pck_helper = helper("PCKHelper", "X509_HELPER_SALT").await?;
    let crl_helper = helper("X509CRLHelper", "X509_CRL_HELPER_SALT").await?;
    let tcb_eval_helper = helper("TcbEvalHelper", "TCB_EVAL_HELPER_SALT").await?;

    let storage = c
        .create2(
            "AutomataDaoStorage",
            salt(ns, "PCCS_STORAGE_SALT"),
            &code.pccs("AutomataDaoStorage", "AutomataDaoStorage")?.initcode(&owner.abi_encode()),
        )
        .await?;
    let crl_helper_v2 = c
        .create2(
            "X509CRLHelperV2",
            salt(ns, "X509_CRL_HELPER_V2_SALT"),
            &code.pccs("X509CRLHelperV2", "X509CRLHelperV2")?.initcode(&owner.abi_encode()),
        )
        .await?;
    let dependency_config = c
        .create2(
            "PccsDependencyConfig",
            salt(ns, "PCCS_DEPENDENCY_CONFIG_SALT"),
            &code.pccs("PccsDependencyConfig", "PccsDependencyConfig")?.initcode(&owner.abi_encode()),
        )
        .await?;
    let pcs_dao = c
        .create2(
            "AutomataPcsDaoV2",
            salt(ns, "PCS_DAO_V2_SALT"),
            &code
                .pccs("AutomataPcsDaoV2", "AutomataPcsDaoV2")?
                .initcode(&(storage, p256, pck_helper, dependency_config).abi_encode_params()),
        )
        .await?;

    let dep = IDependencyConfig::new(dependency_config, &c.p);
    let configured = dep.pcsDao().call().await.context("reading the dependency config's PCS DAO")?;
    if configured == Address::ZERO {
        let data = IDependencyConfig::initializeCall { initialPcsDao: pcs_dao, initialCrlHelper: crl_helper_v2 }.abi_encode();
        c.send("initializing the PCCS dependency config", dependency_config, data.into()).await?;
    } else {
        let crl = dep.crlHelper().call().await.context("reading the dependency config's CRL helper")?;
        if configured != pcs_dao || crl != crl_helper_v2 {
            bail!(
                "the dependency config {dependency_config} points at PCS DAO {configured} and CRL helper {crl}, \
                 not {pcs_dao} and {crl_helper_v2}; it belongs to another deployment"
            );
        }
    }

    let pck_dao = c
        .create2(
            "AutomataPckDaoV2",
            salt(ns, "PCK_DAO_V2_SALT"),
            &code
                .pccs("AutomataPckDaoV2", "AutomataPckDaoV2")?
                .initcode(&(storage, p256, dependency_config, pck_helper).abi_encode_params()),
        )
        .await?;
    grant_writer(c, storage, pcs_dao, "the PCS DAO").await?;
    grant_writer(c, storage, pck_dao, "the PCK DAO").await?;
    let indexer = ICrlHelperV2::new(crl_helper_v2, &c.p);
    if !indexer.authorizedIndexers(pcs_dao).call().await.context("reading the CRL helper's indexers")? {
        let data = ICrlHelperV2::setAuthorizedIndexerCall { indexer: pcs_dao, authorized: true }.abi_encode();
        c.send("letting the PCS DAO index CRLs", crl_helper_v2, data.into()).await?;
    }

    let tcb_eval_dao = c
        .create2(
            "AutomataTcbEvalDao",
            salt(ns, "TCB_EVAL_DAO_SALT"),
            &code.pccs("AutomataTcbEvalDao", "AutomataTcbEvalDao")?.initcode(
                &(storage, p256, dependency_config, tcb_eval_helper, pck_helper, owner).abi_encode_params(),
            ),
        )
        .await?;
    grant_writer(c, storage, tcb_eval_dao, "the TCB evaluation DAO").await?;
    grant_attester(c, tcb_eval_dao, seeder, "the TCB evaluation DAO").await?;

    let router = c
        .create2(
            "PCCSRouter",
            salt(ns, "PCCS_ROUTER_SALT"),
            &code.dcap("PCCSRouter")?.initcode(
                &(owner, tcb_eval_dao, pcs_dao, pck_dao, pck_helper, crl_helper_v2, fmspc_tcb_helper).abi_encode_params(),
            ),
        )
        .await?;
    let st = IStorage::new(storage, &c.p);
    if !st.isAuthorizedCaller(router).call().await.context("reading the storage's readers")? {
        let data = IStorage::setCallerAuthorizationCall { caller: router, authorized: true }.abi_encode();
        c.send("letting the router read the PCCS storage", storage, data.into()).await?;
    }

    let entrypoint = c
        .create2(
            "AutomataDcapAttestationFee",
            salt(ns, "DCAP_ATTESTATION_SALT"),
            &code.dcap("AutomataDcapAttestationFee")?.initcode(&owner.abi_encode()),
        )
        .await?;

    let mut verifiers = [Address::ZERO; 2];
    for (i, version) in [4u16, 5].into_iter().enumerate() {
        let name = format!("V{version}QuoteVerifier");
        let v = c
            .create2(&name, verifier_salt(ns, version), &code.dcap(&name)?.initcode(&(p256, router).abi_encode_params()))
            .await?;
        if !router_serves(c, router, v).await? {
            let data = IRouter::setAuthorizedCall { caller: v, authorized: true }.abi_encode();
            c.send(&format!("letting {name} read the router"), router, data.into()).await?;
        }
        let current = IEntrypoint::new(entrypoint, &c.p)
            .quoteVerifiers(version)
            .call()
            .await
            .with_context(|| format!("reading the entrypoint's v{version} verifier"))?;
        if current != v {
            let data = IEntrypoint::setQuoteVerifierCall { verifier: v }.abi_encode();
            c.send(&format!("registering {name} on the entrypoint"), entrypoint, data.into()).await?;
        }
        verifiers[i] = v;
    }

    let mut pccs = Pccs {
        namespace: ns.to_string(),
        owner,
        p256,
        enclave_identity_helper,
        fmspc_tcb_helper,
        pck_helper,
        crl_helper,
        tcb_eval_helper,
        storage,
        crl_helper_v2,
        dependency_config,
        pcs_dao,
        pck_dao,
        tcb_eval_dao,
        router,
        entrypoint,
        v4_verifier: verifiers[0],
        v5_verifier: verifiers[1],
        evals: BTreeMap::new(),
    };
    for n in evals {
        let daos = deploy_eval(&code, c, &pccs, *n, seeder).await?;
        pccs.evals.insert(*n, daos);
    }
    Ok(pccs)
}

/// The QE identity and TCB info contracts for one TCB evaluation number, registered on the
/// router. The verifier reads Intel's "standard" number by block time, and moves to the next
/// twelve months after Intel releases it, so the next number needs these before that day.
async fn deploy_eval(code: &Code, c: &Chain, p: &Pccs, n: u32, seeder: Address) -> Result<VersionedDaos> {
    let qe_identity = c
        .create2(
            &format!("AutomataEnclaveIdentityDaoVersioned ({n})"),
            salt(&p.namespace, "ENCLAVE_ID_DAO_SALT"),
            &code.pccs("AutomataEnclaveIdentityDaoVersioned", "AutomataEnclaveIdentityDaoVersioned")?.initcode(
                &(p.storage, p.p256, p.dependency_config, p.enclave_identity_helper, p.pck_helper, p.owner, n)
                    .abi_encode_params(),
            ),
        )
        .await?;
    let fmspc_tcb = c
        .create2(
            &format!("AutomataFmspcTcbDaoVersioned ({n})"),
            salt(&p.namespace, "FMSPC_TCB_DAO_SALT"),
            &code.pccs("AutomataFmspcTcbDaoVersioned", "AutomataFmspcTcbDaoVersioned")?.initcode(
                &(p.storage, p.p256, p.pcs_dao, p.fmspc_tcb_helper, p.pck_helper, p.crl_helper, p.owner, n)
                    .abi_encode_params(),
            ),
        )
        .await?;
    for (dao, name) in [(qe_identity, "QE identity DAO"), (fmspc_tcb, "TCB info DAO")] {
        let got = IVersioned::new(dao, &c.p)
            .TCB_EVALUATION_NUMBER()
            .call()
            .await
            .with_context(|| format!("reading the evaluation number of {dao}"))?;
        if got != n {
            bail!("{name} {dao} is built for evaluation {got}, not {n}");
        }
        grant_writer(c, p.storage, dao, &format!("the evaluation-{n} {name}")).await?;
        grant_attester(c, dao, seeder, &format!("the evaluation-{n} {name}")).await?;
    }
    let router = IRouter::new(p.router, &c.p);
    if router.qeIdDaoVersionedAddr(n).call().await.context("reading the router's QE identity DAOs")? != qe_identity {
        let data = IRouter::setQeIdDaoVersionedAddrCall { tcbEval: n, addr: qe_identity }.abi_encode();
        c.send(&format!("registering the evaluation-{n} QE identity DAO"), p.router, data.into()).await?;
    }
    if router.fmspcTcbDaoVersionedAddr(n).call().await.context("reading the router's TCB info DAOs")? != fmspc_tcb {
        let data = IRouter::setFmspcTcbDaoVersionedAddrCall { tcbEval: n, addr: fmspc_tcb }.abi_encode();
        c.send(&format!("registering the evaluation-{n} TCB info DAO"), p.router, data.into()).await?;
    }
    Ok(VersionedDaos { qe_identity, fmspc_tcb })
}

/// Add contracts for one more evaluation number to an existing deployment.
pub async fn add_eval(ctx: &Ctx, c: &Chain, p: &mut Pccs, n: u32, seeder: Address) -> Result<()> {
    let code = Code { pccs: pccs_dir(ctx), dcap: dcap_dir(ctx) };
    let daos = deploy_eval(&code, c, p, n, seeder).await?;
    p.evals.insert(n, daos);
    Ok(())
}

// ---------------------------------------------------------------- Intel's collateral

pub struct Intel {
    http: reqwest::Client,
}

pub struct Fetched {
    pub body: Vec<u8>,
    pub headers: reqwest::header::HeaderMap,
}

impl Intel {
    pub fn new() -> Self {
        Self { http: reqwest::Client::new() }
    }

    /// GET with retries: Intel rate-limits, which is the common failure for a scheduled run.
    pub async fn get(&self, what: &str, url: &str) -> Result<Fetched> {
        let mut last = String::new();
        for attempt in 0..4 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(2 * attempt)).await;
            }
            match self.http.get(url).timeout(std::time::Duration::from_secs(60)).send().await {
                Ok(r) if r.status().is_success() => {
                    let headers = r.headers().clone();
                    let body = r.bytes().await.with_context(|| format!("reading {what} from {url}"))?.to_vec();
                    return Ok(Fetched { body, headers });
                }
                Ok(r) => last = format!("HTTP {}", r.status()),
                Err(e) => last = e.to_string(),
            }
        }
        bail!("could not fetch {what} from Intel ({url}): {last}")
    }

    pub async fn json(&self, what: &str, url: &str) -> Result<(String, reqwest::header::HeaderMap)> {
        let f = self.get(what, url).await?;
        let s = String::from_utf8(f.body).with_context(|| format!("{what} from {url} is not text"))?;
        Ok((s, f.headers))
    }

    /// The TDX evaluation numbers Intel publishes.
    pub async fn tdx_evals(&self) -> Result<(BTreeSet<u32>, String)> {
        let (raw, _) = self.json("Intel's TDX evaluation numbers", &format!("{TDX_API}/tcbevaluationdatanumbers")).await?;
        let v: serde_json::Value = serde_json::from_str(&raw).context("Intel's evaluation-number list is not JSON")?;
        let list = v["tcbEvaluationDataNumbers"]["tcbEvalNumbers"]
            .as_array()
            .context("Intel's evaluation-number list has no tcbEvalNumbers")?;
        let set = list
            .iter()
            .filter_map(|x| x["tcbEvaluationDataNumber"].as_u64().map(|n| n as u32))
            .collect();
        Ok((set, raw))
    }

    /// The evaluation number Intel calls standard today: the one the verifier reads.
    pub async fn standard(&self, fmspc: &str) -> Result<u32> {
        let (raw, _) = self.json("TCB info", &format!("{TDX_API}/tcb?fmspc={fmspc}")).await?;
        let v: serde_json::Value = serde_json::from_str(&raw).context("Intel's TCB info is not JSON")?;
        v["tcbInfo"]["tcbEvaluationDataNumber"]
            .as_u64()
            .map(|n| n as u32)
            .context("Intel's TCB info names no tcbEvaluationDataNumber")
    }
}

/// The evaluation numbers a new deployment needs: Intel's standard today, and every number
/// Intel already publishes after it, so the day the next one becomes standard is covered.
pub async fn evals_to_deploy(intel: &Intel, fmspc: &str) -> Result<BTreeSet<u32>> {
    let standard = intel.standard(fmspc).await?;
    let (published, _) = intel.tdx_evals().await?;
    let mut out: BTreeSet<u32> = published.into_iter().filter(|n| *n >= standard).collect();
    out.insert(standard);
    Ok(out)
}

/// The certificates in an issuer-chain header: URL-escaped PEM, leaf first, root last.
fn issuer_chain(headers: &reqwest::header::HeaderMap, name: &str) -> Result<Vec<Vec<u8>>> {
    let raw = headers
        .get(name)
        .with_context(|| format!("Intel's response carried no {name} header"))?
        .to_str()
        .with_context(|| format!("{name} is not text"))?;
    let pem = percent_encoding::percent_decode_str(raw).decode_utf8().context("issuer chain is not UTF-8")?;
    let mut certs = Vec::new();
    for block in pem.split("-----BEGIN CERTIFICATE-----").skip(1) {
        let body = block.split("-----END CERTIFICATE-----").next().unwrap_or("");
        let b64: String = body.chars().filter(|c| !c.is_whitespace()).collect();
        use base64::Engine;
        certs.push(base64::engine::general_purpose::STANDARD.decode(b64).context("a certificate in the chain is not base64")?);
    }
    if certs.len() < 2 {
        bail!("{name} has {} certificate(s), expected the issuer and the root", certs.len());
    }
    Ok(certs)
}

/// The root CA CRL's location, from the root certificate's distribution point.
fn crl_url(root: &[u8]) -> String {
    use x509_parser::extensions::{DistributionPointName, GeneralName, ParsedExtension};
    if let Ok((_, cert)) = x509_parser::parse_x509_certificate(root) {
        for ext in cert.extensions() {
            if let ParsedExtension::CRLDistributionPoints(points) = ext.parsed_extension() {
                for p in points.iter() {
                    if let Some(DistributionPointName::FullName(names)) = &p.distribution_point {
                        for n in names {
                            if let GeneralName::URI(uri) = n {
                                return uri.to_string();
                            }
                        }
                    }
                }
            }
        }
    }
    ROOT_CRL_FALLBACK.to_string()
}

/// The object under `key`, lifted from Intel's response as the exact bytes it served. The
/// signature covers those bytes, so it is never re-serialised.
fn signed_object(raw: &str, key: &str) -> Result<JsonObj> {
    let at = raw.find(&format!("\"{key}\"")).with_context(|| format!("no \"{key}\" in Intel's response"))?;
    let open = at + raw[at..].find('{').with_context(|| format!("\"{key}\" is not an object"))?;
    let mut depth = 0usize;
    let mut end = None;
    for (i, ch) in raw[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end.with_context(|| format!("\"{key}\" never closes"))?;
    let v: serde_json::Value = serde_json::from_str(raw).context("Intel's response is not JSON")?;
    let sig = v["signature"].as_str().context("Intel's response has no signature")?;
    Ok(JsonObj {
        body: raw[open..=end].to_string(),
        signature: hex::decode(sig).context("Intel's signature is not hex")?.into(),
    })
}

/// Publish everything the verifier reads, for every evaluation number the router has
/// contracts for. Every artifact is Intel-signed and the DAOs check that signature, so the key
/// that sends needs no trust, only the uploader role.
pub async fn seed(c: &Chain, p: &Pccs, fmspc: &str, dry_run: bool) -> Result<()> {
    let intel = Intel::new();
    let (published, eval_tdx_raw) = intel.tdx_evals().await?;
    let standard = intel.standard(fmspc).await?;

    // Check everything before the first transaction, so a gap is reported as nothing sent
    // rather than as a half-seeded chain.
    let mut problems = Vec::new();
    if !p.evals.contains_key(&standard) {
        problems.push(format!(
            "evaluation {standard} is Intel's standard today, which is what the verifier reads, but this \
             deployment has no contracts for it. Add them: teeism pccs tcb-eval --chain {} {standard}",
            c.name()
        ));
    }
    let evals: Vec<(u32, VersionedDaos)> = p
        .evals
        .iter()
        .filter(|(n, _)| published.contains(n))
        .map(|(n, d)| (*n, d.clone()))
        .collect();
    for (n, _) in p.evals.iter().filter(|(n, _)| !published.contains(n)) {
        out::warn(format!("Intel no longer publishes evaluation {n}; skipping it"));
    }
    for (n, d) in &evals {
        for dao in [d.qe_identity, d.fmspc_tcb] {
            if !IRoles::new(dao, &c.p).hasAnyRole(c.from, U256::from(ATTESTER_ROLE)).call().await? {
                problems.push(format!("{} lacks ATTESTER_ROLE on {dao} (evaluation {n}); `teeism pccs deploy` grants it", c.from));
            }
        }
    }
    if !IRoles::new(p.tcb_eval_dao, &c.p).hasAnyRole(c.from, U256::from(ATTESTER_ROLE)).call().await? {
        problems.push(format!("{} lacks ATTESTER_ROLE on the TCB evaluation DAO {}", c.from, p.tcb_eval_dao));
    }
    if !problems.is_empty() {
        bail!("nothing was published on {}:\n  - {}", c.chain.label, problems.join("\n  - "));
    }
    out::info(format!(
        "evaluations {:?} (Intel's standard is {standard}), seeder {}",
        evals.iter().map(|(n, _)| n).collect::<Vec<_>>(),
        c.from
    ));

    let tcb = intel.get("TCB info", &format!("{TDX_API}/tcb?fmspc={fmspc}")).await?;
    let tcb_chain = issuer_chain(&tcb.headers, "TCB-Info-Issuer-Chain")?;
    let plat = intel.get("the PCK platform CRL", &format!("{SGX_API}/pckcrl?ca=platform&encoding=der")).await?;
    let proc = intel.get("the PCK processor CRL", &format!("{SGX_API}/pckcrl?ca=processor&encoding=der")).await?;
    let plat_chain = issuer_chain(&plat.headers, "SGX-PCK-CRL-Issuer-Chain")?;
    let proc_chain = issuer_chain(&proc.headers, "SGX-PCK-CRL-Issuer-Chain")?;
    let signing = tcb_chain[0].clone();
    let root = tcb_chain[1].clone();
    let root_crl_url = crl_url(&root);
    let root_crl = intel.get("the root CA CRL", &root_crl_url).await?.body;
    let (eval_sgx_raw, _) = intel
        .json("Intel's SGX evaluation numbers", &format!("{SGX_API}/tcbevaluationdatanumbers"))
        .await?;
    let mut per_eval = Vec::new();
    for (n, d) in &evals {
        let (tcb_raw, _) = intel
            .json(&format!("evaluation-{n} TCB info"), &format!("{TDX_API}/tcb?fmspc={fmspc}&tcbEvaluationDataNumber={n}"))
            .await?;
        let (qe_raw, _) = intel
            .json(&format!("the evaluation-{n} QE identity"), &format!("{TDX_API}/qe/identity?tcbEvaluationDataNumber={n}"))
            .await?;
        per_eval.push((*n, d.clone(), signed_object(&tcb_raw, "tcbInfo")?, signed_object(&qe_raw, "enclaveIdentity")?));
    }

    if dry_run {
        out::info("dry run: every check passed and Intel served every artifact; nothing was sent");
        return Ok(());
    }

    use ICollateral as C;
    let pcs = p.pcs_dao;
    let mut uploads: Vec<(String, Address, Vec<u8>)> = vec![
        ("root CA certificate".into(), pcs, C::upsertPcsCertificatesCall { ca: ca::ROOT, cert: root.into() }.abi_encode()),
        ("TCB signing certificate".into(), pcs, C::upsertPcsCertificatesCall { ca: ca::SIGNING, cert: signing.into() }.abi_encode()),
        ("PCK platform CA certificate".into(), pcs, C::upsertPcsCertificatesCall { ca: ca::PLATFORM, cert: plat_chain[0].clone().into() }.abi_encode()),
        // The processor CRL is issued by a different CA, whose certificate has to be there too
        // or its CRL has nothing to be verified against.
        ("PCK processor CA certificate".into(), pcs, C::upsertPcsCertificatesCall { ca: ca::PROCESSOR, cert: proc_chain[0].clone().into() }.abi_encode()),
        (format!("root CA CRL ({root_crl_url})"), pcs, C::upsertRootCACrlCall { rootcacrl: root_crl.into() }.abi_encode()),
        ("PCK platform CRL".into(), pcs, C::upsertPckCrlCall { ca: ca::PLATFORM, crl: plat.body.into() }.abi_encode()),
        ("PCK processor CRL".into(), pcs, C::upsertPckCrlCall { ca: ca::PROCESSOR, crl: proc.body.into() }.abi_encode()),
    ];
    for (plat_name, raw) in [("SGX", &eval_sgx_raw), ("TDX", &eval_tdx_raw)] {
        let obj = signed_object(raw, "tcbEvaluationDataNumbers")?;
        uploads.push((format!("{plat_name} evaluation numbers"), p.tcb_eval_dao, C::upsertTcbEvaluationDataCall { tcbEvalObj: obj }.abi_encode()));
    }
    for (n, d, tcb_obj, qe_obj) in per_eval {
        // EnclaveId TD_QE = 2, PCS API version 4.
        uploads.push((
            format!("evaluation-{n} TDX QE identity"),
            d.qe_identity,
            C::upsertEnclaveIdentityCall { id: U256::from(2), version: U256::from(4), enclaveIdentityObj: qe_obj }.abi_encode(),
        ));
        uploads.push((format!("evaluation-{n} TDX TCB info"), d.fmspc_tcb, C::upsertFmspcTcbCall { tcbInfoObj: tcb_obj }.abi_encode()));
    }

    // Every upload is attempted, and the run fails at the end if any did not land. The DAOs
    // refuse an artifact identical to the one they hold, which is "already current", not a
    // failure: those need very different responses.
    let mut failures = Vec::new();
    for (label, to, data) in uploads {
        match c.send(&label, to, data.into()).await {
            Ok(_) => out::info(format!("{label}: published")),
            Err(e) if format!("{e:#}").contains("Duplicate_Collateral") => out::info(format!("{label}: already current")),
            Err(e) => {
                out::bad(format!("{label}: {e:#}"));
                failures.push(label);
            }
        }
    }
    if !failures.is_empty() {
        bail!(
            "{} artifact(s) could not be published on {}: {} (the reasons are above)",
            failures.len(),
            c.chain.label,
            failures.join(", ")
        );
    }
    Ok(())
}

// ---------------------------------------------------------------- status

pub struct Validity {
    pub label: String,
    /// (not before, not after), or why it could not be read.
    pub window: Result<(u64, u64), String>,
}

/// When each piece of collateral the verifier reads stops being valid.
pub async fn validity(rpc: &str, p: &Pccs, fmspc: &str) -> Result<Vec<Validity>> {
    let r = evm::reader(rpc)?;
    let fm: [u8; 6] = hex::decode(fmspc)
        .ok()
        .and_then(|b| b.try_into().ok())
        .with_context(|| format!("FMSPC {fmspc} is not 6 bytes of hex"))?;
    let mut rows = Vec::new();
    let read = |dao: Address, key: Result<FixedBytes<32>, String>| {
        let r = r.clone();
        async move {
            let key = key?;
            ICollateral::new(dao, &r)
                .getCollateralValidity(key)
                .call()
                .await
                .map(|v| (v._0, v._1))
                .map_err(|e| e.to_string())
        }
    };
    let key = |res: Result<FixedBytes<32>, alloy::contract::Error>| res.map_err(|e| format!("deriving the key: {e}"));
    for (n, d) in &p.evals {
        let k = key(ICollateral::new(d.fmspc_tcb, &r).FMSPC_TCB_KEY(1, fm.into(), 3).call().await);
        rows.push(Validity { label: format!("TDX TCB info ({n})"), window: read(d.fmspc_tcb, k).await });
        let k = key(ICollateral::new(d.qe_identity, &r).ENCLAVE_ID_KEY(U256::from(2), U256::from(4)).call().await);
        rows.push(Validity { label: format!("TD_QE identity ({n})"), window: read(d.qe_identity, k).await });
    }
    for (label, id) in [("SGX evaluation numbers", 0u8), ("TDX evaluation numbers", 1)] {
        let k = key(ICollateral::new(p.tcb_eval_dao, &r).TCB_EVAL_KEY(id).call().await);
        rows.push(Validity { label: label.into(), window: read(p.tcb_eval_dao, k).await });
    }
    for (label, ca, crl) in [
        ("root CA CRL", ca::ROOT, true),
        ("PCK processor CRL", ca::PROCESSOR, true),
        ("PCK platform CRL", ca::PLATFORM, true),
        ("root CA certificate", ca::ROOT, false),
        ("TCB signing certificate", ca::SIGNING, false),
        ("PCK processor certificate", ca::PROCESSOR, false),
        ("PCK platform certificate", ca::PLATFORM, false),
    ] {
        let k = key(ICollateral::new(p.pcs_dao, &r).PCS_KEY(ca, crl).call().await);
        rows.push(Validity { label: label.into(), window: read(p.pcs_dao, k).await });
    }
    Ok(rows)
}

/// Run a quote through the chain's DCAP verifier, as the ISM will. Returns why it was refused.
pub async fn check_quote(rpc: &str, p: &Pccs, quote: &[u8]) -> Result<()> {
    let r = evm::reader(rpc)?;
    let res = IEntrypoint::new(p.entrypoint, &r)
        .verifyAndAttestOnChain(quote.to_vec().into())
        .call()
        .await
        .map_err(|e| anyhow::anyhow!("the verifier reverted: {}", evm::explain_revert(&e.to_string())))?;
    if !res.success {
        let why = String::from_utf8_lossy(&res.output);
        bail!("the verifier refused the quote: {why} (0x{})", hex::encode(&res.output));
    }
    Ok(())
}

pub fn pccs_for<'a>(d: &'a crate::state::Deployment, chain: &EvmChain) -> Result<&'a Pccs> {
    d.evm(chain.name)
        .and_then(|e| e.pccs.as_ref())
        .with_context(|| format!("no PCCS on {}; run `teeism pccs deploy --chain {}`", chain.label, chain.name))
}

/// Fetch Automata's Solidity dependencies at the revisions their foundry.lock pins and
/// compile both projects. The dependency directories are gitignored.
pub fn build(ctx: &Ctx) -> Result<()> {
    use crate::run::Cmd;
    let pccs = pccs_dir(ctx);
    let dcap = dcap_dir(ctx);
    let deps: [(&Path, &str, &str, &str); 6] = [
        (&pccs, "lib/forge-std", "https://github.com/foundry-rs/forge-std", "155d547c449afa8715f538d69454b83944117811"),
        (&pccs, "lib/openzeppelin-contracts", "https://github.com/OpenZeppelin/openzeppelin-contracts", "442886ed5ff8a0b9ab477b191f5238541ee6d772"),
        (&pccs, "lib/solady", "https://github.com/Vectorized/solady", "772a49d91b004eb55dc9129ae57dd36fbb8417ee"),
        (&dcap, "lib/forge-std", "https://github.com/foundry-rs/forge-std", "3b20d60d14b343ee4f908cb8079495c07f5e8981"),
        (&dcap, "lib/risc0-ethereum", "https://github.com/risc0/risc0-ethereum", "365e7b2db4f620fa256580c27558d2623362b9ae"),
        (&dcap, "lib/sp1-contracts", "https://github.com/succinctlabs/sp1-contracts", "512b5e029abc27f6e46a3c7eba220dac83ecc306"),
    ];
    for (project, dir, url, rev) in deps {
        let path = project.join(dir);
        if !path.join(".git").exists() {
            out::info(format!("fetching {url} at {rev}"));
            Cmd::new(format!("cloning {url}"), "git").args(["clone", "-q", url]).arg(&path).output()?;
        }
        let head = Cmd::new("reading a dependency's revision", "git").arg("-C").arg(&path).args(["rev-parse", "HEAD"]).output()?;
        if head.trim() != rev {
            Cmd::new(format!("fetching {url}"), "git").arg("-C").arg(&path).args(["fetch", "-q", "origin"]).output()?;
            Cmd::new(format!("checking out {rev} of {url}"), "git").arg("-C").arg(&path).args(["checkout", "-q", rev]).output()?;
        }
    }
    // risc0-ethereum carries the OpenZeppelin that Automata's remappings name.
    Cmd::new("fetching risc0-ethereum's OpenZeppelin", "git")
        .arg("-C")
        .arg(dcap.join("lib/risc0-ethereum"))
        .args(["submodule", "update", "-q", "--init", "lib/openzeppelin-contracts"])
        .output()?;
    let link = dcap.join("lib/automata-on-chain-pccs");
    if !link.exists() {
        #[cfg(unix)]
        std::os::unix::fs::symlink("../../../pccs", &link).with_context(|| format!("linking {}", link.display()))?;
    }
    Cmd::new("compiling the Automata PCCS", "forge")
        .args(["build", "--skip", "test", "--skip", "script"])
        .cwd(&pccs)
        .run()?;
    Cmd::new("compiling the Automata DCAP verifier", "forge")
        .args(["build", "--skip", "test", "--skip", "script", "--remappings"])
        .arg("@openzeppelin/contracts/=lib/automata-on-chain-pccs/lib/openzeppelin-contracts/contracts/")
        .cwd(&dcap)
        .run()?;
    Ok(())
}
