//! `teeism services`: the relayer and the gas oracle as systemd units on the host.
//!
//! The units are deploy/server/*.service. Installing needs root, so these commands run `sudo`
//! and it asks for the password on the terminal.

use anyhow::{bail, Context, Result};
use std::time::Duration;

use crate::shared::print;
use crate::shared::settings::Ctx;
use crate::shared::shell::Cmd;
use crate::steps::config;

pub const UNITS: [&str; 2] = ["teeism-relayer", "teeism-gas-oracle"];

fn sudo(what: &str, args: &[&str]) -> Result<()> {
    Cmd::new(what, "sudo").args(args).run()
}

pub async fn install(ctx: &Ctx) -> Result<()> {
    config::write(ctx)?;
    for unit in UNITS {
        let src = ctx.repo.join(format!("deploy/server/{unit}.service"));
        if !src.is_file() {
            bail!("no unit file at {}", src.display());
        }
        sudo(
            &format!("installing {unit}"),
            &[
                "cp",
                &src.to_string_lossy(),
                &format!("/etc/systemd/system/{unit}.service"),
            ],
        )?;
    }
    sudo("reloading systemd", &["systemctl", "daemon-reload"])?;
    let mut args = vec!["systemctl", "enable", "--now"];
    args.extend(UNITS);
    sudo("enabling and starting the services", &args)?;
    restart(ctx).await
}

pub async fn restart(ctx: &Ctx) -> Result<()> {
    config::write(ctx)?;
    let mut args = vec!["systemctl", "restart"];
    args.extend(UNITS);
    sudo("restarting the services", &args)?;
    wait_for_relayer(ctx).await
}

pub fn stop() -> Result<()> {
    let mut args = vec!["systemctl", "stop"];
    args.extend(UNITS);
    sudo("stopping the services", &args)
}

/// Block until the relayer's API answers with every configured route.
pub async fn wait_for_relayer(ctx: &Ctx) -> Result<()> {
    let api = ctx.var_or("RELAYER_API", "http://localhost:3001");
    let want = std::fs::read_to_string(ctx.coprocessor_config())?
        .matches("[[routes]]")
        .count();
    let mut last = String::new();
    for _ in 0..60 {
        match reqwest::Client::new()
            .get(format!("{api}/api/status"))
            .timeout(Duration::from_secs(5))
            .send()
            .await
        {
            Ok(r) if r.status().is_success() => {
                let v: serde_json::Value = r.json().await.context("the relayer's /api/status is not JSON")?;
                let n = v.as_array().map(|a| a.len()).unwrap_or(0);
                if n == want {
                    for route in v.as_array().into_iter().flatten() {
                        print::info(format!(
                            "{} ism {}",
                            route["name"].as_str().unwrap_or("?"),
                            route["ism"].as_str().unwrap_or("?")
                        ));
                    }
                    print::ok(format!("relayer up with {n} routes"));
                    return Ok(());
                }
                last = format!("{n} of {want} routes");
            }
            Ok(r) => last = format!("HTTP {}", r.status()),
            Err(e) => last = e.to_string(),
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    bail!("the relayer did not come up with its {want} routes within 5 minutes ({last}); see `journalctl -u teeism-relayer -n 100`")
}
