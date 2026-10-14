//! The enclaves on Phala Cloud: `enclaves up` deploys them and records the identity each ISM
//! will pin; `enclaves verify` checks a running enclave against this checkout.
//!
//! The one piece that cannot be local: a TDX quote has to come from real Intel hardware.

use alloy::primitives::keccak256;
use anyhow::{bail, Context, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::Duration;

use crate::chains::{self, ENCLAVES};
use crate::ctx::Ctx;
use crate::images::{self, compose_file};
use crate::out;
use crate::run::{first_json, Cmd};
use crate::state::{Cvm, Enclave};

/// Phala node 18 is prod9. Auto-selection sometimes lands on a node whose gateway never
/// registers the CVM: it runs, and every request returns nothing.
const DEFAULT_NODE: &str = "18";
pub const DEFAULT_OS_IMAGE: &str = "dstack-0.5.9";
const DEFAULT_INSTANCE: &str = "tdx.small";

/// The chains a CVM target serves.
pub fn chains_in(target: &str) -> Vec<&'static str> {
    if target == "all" {
        ENCLAVES.iter().map(|(c, _)| *c).collect()
    } else {
        ENCLAVES.iter().map(|(c, _)| *c).filter(|c| *c == target).collect()
    }
}

/// What `GET /identity` returns: the quote, and dstack's unsigned description of the CVM.
pub struct Identity {
    pub raw: Value,
    pub quote: Vec<u8>,
}

pub async fn fetch_identity(url: &str) -> Result<Identity> {
    let raw: Value = reqwest::Client::new()
        .get(format!("{url}/identity"))
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .with_context(|| format!("the enclave at {url} did not answer /identity"))?
        .error_for_status()
        .with_context(|| format!("the enclave at {url} refused /identity"))?
        .json()
        .await
        .with_context(|| format!("{url}/identity is not JSON"))?;
    let q = raw["quote"].as_str().with_context(|| format!("{url}/identity carries no quote"))?;
    let quote = hex::decode(q.trim_start_matches("0x")).with_context(|| format!("the quote from {url} is not hex"))?;
    Ok(Identity { raw, quote })
}

/// The TD report body: after the 48-byte quote header, plus a 6-byte descriptor in v5.
pub fn report_body(quote: &[u8]) -> Result<&[u8]> {
    if quote.len() < 48 + 6 + 584 {
        bail!("the quote is {} bytes, too short to hold a TD report", quote.len());
    }
    let version = u16::from_le_bytes([quote[0], quote[1]]);
    let start = match version {
        4 => 48,
        5 => 48 + 6,
        v => bail!("quote version {v}; only v4 and v5 are understood"),
    };
    Ok(&quote[start..])
}

/// What a TeeDcapIsm pins: keccak256(mr_td ++ mr_config_id ++ rtmr0 ++ rtmr1 ++ rtmr2). RTMR3 is
/// left out on purpose: it carries the app and instance ids, so including it would tie the ISM
/// to one CVM rather than to the code it runs.
pub fn measurements(quote: &[u8]) -> Result<String> {
    let b = report_body(quote)?;
    let mut pre = b[136..232].to_vec();
    pre.extend_from_slice(&b[328..472]);
    Ok(format!("{}", keccak256(pre)))
}

/// mr_td, the measurement of the TD's firmware and OS image: what a new dstack OS moves.
pub fn mr_td(quote: &[u8]) -> Result<String> {
    Ok(format!("0x{}", hex::encode(&report_body(quote)?[136..184])))
}

/// The dstack app_compose the enclave measured, from its unsigned info.
fn app_compose(id: &Identity) -> Result<String> {
    let tcb = &id.raw["info"]["tcb_info"];
    let tcb: Value = match tcb.as_str() {
        Some(s) => serde_json::from_str(s).context("tcb_info is not JSON")?,
        None => tcb.clone(),
    };
    tcb["app_compose"].as_str().map(str::to_string).context("the enclave reported no app_compose")
}

fn measured_compose(id: &Identity) -> Result<String> {
    let ac: Value = serde_json::from_str(&app_compose(id)?).context("app_compose is not JSON")?;
    ac["docker_compose_file"].as_str().map(str::to_string).context("app_compose has no docker_compose_file")
}

