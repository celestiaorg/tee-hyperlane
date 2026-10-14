//! `teeism enclaves up`: deploy the shared CVM (or each `--chain` alone) on Phala and record
//! the identity every ISM will pin.

use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::time::Duration;

use super::*;
use crate::shared::chains;
use crate::shared::print;
use crate::shared::record::{Cvm, Enclave};
use crate::shared::settings::Ctx;
use crate::shared::shell::{first_json, Cmd};
use crate::steps::images::{self, compose_file};

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
    pub os_image: String,
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
        deploy_cvm(ctx, target, &a.os_image, &mut authenticated).await?;
    }

    print::step("reading each enclave's identity from its quote");
    let ident_dir = ctx.state_dir.join("identity");
    std::fs::create_dir_all(&ident_dir)?;
    for target in &targets {
        for chain in chains_in(target) {
            super::identity::record_identity(ctx, chain).await?;
        }
    }
    Ok(())
}

async fn deploy_cvm(ctx: &Ctx, target: &str, os_image: &str, authenticated: &mut bool) -> Result<()> {
    let compose = compose_file(ctx, target);
    let file = std::fs::read_to_string(&compose)
        .with_context(|| format!("no compose for {target} at {}", compose.display()))?;
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
            if measured_compose(&id)? == file
                && cvm.os_image.as_deref().unwrap_or(DEFAULT_OS_IMAGE) == os_image
            {
                print::info(format!(
                    "{target} CVM {} already runs this compose on {os_image}",
                    cvm.app_id
                ));
                record_urls(&mut d, target, &cvm.app_id)?;
                return ctx.save(&d);
            }
            print::warn(format!(
                "the recorded {target} CVM {} runs a different compose or OS image; deploying a new one. \
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
    let instance = ctx.var_or("PHALA_INSTANCE_TYPE", DEFAULT_INSTANCE);
    let node = ctx.var_or("PHALA_NODE_ID", DEFAULT_NODE);
    print::step(format!("deploying {name} ({instance}, node {node}, {os_image})"));
    // --no-dev-os matters: with an SSH key on this machine the CLI otherwise provisions a dev
    // image that allows a shell in the CVM, which would make its measurements meaningless.
    let raw = Cmd::new(format!("deploying the {target} CVM on Phala"), "phala")
        .args(["deploy", "--name", &name, "--compose"])
        .arg(&compose)
        .args([
            "--instance-type",
            &instance,
            "--node-id",
            &node,
            "--image",
            os_image,
            "--no-dev-os",
            "--wait",
            "--json",
        ])
        .output()?;
    let app_id = first_json(&raw)
        .and_then(|v| find_key(&v, &["app_id", "appId"]))
        .with_context(|| format!("could not read the app id for {target} from phala's output; check `phala cvms ls` for a stray {name}:\n{raw}"))?;
    if let Some(old) = d.enclaves.cvms.insert(
        target.to_string(),
        Cvm {
            name: name.clone(),
            app_id: app_id.clone(),
            os_image: Some(os_image.to_string()),
        },
    ) {
        d.enclaves.retired.push(old);
    }
    ctx.save(&d)?;
    print::kv("app id", &app_id);

    for chain in chains_in(target) {
        let url = chains::enclave_url(&app_id, chain)?;
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
    record_urls(&mut d, target, &app_id)?;
    ctx.save(&d)
}

/// Point every chain this CVM serves at it. A chain deployed alone later takes over its own.
fn record_urls(d: &mut crate::shared::record::Deployment, target: &str, app_id: &str) -> Result<()> {
    for chain in chains_in(target) {
        let e = d.enclaves.chains.entry(chain.to_string()).or_default();
        let url = chains::enclave_url(app_id, chain)?;
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
