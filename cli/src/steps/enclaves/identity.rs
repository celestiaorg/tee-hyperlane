//! `teeism enclaves identity`: read what each recorded enclave's quote says, into the record.

use anyhow::{bail, Context, Result};

use super::*;
use crate::shared::print;
use crate::shared::settings::Ctx;
use crate::shared::shell::Cmd;

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
pub async fn run(ctx: &Ctx, chains: Vec<String>) -> Result<()> {
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