async fn healthy(url: &str) -> bool {
    matches!(
        reqwest::Client::new().get(format!("{url}/health")).timeout(Duration::from_secs(15)).send().await,
        Ok(r) if r.status().is_success()
    )
}

/// Refuse to deploy an image the repo does not describe, or that the current code would not
/// build. For each chain deployed:
///   - its source still evaluates to the store path deploy/images.lock recorded;
///   - its compose file pins the digest the lock recorded;
///   - docker-compose.all.yml is exactly what the per-chain files generate;
///   - all of them are committed and pushed, so `main` names what the enclaves run.
fn check_pins(ctx: &Ctx, targets: &[String]) -> Result<()> {
    let lock = images::read_lock(ctx)?;
    for target in targets {
        for chain in chains_in(target) {
            let (locked_out, locked_digest) = lock
                .get(chain)
                .with_context(|| format!("{chain} has no entry in deploy/images.lock; run `teeism images`"))?;
            if images::out_path(ctx, chain)? != *locked_out {
                bail!("{chain}: the enclave code changed since its image was pinned; run `teeism images`");
            }
            if images::pinned_digest(ctx, chain)? != *locked_digest {
                bail!("{chain}: deploy/docker-compose.{chain}.yml does not pin the digest in deploy/images.lock");
            }
        }
    }
    let all = std::fs::read_to_string(compose_file(ctx, "all"))?;
    if all != images::compose_all(ctx)? {
        bail!("deploy/docker-compose.all.yml is not what the per-chain files generate; run `teeism images`");
    }
    let git = |what: &str, args: &[&str]| Cmd::new(what, "git").arg("-C").arg(&ctx.repo).args(args).capture();
    let (s, _, _) = git("checking the pins are committed", &["diff", "--quiet", "HEAD", "--", "deploy/images.lock", "deploy/docker-compose.*.yml"])?;
    if !s.success() {
        bail!("deploy/images.lock or a compose file has uncommitted changes; commit and push them first");
    }
    Cmd::new("fetching the remote to check this commit is pushed", "git").arg("-C").arg(&ctx.repo).args(["fetch", "-q"]).output()?;
    let (s, _, _) = git("checking this commit is pushed", &["merge-base", "--is-ancestor", "HEAD", "@{upstream}"])?;
    if !s.success() {
        bail!("this commit is not pushed (or the branch tracks no remote); push it first, so the repo names what the enclaves run");
    }
    Ok(())
}

pub struct UpArgs {
    pub chains: Vec<String>,
    pub unpinned: bool,
    pub os_image: String,
}

/// `teeism enclaves up`
pub async fn up(ctx: &Ctx, a: UpArgs) -> Result<()> {
    let targets: Vec<String> = if a.chains.is_empty() {
        vec!["all".into()]
    } else {
        for c in &a.chains {
            chains::enclave_port(c)?;
        }
        a.chains.clone()
    };
    if a.unpinned {
        out::warn("--unpinned: deploying without checking the pins against the code. Never on a deployment others rely on");
    } else {
        out::step("checking the pinned images against the code");
        check_pins(ctx, &targets)?;
    }
    let mut authenticated = false;
    for target in &targets {
        deploy_cvm(ctx, target, &a.os_image, &mut authenticated).await?;
    }

    out::step("reading each enclave's identity from its quote");
    let ident_dir = ctx.state_dir.join("identity");
    std::fs::create_dir_all(&ident_dir)?;
    for target in &targets {
        for chain in chains_in(target) {
            record_identity(ctx, chain).await?;
        }
    }
    Ok(())
}

