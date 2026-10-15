//! `teeism mocha up`: a celestia-node light node on Mocha, which Eden's headers are read
//! through (Eden posts them to Mocha). Takes about half an hour to sync the first time; until
//! then Eden's ISM cannot be created and `teeism isms celestia` says so.

use anyhow::{Context, Result};
use serde_json::Value;

use crate::shared::chains::MOCHA_LIGHT_IMAGE;
use crate::shared::print;
use crate::shared::settings::Ctx;
use crate::shared::shell::Cmd;
use crate::steps::celestia::up::{set_toml, tv};

pub const CONTAINER: &str = "mocha-light";

pub fn dir(ctx: &Ctx) -> std::path::PathBuf {
    ctx.state_dir.join("mocha-light")
}

fn container_state() -> Result<Option<String>> {
    let (status, stdout, _) = Cmd::new("inspecting the mocha light node", "docker")
        .args(["inspect", "-f", "{{.State.Status}}", CONTAINER])
        .capture()?;
    Ok(status.success().then(|| stdout.trim().to_string()))
}

pub async fn up(ctx: &Ctx) -> Result<()> {
    let d = dir(ctx);
    std::fs::create_dir_all(&d).with_context(|| format!("creating {}", d.display()))?;
    let uid = format!(
        "{}:{}",
        Cmd::new("reading the uid", "id").arg("-u").output()?.trim(),
        Cmd::new("reading the gid", "id").arg("-g").output()?.trim()
    );
    let mount = format!("{}:/home/celestia", d.display());

    if !d.join("config.toml").is_file() {
        print::step("initialising the mocha light node");
        Cmd::new("initialising the mocha light node", "docker")
            .args([
                "run",
                "--rm",
                "-v",
                &mount,
                "-u",
                &uid,
                MOCHA_LIGHT_IMAGE,
                "celestia",
                "light",
                "init",
                "--p2p.network",
                "mocha",
            ])
            .run()?;
    }
    // Applied on every run, so a node initialised by an earlier, interrupted run still gets it.
    // The long pruning window keeps the headers Eden's ISM may need to prove from: a quiet
    // route's trusted height can sit far behind the head.
    set_toml(&d.join("config.toml"), "Header.Syncer", "PruningWindow", tv("800h0m0s"))?;

    match container_state()?.as_deref() {
        Some("running") => print::info(format!("{CONTAINER} already running")),
        Some(other) => {
            print::step(format!("starting {CONTAINER} (it was {other})"));
            Cmd::new("starting the mocha light node", "docker")
                .args(["start", CONTAINER])
                .output()?;
        }
        None => {
            print::step(format!("starting {CONTAINER}"));
            Cmd::new("starting the mocha light node", "docker")
                .args([
                    "run",
                    "-d",
                    "--name",
                    CONTAINER,
                    "--restart",
                    "unless-stopped",
                    "-p",
                    "127.0.0.1:26658:26658",
                    "-v",
                    &mount,
                    "-u",
                    &uid,
                    MOCHA_LIGHT_IMAGE,
                    "celestia",
                    "light",
                    "start",
                    "--p2p.network",
                    "mocha",
                    "--rpc.addr",
                    "0.0.0.0",
                    "--rpc.port",
                    "26658",
                    "--rpc.skip-auth",
                ])
                .output()?;
        }
    }
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    match sync_state(&ctx.var_or("EDEN_DA_RPC", "http://localhost:26658")).await {
        Ok((h, to)) => print::kv("synced", format!("{h} of {to}")),
        Err(e) => print::warn(format!(
            "not answering yet ({e:#}); `docker logs {CONTAINER}` shows its progress"
        )),
    }
    Ok(())
}

/// (synced height, network height) from the light node's JSON-RPC.
pub async fn sync_state(rpc: &str) -> Result<(u64, u64)> {
    let body = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "header.SyncState", "params": []});
    let v: Value = reqwest::Client::new()
        .post(rpc)
        .json(&body)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .with_context(|| format!("the light node at {rpc} is not answering"))?
        .json()
        .await
        .context("the light node's answer is not JSON")?;
    let r = &v["result"];
    let h = r["height"]
        .as_u64()
        .with_context(|| format!("no sync state in {v}"))?;
    Ok((h, r["to_height"].as_u64().unwrap_or(h)))
}
