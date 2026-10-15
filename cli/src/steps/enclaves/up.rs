//! `teeism enclaves up`, `identity` and `delete`: the CVMs on Phala and the identity every ISM
//! pins. The one piece that cannot be local: a TDX quote has to come from real Intel hardware.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use super::quote::*;
use crate::shared::record::{Cvm, Enclave};
use crate::shared::settings::Ctx;
use crate::shared::shell::{first_json, Cmd};
use crate::shared::{chains, print};
use crate::steps::enclaves::images::{self, compose_file};

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
            let (locked_out, locked_digest) = lock.get(chain).with_context(|| {
                format!("{chain} has no entry in deploy/images.lock; run `teeism images`")
            })?;
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
    let git = |what: &str, args: &[&str]| {
        Cmd::new(what, "git")
            .arg("-C")
            .arg(&ctx.repo)
            .args(args)
            .capture()
    };
    let (s, _, _) = git(
        "checking the pins are committed",
        &[
            "diff",
            "--quiet",
            "HEAD",
            "--",
            "deploy/images.lock",
            "deploy/docker-compose.*.yml",
        ],
    )?;
    if !s.success() {
        bail!("deploy/images.lock or a compose file has uncommitted changes; commit and push them first");
    }
    Cmd::new("fetching the remote to check this commit is pushed", "git")
        .arg("-C")
        .arg(&ctx.repo)
        .args(["fetch", "-q"])
        .output()?;
    let (s, _, _) = git(
        "checking this commit is pushed",
        &["merge-base", "--is-ancestor", "HEAD", "@{upstream}"],
    )?;
    if !s.success() {
        bail!("this commit is not pushed (or the branch tracks no remote); push it first, so the repo names what the enclaves run");
    }
    Ok(())
}

pub struct Args {
    pub chains: Vec<String>,
    pub unpinned: bool,
    pub os_image: Option<String>,
    pub instance_type: String,
}

/// `teeism enclaves up`
pub async fn run(ctx: &Ctx, a: Args) -> Result<()> {
    let targets: Vec<String> = if a.chains.is_empty() {
        vec!["all".into()]
    } else {
        for c in &a.chains {
            chains::enclave_port(c)?;
        }
        a.chains.clone()
    };
    if a.unpinned {
        print::warn("--unpinned: deploying without checking the pins against the code. Never on a deployment others rely on");
    } else {
        print::step("checking the pinned images against the code");
        check_pins(ctx, &targets)?;
    }
    let mut authenticated = false;
    for target in &targets {
        deploy_cvm(
            ctx,
            target,
            a.os_image.as_deref(),
            &a.instance_type,
            &mut authenticated,
        )
        .await?;
    }

    print::step("reading each enclave's identity from its quote");
    let ident_dir = ctx.state_dir.join("identity");
    std::fs::create_dir_all(&ident_dir)?;
    for target in &targets {
        for chain in chains_in(target) {
            record_identity(ctx, chain).await?;
        }
    }
    Ok(())
}