/// The identity file x/teeism takes, its digest, and the TeeDcapIsm measurements, for one
/// enclave. Read from the quote, and only after the event log replays to the RTMRs the
/// hardware signed.
pub async fn record_identity(ctx: &Ctx, chain: &str) -> Result<()> {
    let mut d = ctx.deployment()?;
    let url = d
        .enclaves
        .chains
        .get(chain)
        .map(|e| e.url.clone())
        .with_context(|| format!("no {chain} enclave recorded; run `teeism enclaves up`"))?;
    let file = identity_file(ctx, chain);
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    Cmd::new(format!("reading the {chain} enclave's identity"), ctx.coprocessor_bin())
        .args(["identity", "--url", &url, "--json"])
        .arg(&file)
        .output()?;
    let digest = Cmd::new(format!("digesting the {chain} identity"), ctx.bin_dir().join("teeism-identity"))
        .arg("-identity")
        .arg(&file)
        .output()?
        .trim()
        .to_string();
    if digest.is_empty() {
        bail!("teeism-identity printed no digest for {}", file.display());
    }
    let id = fetch_identity(&url).await?;
    let e = d.enclaves.chains.get_mut(chain).context("enclave vanished from the record")?;
    e.identity_digest = Some(digest.clone());
    e.measurements = Some(measurements(&id.quote)?);
    out::kv(&format!("{chain} identity"), &digest);
    out::kv(&format!("{chain} measurements"), e.measurements.as_deref().unwrap_or(""));
    out::kv(&format!("{chain} mr_td"), mr_td(&id.quote)?);
    ctx.save(&d)
}

pub fn identity_file(ctx: &Ctx, chain: &str) -> std::path::PathBuf {
    ctx.state_dir.join("identity").join(format!("{chain}.json"))
}

