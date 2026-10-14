//! `teeism status`: what is deployed, and whether each piece answers. Read only.

use anyhow::Result;
use std::time::Duration;

use crate::shared::celestia_cli;
use crate::shared::chains::EVM_CHAINS;
use crate::shared::print;
use crate::shared::settings::Ctx;

fn show(name: &str, v: &Option<String>) {
    print::kv(name, v.as_deref().unwrap_or("-"));
}

pub async fn run(ctx: &Ctx) -> Result<()> {
    let d = ctx.deployment()?;
    print::step("celestia");
    match celestia_cli::height(ctx).await {
        Ok(h) => print::kv("height", h),
        Err(e) => print::bad(format!("down: {e:#}")),
    }
    let c = &d.celestia;
    show("mailbox", &c.mailbox);
    show("merkle tree hook", &c.merkle_hook);
    show("paymaster", &c.igp);
    show("routing ism", &c.routing_ism);
    show("TIA token", &c.tia_token);
    show("teeUSD token", &c.teeusd_token);
    for (origin, ism) in &c.isms {
        print::kv(&format!("ism for {origin}"), ism);
    }
    show("relayer key", &c.relayer);
    show("user key", &c.user);

    print::step("enclaves");
    for (target, cvm) in &d.enclaves.cvms {
        print::kv(
            &format!("cvm {target}"),
            format!(
                "{} ({}, {})",
                cvm.app_id,
                cvm.name,
                cvm.os_image.as_deref().unwrap_or("?")
            ),
        );
    }
    for (chain, e) in &d.enclaves.chains {
        let up = matches!(
            reqwest::Client::new().get(format!("{}/health", e.url)).timeout(Duration::from_secs(10)).send().await,
            Ok(r) if r.status().is_success()
        );
        let state = if up { "up" } else { "NOT ANSWERING" };
        print::kv(chain, format!("{state} {}", e.url));
        print::kv("  identity", e.identity_digest.as_deref().unwrap_or("-"));
        print::kv("  measurements", e.measurements.as_deref().unwrap_or("-"));
    }

    for chain in &EVM_CHAINS {
        let Some(e) = d.evm(chain.name) else { continue };
        print::step(chain.label);
        let a =
            |v: Option<alloy::primitives::Address>| v.map(|a| a.to_string()).unwrap_or_else(|| "-".into());
        if let Some(p) = &e.pccs {
            print::kv("dcap entrypoint", p.entrypoint);
            print::kv("pccs namespace", &p.namespace);
        }
        if chain.canonical.is_none() {
            print::kv("mailbox", a(e.mailbox));
            print::kv("merkle tree hook", a(e.merkle_hook));
        }
        print::kv("ism", a(e.ism));
        print::kv("TIA router", a(e.tia_router));
        print::kv("teeUSD router", a(e.teeusd_router));
    }

    print::step("routes");
    let api = ctx.var_or("RELAYER_API", "http://localhost:3001");
    let routes: serde_json::Value = match reqwest::Client::new()
        .get(format!("{api}/api/v1/routes"))
        .timeout(Duration::from_secs(10))
        .send()
        .await
    {
        Ok(r) => r.json().await.unwrap_or_default(),
        Err(e) => anyhow::bail!("the relayer is not answering at {api}: {e}"),
    };
    let mut stale = Vec::new();
    // With the hourly heartbeat every ISM's trusted state is under an hour old plus the
    // origin's own lag; much older means the route is not advancing.
    for r in routes.as_array().into_iter().flatten() {
        let name = r["name"].as_str().unwrap_or("?");
        let ism = &r["ismReading"];
        let age = ism["readAt"]
            .as_u64()
            .unwrap_or(0)
            .saturating_sub(ism["timestamp"].as_u64().unwrap_or(0));
        let line = format!(
            "{name:<24} {:<6} ism height {} (head {}), state {}m old",
            r["status"].as_str().unwrap_or("?"),
            ism["height"],
            r["originHead"]["height"],
            age / 60
        );
        if age > 2 * 3600 || r["status"] != "ok" {
            print::bad(line);
            stale.push(name.to_string());
        } else {
            print::ok(line);
        }
    }
    if !stale.is_empty() {
        anyhow::bail!(
            "{} route(s) not advancing: {}. `journalctl -u teeism-relayer` says why",
            stale.len(),
            stale.join(", ")
        );
    }
    Ok(())
}
