//! `teeism teardown`: stop and delete everything this host runs for the bridge, and the CVMs
//! on Phala, so the next deploy starts from nothing.
//!
//! Without --yes it only lists what it would do. Contracts already on the EVM chains cannot be
//! deleted; they stay there, unused. Secrets are never deleted: devnet/.env is not touched,
//! and a key file found under .state is moved into it first.

use anyhow::{Context, Result};

use crate::ctx::Ctx;
use crate::enclaves;
use crate::mocha;
use crate::out;
use crate::run::Cmd;

/// The units this host has run for the bridge, current and retired.
const UNITS: [&str; 4] = ["teeism-relayer", "teeism-gas-oracle", "teeism-api", "bridge-ui"];
const CONTAINERS: [&str; 3] = ["teeism-gateway", crate::chain::CONTAINER, mocha::CONTAINER];

/// Every CVM this deployment knows of: the current record, and the per-file record older
/// hosts kept in .state/out/enclave-app-id-<target>.
fn known_cvms(ctx: &Ctx) -> Result<Vec<(String, String)>> {
    let mut out: Vec<(String, String)> = ctx
        .deployment()?
        .enclaves
        .cvms
        .iter()
        .map(|(t, c)| (t.clone(), c.app_id.clone()))
        .collect();
    for c in ctx.deployment()?.enclaves.retired {
        out.push(("retired".into(), c.app_id));
    }
    if let Ok(dir) = std::fs::read_dir(ctx.state_dir.join("out")) {
        for f in dir.flatten() {
            let name = f.file_name().to_string_lossy().into_owned();
            if let Some(target) = name.strip_prefix("enclave-app-id-") {
                let id = std::fs::read_to_string(f.path())?.trim().to_string();
                if !out.iter().any(|(_, a)| *a == id) {
                    out.push((target.to_string(), id));
                }
            }
        }
    }
    Ok(out)
}

pub async fn run(ctx: &mut Ctx, yes: bool) -> Result<()> {
    let cvms = known_cvms(ctx)?;
    out::step(if yes { "tearing down" } else { "would tear down (dry run; pass --yes to do it)" });
    for u in UNITS {
        out::info(format!("systemd unit {u}: stop, disable, remove"));
    }
    for c in CONTAINERS {
        out::info(format!("container {c}: remove"));
    }
    for (target, id) in &cvms {
        out::info(format!("Phala CVM {id} ({target}): delete"));
    }
    out::info(format!("{}: delete, keeping only secrets (moved into {})", ctx.state_dir.display(), ctx.env_file().display()));
    out::info("bridge-app/.env.local and bridge-app/dist: delete");

    // CVMs named like ours that no record mentions: reported, never deleted, since this host
    // cannot know whose they are.
    if let Ok(list) = Cmd::new("listing CVMs on Phala", "phala").args(["cvms", "ls", "--json"]).output() {
        if let Some(v) = crate::run::first_json(&list) {
            for item in v["items"].as_array().into_iter().flatten() {
                let name = item["cvmName"].as_str().unwrap_or("");
                let id = item["appId"].as_str().unwrap_or("");
                if name.starts_with("teeism") && !cvms.iter().any(|(_, a)| a == id) {
                    out::warn(format!("CVM {name} ({id}) is not in this deployment's record; left alone. Delete it with `teeism enclaves delete --app-id {id}` if it is a stray"));
                }
            }
        }
    }
    if !yes {
        return Ok(());
    }

    for u in UNITS {
        let (s, _, _) = Cmd::new(format!("checking for {u}"), "systemctl").args(["cat", u]).capture().unwrap_or_else(|_| (fail_status(), String::new(), String::new()));
        if !s.success() {
            continue;
        }
        Cmd::new(format!("stopping {u}"), "sudo").args(["systemctl", "disable", "--now", u]).run()?;
        Cmd::new(format!("removing {u}'s unit file"), "sudo").args(["rm", "-f", &format!("/etc/systemd/system/{u}.service")]).run()?;
        out::info(format!("removed {u}"));
    }
    let _ = Cmd::new("reloading systemd", "sudo").args(["systemctl", "daemon-reload"]).run();

    for c in CONTAINERS {
        let (s, _, _) = Cmd::new(format!("checking for container {c}"), "docker").args(["inspect", c]).capture()?;
        if s.success() {
            Cmd::new(format!("removing container {c}"), "docker").args(["rm", "-f", c]).output()?;
            out::info(format!("removed container {c}"));
        }
    }

    for (target, id) in &cvms {
        out::info(format!("deleting CVM {id} ({target})"));
        enclaves::delete(id).with_context(|| format!("CVM {id} is still billing; check `phala cvms ls`"))?;
    }

    // Secrets kept under .state on older hosts go to devnet/.env before .state goes.
    let alchemy = ctx.state_dir.join("alchemy-base-key");
    if alchemy.is_file() && ctx.var("ALCHEMY_KEY").is_none() {
        let key = std::fs::read_to_string(&alchemy)?.trim().to_string();
        ctx.set_env("ALCHEMY_KEY", &key)?;
        out::info(format!("moved .state/alchemy-base-key into {} as ALCHEMY_KEY", ctx.env_file().display()));
    }
    let evm_key = ctx.state_dir.join("evm-key");
    if evm_key.is_file() && ctx.var("EVM_PRIVATE_KEY").is_none() {
        let key = std::fs::read_to_string(&evm_key)?.trim().to_string();
        ctx.set_env("EVM_PRIVATE_KEY", &key)?;
        out::info(format!("moved .state/evm-key into {} as EVM_PRIVATE_KEY", ctx.env_file().display()));
    }
    if ctx.state_dir.exists() {
        // The node containers wrote some of this as their own user; docker can always remove it.
        let (s, _, _) = Cmd::new("deleting the state directory", "rm").arg("-rf").arg(&ctx.state_dir).capture()?;
        if !s.success() || ctx.state_dir.exists() {
            Cmd::new("deleting container-owned state", "docker")
                .args(["run", "--rm", "-v", &format!("{}:/s", ctx.state_dir.display()), "alpine:3.20", "sh", "-c", "rm -rf /s/* /s/.[!.]*"])
                .output()?;
            std::fs::remove_dir_all(&ctx.state_dir).with_context(|| format!("deleting {}", ctx.state_dir.display()))?;
        }
        out::info(format!("deleted {}", ctx.state_dir.display()));
    }
    for p in ["bridge-app/.env.local", "bridge-app/dist"] {
        let path = ctx.repo.join(p);
        if path.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else if path.is_file() {
            std::fs::remove_file(&path)?;
        }
    }
    out::ok("torn down. The contracts on the EVM chains remain on chain, unused");
    Ok(())
}

fn fail_status() -> std::process::ExitStatus {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(256)
    }
}