async fn deploy_cvm(
    ctx: &Ctx,
    target: &str,
    os_image: Option<&str>,
    instance: &str,
    authenticated: &mut bool,
) -> Result<()> {
    let compose = compose_file(ctx, target);
    let file = std::fs::read_to_string(&compose)
        .with_context(|| format!("no compose for {target} at {}", compose.display()))?;
    let first = chains_in(target)[0];
    let mut name = format!("teeism-{target}");
    let mut d = ctx.deployment()?;

    // Keep a recorded CVM only if it answers, measured this compose file, and runs the OS image
    // (if one was asked for) and instance type asked for. Answering is not enough: after a
    // re-pin the old CVM is still healthy, and keeping it would leave every ISM pinning the old
    // code.
    if let Some(cvm) = d.enclaves.cvms.get(target).cloned() {
        let gateway = match &cvm.gateway {
            Some(g) => g.clone(),
            None => cvm_info(&cvm.app_id)?.gateway,
        };
        let url = chains::enclave_url(&cvm.app_id, &gateway, first)?;
        if healthy(&url).await {
            let id = fetch_identity(&url).await?;
            if measured_compose(&id)? == file
                && os_image.is_none_or(|want| cvm.os_image.as_deref() == Some(want))
                && cvm.instance_type.as_deref().unwrap_or(DEFAULT_INSTANCE) == instance
            {
                print::info(format!(
                    "{target} CVM {} already runs this compose on {}, {instance}",
                    cvm.app_id,
                    cvm.os_image.as_deref().unwrap_or("its OS image")
                ));
                record_urls(&mut d, target, &cvm.app_id, &gateway)?;
                return ctx.save(&d);
            }
            print::warn(format!(
                "the recorded {target} CVM {} runs a different compose, OS image or instance type; deploying a new one. \
                 The old one keeps running (and billing) until `teeism enclaves delete --retired`",
                cvm.app_id
            ));
        } else {
            print::warn(format!(
                "the recorded {target} CVM {} is not answering; deploying a new one",
                cvm.app_id
            ));
        }
        // The old CVM still holds the plain name, and Phala refuses a second CVM with it.
        name = format!(
            "{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs()
        );
    }

    if !*authenticated {
        Cmd::new("checking the Phala Cloud login (phala auth login <api-key>, key from cloud.phala.network > Settings > API Keys)", "phala")
            .arg("status")
            .output()?;
        *authenticated = true;
    }
    print::step(format!(
        "deploying {name} ({instance}, {}; Phala picks the node)",
        os_image.unwrap_or("Phala's default OS image")
    ));
    // --no-dev-os matters: with an SSH key on this machine the CLI otherwise provisions a dev
    // image that allows a shell in the CVM, which would make its measurements meaningless.
    let mut deploy = Cmd::new(format!("deploying the {target} CVM on Phala"), "phala")
        .args(["deploy", "--name", &name, "--compose"])
        .arg(&compose)
        .args(["--instance-type", instance, "--no-dev-os", "--wait", "--json"]);
    if let Some(image) = os_image {
        deploy = deploy.args(["--image", image]);
    }
    let raw = deploy.output()?;
    let app_id = first_json(&raw)
        .and_then(|v| find_key(&v, &["app_id", "appId"]))
        .with_context(|| format!("could not read the app id for {target} from phala's output; check `phala cvms ls` for a stray {name}:\n{raw}"))?;
    // Where Phala put it: the node's gateway, and the OS image it actually runs.
    let info = cvm_info(&app_id)?;
    if let Some(old) = d.enclaves.cvms.insert(
        target.to_string(),
        Cvm {
            name: name.clone(),
            app_id: app_id.clone(),
            os_image: Some(info.os_image.clone()),
            instance_type: Some(instance.to_string()),
            gateway: Some(info.gateway.clone()),
        },
    ) {
        d.enclaves.retired.push(old);
    }
    ctx.save(&d)?;
    print::kv("app id", &app_id);
    print::kv("node", &info.node);
    print::kv("os image", &info.os_image);

    for chain in chains_in(target) {
        let url = chains::enclave_url(&app_id, &info.gateway, chain)?;
        print::info(format!("waiting for {url}"));
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
    record_urls(&mut d, target, &app_id, &info.gateway)?;
    ctx.save(&d)
}

/// Point every chain this CVM serves at it. A chain deployed alone later takes over its own.
fn record_urls(
    d: &mut crate::shared::record::Deployment,
    target: &str,
    app_id: &str,
    gateway: &str,
) -> Result<()> {
    for chain in chains_in(target) {
        let e = d.enclaves.chains.entry(chain.to_string()).or_default();
        let url = chains::enclave_url(app_id, gateway, chain)?;
        if e.url != url {
            *e = Enclave {
                url,
                target: target.to_string(),
                ..Enclave::default()
            };
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

// ---------------------------------------------------------------- teeism enclaves identity

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
    Cmd::new(
        format!("reading the {chain} enclave's identity"),
        ctx.coprocessor_bin(),
    )
    .args(["identity", "--url", &url, "--json"])
    .arg(&file)
    .output()?;
    let digest = Cmd::new(
        format!("digesting the {chain} identity"),
        ctx.bin_dir().join("teeism-identity"),
    )
    .arg("-identity")
    .arg(&file)
    .output()?
    .trim()
    .to_string();
    if digest.is_empty() {
        bail!("teeism-identity printed no digest for {}", file.display());
    }
    let id = fetch_identity(&url).await?;
    let e = d
        .enclaves
        .chains
        .get_mut(chain)
        .context("enclave vanished from the record")?;
    e.identity_digest = Some(digest.clone());
    e.measurements = Some(measurements(&id.quote)?);
    print::kv(&format!("{chain} identity"), &digest);
    print::kv(
        &format!("{chain} measurements"),
        e.measurements.as_deref().unwrap_or(""),
    );
    print::kv(&format!("{chain} mr_td"), mr_td(&id.quote)?);
    ctx.save(&d)
}

/// `teeism enclaves identity`: re-read what each recorded enclave's quote says.
pub async fn identity(ctx: &Ctx, chains: Vec<String>) -> Result<()> {
    let chains = if chains.is_empty() {
        ctx.deployment()?.enclaves.chains.keys().cloned().collect()
    } else {
        chains
    };
    for c in chains {
        record_identity(ctx, &c).await?;
    }
    Ok(())
}

// ---------------------------------------------------------------- teeism enclaves delete

/// `teeism enclaves delete`
pub fn delete_cmd(ctx: &Ctx, app_id: Option<String>, retired: bool) -> Result<()> {
    match (app_id, retired) {
        (_, true) => delete_retired(ctx),
        (Some(id), false) => delete(&id),
        (None, false) => bail!("give --app-id <id> or --retired"),
    }
}

/// `teeism enclaves delete --retired`: delete every CVM a newer one replaced.
pub fn delete_retired(ctx: &Ctx) -> Result<()> {
    let mut d = ctx.deployment()?;
    if d.enclaves.retired.is_empty() {
        print::info("no retired CVMs");
        return Ok(());
    }
    while let Some(cvm) = d.enclaves.retired.first().cloned() {
        print::step(format!("deleting retired CVM {} ({})", cvm.app_id, cvm.name));
        delete(&cvm.app_id)?;
        d.enclaves.retired.remove(0);
        ctx.save(&d)?;
    }
    Ok(())
}

/// Delete a CVM on Phala.
/// Delete a CVM on Phala. One Phala no longer has is already where this wants it, so "not
/// found" counts as done; any other failure stops, since the CVM may still be billing.
pub fn delete(app_id: &str) -> Result<()> {
    let (status, stdout, stderr) = Cmd::new(format!("deleting CVM {app_id} on Phala"), "phala")
        .args(["cvms", "delete", app_id, "--force"])
        .capture()?;
    if status.success() {
        return Ok(());
    }
    let said = format!("{stdout}\n{stderr}");
    if said.contains("CVM not found") || said.contains("ERR-03-001") {
        print::info(format!("CVM {app_id} is already gone on Phala"));
        return Ok(());
    }
    bail!(
        "deleting CVM {app_id} on Phala failed; it may still be billing, check `phala cvms ls`:\n{}",
        crate::shared::shell::tail(&said, 20)
    )
}

/// What Phala says about a CVM: the node it runs on, that node's gateway, its OS image.
pub struct CvmInfo {
    pub node: String,
    pub gateway: String,
    pub os_image: String,
}

pub fn cvm_info(app_id: &str) -> Result<CvmInfo> {
    let raw = Cmd::new(format!("reading CVM {app_id} from Phala"), "phala")
        .args(["cvms", "get", app_id, "--json"])
        .output()?;
    let v = first_json(&raw)
        .with_context(|| format!("phala printed nothing readable for CVM {app_id}:\n{raw}"))?;
    let gateway = v["gateway"]["base_domain"]
        .as_str()
        .filter(|g| !g.is_empty())
        .with_context(|| format!("Phala reports no gateway for CVM {app_id}; its enclaves are unreachable"))?
        .to_string();
    Ok(CvmInfo {
        node: v["node_info"]["name"].as_str().unwrap_or("?").to_string(),
        gateway,
        os_image: v["os"]["name"].as_str().unwrap_or("?").to_string(),
    })
}