async fn deploy_cvm(ctx: &Ctx, target: &str, os_image: &str, authenticated: &mut bool) -> Result<()> {
    let compose = compose_file(ctx, target);
    let file = std::fs::read_to_string(&compose).with_context(|| format!("no compose for {target} at {}", compose.display()))?;
    let first = chains_in(target)[0];
    let mut name = format!("teeism-{target}");
    let mut d = ctx.deployment()?;

    // Keep a recorded CVM only if it answers, measured this compose file, and runs this OS
    // image. Answering is not enough: after a re-pin the old CVM is still healthy, and keeping
    // it would leave every ISM pinning the old code.
    if let Some(cvm) = d.enclaves.cvms.get(target).cloned() {
        let url = chains::enclave_url(&cvm.app_id, first)?;
        if healthy(&url).await {
            let id = fetch_identity(&url).await?;
            if measured_compose(&id)? == file && cvm.os_image.as_deref().unwrap_or(DEFAULT_OS_IMAGE) == os_image {
                out::info(format!("{target} CVM {} already runs this compose on {os_image}", cvm.app_id));
                record_urls(&mut d, target, &cvm.app_id)?;
                return ctx.save(&d);
            }
            out::warn(format!(
                "the recorded {target} CVM {} runs a different compose or OS image; deploying a new one. \
                 The old one keeps running (and billing) until `teeism enclaves delete --retired`",
                cvm.app_id
            ));
        } else {
            out::warn(format!("the recorded {target} CVM {} is not answering; deploying a new one", cvm.app_id));
        }
        // The old CVM still holds the plain name, and Phala refuses a second CVM with it.
        name = format!("{name}-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs());
    }

    if !*authenticated {
        Cmd::new("checking the Phala Cloud login (phala auth login <api-key>, key from cloud.phala.network > Settings > API Keys)", "phala")
            .arg("status")
            .output()?;
        *authenticated = true;
    }
    let instance = ctx.var_or("PHALA_INSTANCE_TYPE", DEFAULT_INSTANCE);
    let node = ctx.var_or("PHALA_NODE_ID", DEFAULT_NODE);
    out::step(format!("deploying {name} ({instance}, node {node}, {os_image})"));
    // --no-dev-os matters: with an SSH key on this machine the CLI otherwise provisions a dev
    // image that allows a shell in the CVM, which would make its measurements meaningless.
    let raw = Cmd::new(format!("deploying the {target} CVM on Phala"), "phala")
        .args(["deploy", "--name", &name, "--compose"])
        .arg(&compose)
        .args(["--instance-type", &instance, "--node-id", &node, "--image", os_image, "--no-dev-os", "--wait", "--json"])
        .output()?;
    let app_id = first_json(&raw)
        .and_then(|v| find_key(&v, &["app_id", "appId"]))
        .with_context(|| format!("could not read the app id for {target} from phala's output; check `phala cvms ls` for a stray {name}:\n{raw}"))?;
    if let Some(old) = d.enclaves.cvms.insert(target.to_string(), Cvm { name: name.clone(), app_id: app_id.clone(), os_image: Some(os_image.to_string()) }) {
        d.enclaves.retired.push(old);
    }
    ctx.save(&d)?;
    out::kv("app id", &app_id);

    for chain in chains_in(target) {
        let url = chains::enclave_url(&app_id, chain)?;
        out::info(format!("waiting for {url}"));
        let mut up = false;
        for _ in 0..60 {
            if healthy(&url).await {
                up = true;
                break;
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
        if !up {
            bail!("the {chain} enclave did not answer within 10 minutes; check `phala cvms get {app_id}`");
        }
    }
    record_urls(&mut d, target, &app_id)?;
    ctx.save(&d)
}

/// Point every chain this CVM serves at it. A chain deployed alone later takes over its own.
fn record_urls(d: &mut crate::state::Deployment, target: &str, app_id: &str) -> Result<()> {
    for chain in chains_in(target) {
        let e = d.enclaves.chains.entry(chain.to_string()).or_default();
        let url = chains::enclave_url(app_id, chain)?;
        if e.url != url {
            *e = Enclave { url, target: target.to_string(), ..Enclave::default() };
        }
    }
    Ok(())
}

fn find_key(v: &Value, keys: &[&str]) -> Option<String> {
    match v {
        Value::Object(m) => {
            for k in keys {
                if let Some(Value::String(s)) = m.get(*k) {
                    if !s.is_empty() {
                        return Some(s.clone());
                    }
                }
            }
            m.values().find_map(|x| find_key(x, keys))
        }
        Value::Array(a) => a.iter().find_map(|x| find_key(x, keys)),
        _ => None,
    }
}

/// `teeism enclaves delete --retired`: delete every CVM a newer one replaced.
pub fn delete_retired(ctx: &Ctx) -> Result<()> {
    let mut d = ctx.deployment()?;
    if d.enclaves.retired.is_empty() {
        out::info("no retired CVMs");
        return Ok(());
    }
    while let Some(cvm) = d.enclaves.retired.first().cloned() {
        out::step(format!("deleting retired CVM {} ({})", cvm.app_id, cvm.name));
        delete(&cvm.app_id)?;
        d.enclaves.retired.remove(0);
        ctx.save(&d)?;
    }
    Ok(())
}

/// Delete a CVM on Phala.
pub fn delete(app_id: &str) -> Result<()> {
    Cmd::new(format!("deleting CVM {app_id} on Phala"), "phala")
        .args(["cvms", "delete", app_id, "--force"])
        .output()?;
    Ok(())
}

pub struct VerifyArgs {
    pub target: String,
    pub app_id: Option<String>,
    pub ism_chain: Option<String>,
    pub rebuild: bool,
}

/// `teeism enclaves verify`: is the enclave running the code in this checkout? Each link is
/// checked against the thing downstream of it, never against this repo's own claims:
///
///   this checkout's compose file -> the app_compose dstack measured -> compose_hash
///     -> mr_config_id inside the quote Intel signed -> what the ISM on chain accepts
///
/// `info` in the enclave's reply is unsigned and could say anything; the quote is signed, so
/// where they disagree the quote wins.
pub async fn verify(ctx: &Ctx, a: VerifyArgs) -> Result<()> {
    let d = ctx.deployment()?;
    let app_id = match a.app_id {
        Some(id) => id,
        None => d
            .enclaves
            .cvms
            .get(&a.target)
            .map(|c| c.app_id.clone())
            .with_context(|| format!("no recorded {} CVM; pass --app-id", a.target))?,
    };
    let first = *chains_in(&a.target).first().context("unknown target")?;
    let url = chains::enclave_url(&app_id, first)?;
    out::step(format!("enclave {url}"));
    let id = fetch_identity(&url).await?;
    let mut failed = Vec::new();

    out::step("1. compose_hash is the hash of what dstack measured");
    let ac = app_compose(&id)?;
    let hash = hex::encode(Sha256::digest(ac.as_bytes()));
    let claimed = id.raw["info"]["compose_hash"].as_str().unwrap_or("").trim_start_matches("0x").to_string();
    out::kv("sha256(app_compose)", &hash);
    if hash == claimed {
        out::ok("reproduced from the preimage the enclave supplied");
    } else {
        out::bad(format!("the enclave claims {claimed}"));
        failed.push("compose_hash");
    }

    out::step("2. that hash is inside the signed quote, not only in the unsigned info");
    let body = report_body(&id.quote)?;
    let cfg = &body[184..232];
    // dstack writes mr_config_id as 0x01, then the compose hash, then zero padding.
    if cfg[0] == 1 && hex::encode(&cfg[1..33]) == hash {
        out::ok(format!("mr_config_id carries it: {}", hex::encode(&cfg[1..33])));
    } else {
        out::bad(format!("mr_config_id is {}", hex::encode(cfg)));
        failed.push("mr_config_id");
    }

    out::step("3. the measured compose is this checkout's compose");
    let ours = std::fs::read_to_string(compose_file(ctx, &a.target))?;
    let measured = measured_compose(&id)?;
    if measured == ours {
        out::ok(format!("byte for byte identical to deploy/docker-compose.{}.yml", a.target));
    } else {
        out::bad("the enclave measured a different compose file");
        failed.push("compose");
    }

    out::step("4. the images that compose pins");
    let mut service = String::new();
    let mut services = Vec::new();
    for line in measured.lines() {
        if line.starts_with("  ") && !line.starts_with("   ") && line.trim_end().ends_with(':') {
            service = line.trim().trim_end_matches(':').to_string();
        } else if let Some(img) = line.trim().strip_prefix("image:") {
            services.push((service.clone(), img.trim().to_string()));
            out::info(format!("{service}: {}", img.trim()));
        }
    }

    out::step("5. what the chain will accept");
    out::kv("mr_td", mr_td(&id.quote)?);
    out::kv("measurements", measurements(&id.quote)?);
    if let Some(chain) = &a.ism_chain {
        let chain = chains::evm(chain)?;
        let ism = d.evm(chain.name).and_then(|e| e.ism).with_context(|| format!("no ISM recorded on {}", chain.label))?;
        let pinned = crate::isms::pinned_measurements(&chain.rpc(ctx), ism).await?;
        if pinned == measurements(&id.quote)? {
            out::ok(format!("the ISM {ism} on {} pins exactly this enclave", chain.label));
        } else {
            out::bad(format!("the ISM {ism} on {} pins {pinned}: it would reject this enclave", chain.label));
            failed.push("ism");
        }
    } else {
        out::info("skipped; pass --ism-chain <chain> to compare with an EVM ISM");
    }

    out::step("6. the images are what this source builds");
    if a.rebuild {
        for (chain, image) in &services {
            let digest = image.rsplit('@').next().unwrap_or("");
            Cmd::new(format!("building the {chain} image"), "nix")
                .args(["build", &format!(".#image-{chain}"), "-o", &format!("result-verify-{chain}")])
                .cwd(&ctx.repo)
                .run()?;
            let manifest = Cmd::new("reading the built image's manifest", "tar")
                .args(["-xOf"])
                .arg(ctx.repo.join(format!("result-verify-{chain}")))
                .arg("manifest.json")
                .output()?;
            let m: Value = serde_json::from_str(&manifest).context("the image's manifest.json is not JSON")?;
            let built = m[0]["Config"].as_str().unwrap_or("").trim_end_matches(".json").rsplit('/').next().unwrap_or("").trim_start_matches("sha256:").to_string();
            Cmd::new("pulling the pinned image", "docker").args(["pull", "-q", &format!("{}@{digest}", chains::IMAGE_REPO)]).output()?;
            let pinned = Cmd::new("reading the pinned image's config digest", "docker")
                .args(["image", "inspect", "--format", "{{.Id}}", &format!("{}@{digest}", chains::IMAGE_REPO)])
                .output()?
                .trim()
                .trim_start_matches("sha256:")
                .to_string();
            if !built.is_empty() && built == pinned {
                out::ok(format!("{chain}: rebuilt from this source, and it is the image the compose pins"));
            } else {
                out::bad(format!("{chain}: rebuilt {built}, but the pinned image is {pinned}"));
                failed.push("image");
            }
        }
    } else {
        out::info("not rebuilt: the digests are the registry's word until you pass --rebuild");
        out::info("(--rebuild reproduces the image only on x86_64 Linux with Nix's sandbox on)");
    }

    if failed.is_empty() {
        out::ok("every checked link holds");
        Ok(())
    } else {
        bail!("these links failed: {}", failed.join(", "))
    }
}
