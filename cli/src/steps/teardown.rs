//! `teeism teardown`: stop and delete everything this host runs for the bridge, and the CVMs
//! on Phala, so the next deploy starts from nothing.
//!
//! Without --yes it only lists what it would do. Contracts already on the EVM chains cannot be
//! deleted; they stay there, unused. Secrets are never deleted: devnet/.env is not touched.

use anyhow::{Context, Result};

use crate::shared::print;
use crate::shared::settings::Ctx;
use crate::shared::shell::Cmd;
use crate::steps::celestia::mocha;
use crate::steps::enclaves;

/// Every unit file `teeism services install` puts in /etc/systemd/system, timers before the
/// services they start.
const UNITS: [&str; 4] = [
    "teeism-upstream.timer",
    "teeism-upstream.service",
    "teeism-relayer.service",
    "teeism-gas-oracle.service",
];
const CONTAINERS: [&str; 3] = [
    "teeism-gateway",
    crate::steps::celestia::up::CONTAINER,
    mocha::CONTAINER,
];

/// Every CVM this deployment knows of, current and retired.
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
    Ok(out)
}

pub async fn run(ctx: &mut Ctx, yes: bool) -> Result<()> {
    let cvms = known_cvms(ctx)?;
    print::step(if yes {
        "tearing down"
    } else {
        "would tear down (dry run; pass --yes to do it)"
    });
    for u in UNITS {
        print::info(format!("systemd unit {u}: stop, disable, remove"));
    }
    for c in CONTAINERS {
        print::info(format!("container {c}: remove"));
    }
    for (target, id) in &cvms {
        print::info(format!("Phala CVM {id} ({target}): delete"));
    }
    print::info(format!(
        "{}: delete ({} is kept)",
        ctx.state_dir.display(),
        ctx.env_file().display()
    ));
    print::info("bridge-app/.env.local and bridge-app/dist: delete");

    // CVMs named like ours that no record mentions: reported, never deleted, since this host
    // cannot know whose they are.
    if let Ok(list) = Cmd::new("listing CVMs on Phala", "phala")
        .args(["cvms", "ls", "--json"])
        .output()
    {
        if let Some(v) = crate::shared::shell::first_json(&list) {
            for item in v["items"].as_array().into_iter().flatten() {
                let name = item["cvmName"].as_str().unwrap_or("");
                let id = item["appId"].as_str().unwrap_or("");
                if name.starts_with("teeism") && !cvms.iter().any(|(_, a)| a == id) {
                    print::warn(format!("CVM {name} ({id}) is not in this deployment's record; left alone. Delete it with `teeism enclaves delete --app-id {id}` if it is a stray"));
                }
            }
        }
    }
    if !yes {
        return Ok(());
    }

    for u in UNITS {
        let (s, _, _) = Cmd::new(format!("checking for {u}"), "systemctl")
            .args(["cat", u])
            .capture()
            .unwrap_or_else(|_| (fail_status(), String::new(), String::new()));
        if !s.success() {
            continue;
        }
        Cmd::new(format!("stopping {u}"), "sudo")
            .args(["systemctl", "disable", "--now", u])
            .run()?;
        Cmd::new(format!("removing {u}'s unit file"), "sudo")
            .args(["rm", "-f", &format!("/etc/systemd/system/{u}")])
            .run()?;
        print::info(format!("removed {u}"));
    }
    let _ = Cmd::new("reloading systemd", "sudo")
        .args(["systemctl", "daemon-reload"])
        .run();

    for c in CONTAINERS {
        let (s, _, _) = Cmd::new(format!("checking for container {c}"), "docker")
            .args(["inspect", c])
            .capture()?;
        if s.success() {
            Cmd::new(format!("removing container {c}"), "docker")
                .args(["rm", "-f", c])
                .output()?;
            print::info(format!("removed container {c}"));
        }
    }

    for (target, id) in &cvms {
        print::info(format!("deleting CVM {id} ({target})"));
        enclaves::up::delete(id)?;
    }

    if ctx.state_dir.exists() {
        // The node containers wrote some of this as their own user; docker can always remove it.
        let (s, _, _) = Cmd::new("deleting the state directory", "rm")
            .arg("-rf")
            .arg(&ctx.state_dir)
            .capture()?;
        if !s.success() || ctx.state_dir.exists() {
            Cmd::new("deleting container-owned state", "docker")
                .args([
                    "run",
                    "--rm",
                    "-v",
                    &format!("{}:/s", ctx.state_dir.display()),
                    "alpine:3.20",
                    "sh",
                    "-c",
                    "rm -rf /s/* /s/.[!.]*",
                ])
                .output()?;
            std::fs::remove_dir_all(&ctx.state_dir)
                .with_context(|| format!("deleting {}", ctx.state_dir.display()))?;
        }
        print::info(format!("deleted {}", ctx.state_dir.display()));
    }
    for p in ["bridge-app/.env.local", "bridge-app/dist"] {
        let path = ctx.repo.join(p);
        if path.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else if path.is_file() {
            std::fs::remove_file(&path)?;
        }
    }
    print::ok("torn down. The contracts on the EVM chains remain on chain, unused");
    Ok(())
}

fn fail_status() -> std::process::ExitStatus {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(256)
    }
}
