//! The relayer loop: one per route.
//!
//! Interval-driven and sequential. There is no event subscription, no reorg handling and no
//! speculative work, because the ISM state on the destination chain is the only progress
//! marker - restarting is the same as continuing.
//!
//! Each tick does the whole pipeline for one route, or nothing:
//!
//!   attest   ask the enclave to verify the origin and sign the result
//!   prove    two Groth16 proofs of that one attestation, on local CPU
//!   submit   advance the ISM, authorise the batch, deliver the messages
//!
//! Proving is minutes of CPU, so all routes share one permit. Two routes competing for cores
//! would only make both slower.
//!
//! A batch being complete is the enclave's guarantee, not this loop's - see
//! `tee_node::attest::build_attested_update`. What is this loop's job is not stranding one:
//! abandoning a batch after `updateState` has advanced the trusted height past it would put
//! those leaves behind the next snapshot for good. So an in-flight batch is finished before a
//! new one starts, and the submit scripts skip whatever already landed on chain.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::sync::Semaphore;
use tracing::{info, warn};

use crate::chains::{read_ism_state, Destination};
use crate::commands;
use crate::config::{ChainConfig, RouteConfig};

/// Proving is CPU-bound and local by policy, so routes take turns.
pub fn cpu_prover_permit() -> Arc<Semaphore> {
    Arc::new(Semaphore::new(1))
}

/// Where proved batches are kept, so a crash after proving does not discard the work - and
/// so the attestation API has something to serve.
pub struct ProofStore {
    root: PathBuf,
}

impl ProofStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn path_for(&self, route: &str, height: u64) -> PathBuf {
        self.root.join(route).join(format!("{height}.json"))
    }

    pub fn prepare(&self, route: &str) -> Result<()> {
        std::fs::create_dir_all(self.root.join(route).join("staging"))?;
        // A proving marker found at startup is always stale.
        //
        // The marker is removed when its guard drops, and a guard does not drop when the
        // process is killed - so every restart used to leave one behind, and the dashboard
        // then reported that route as proving for as long as the file sat there. Which is the
        // misreport the marker was added to fix, arriving by another door. On boot this
        // process holds no permit by definition, so anything here belongs to a dead one.
        std::fs::remove_file(self.root.join(route).join("proving")).ok();
        Ok(())
    }

    /// Scratch space for a batch in flight. Kept out of the finished directory so the
    /// attestation API never serves a half-written record.
    pub fn staging(&self, route: &str, name: &str) -> PathBuf {
        self.root.join(route).join("staging").join(name)
    }

    /// Why this route last stopped, for the dashboard to show.
    ///
    /// A route whose proof is finished but whose submission keeps being rejected is, from the
    /// outside, indistinguishable from one still grinding through a proof: both have a batch
    /// in staging and neither has delivered. The difference is the error, and until it was
    /// written down the only place it existed was the journal.
    pub fn record_blocker(&self, route: &str, error: &str, failures: u32) {
        let record = serde_json::json!({
            "error": error,
            "consecutiveFailures": failures,
            "at": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or_default(),
        });
        if let Ok(body) = serde_json::to_vec_pretty(&record) {
            std::fs::write(self.root.join(route).join("blocked.json"), body).ok();
        }
    }

    pub fn clear_blocker(&self, route: &str) {
        std::fs::remove_file(self.root.join(route).join("blocked.json")).ok();
    }

    /// Claim the "this route is on the CPU" marker until the returned guard drops.
    pub fn mark_proving(&self, route: &str) -> ProvingMarker {
        let path = self.root.join(route).join("proving");
        std::fs::write(&path, b"").ok();
        ProvingMarker { path }
    }
}

/// Removes the proving marker however the tick ends, including on error.
pub struct ProvingMarker {
    path: PathBuf,
}

impl Drop for ProvingMarker {
    fn drop(&mut self) {
        std::fs::remove_file(&self.path).ok();
    }
}

/// Drive one route forever, as two tasks that do not wait on each other.
///
/// They used to be one. A route attested, then blocked on the prover permit, then proved,
/// then submitted - so for the whole of a proof it was not looking at the origin at all.
/// Anything that arrived behind the batch being proved went unnoticed until the proof
/// finished, which on this hardware meant hours. Detection was only ever as fast as the tick
/// for a route that happened to be idle, and with one CPU shared by six routes, idle was the
/// exception.
///
/// Splitting them costs nothing in safety because the constraint that made the loop
/// sequential is not the loop: it is the chain. A route's trusted height moves one batch at a
/// time, so there is never a second batch to prove ahead of the first. What the split buys is
/// that noticing is no longer hostage to proving - the scanner records what is pending the
/// moment it lands, and the prover starts the instant the CPU is free rather than at the next
/// tick after it.
pub async fn run_route(
    route: RouteConfig,
    store: Arc<ProofStore>,
    cpu: Arc<Semaphore>,
    tick: Duration,
    lag: u64,
) {
    store.prepare(&route.name).ok();
    let scanning = tokio::spawn(scan_loop(route.clone(), store.clone(), tick, lag));
    let proving = tokio::spawn(prove_loop(route, store, cpu, tick));
    // Neither returns. If either ever does, the route is finished either way.
    let _ = tokio::join!(scanning, proving);
}

/// Look for work and stage it. Never waits for the prover.
async fn scan_loop(route: RouteConfig, store: Arc<ProofStore>, tick: Duration, lag: u64) {
    let mut failures: u32 = 0;
    loop {
        // One batch in flight at a time, because the trusted height only moves one at a time.
        // While one is staged there is nothing to stage, but the scan still runs: it is what
        // keeps the attestable head current, which is how the dashboard shows a route as
        // behind rather than idle.
        if store.staging(&route.name, "attestation.json").exists()
            || store.staging(&route.name, "proved.json").exists()
        {
            tokio::time::sleep(tick).await;
            continue;
        }
        match attest_once(&route, &store, lag).await {
            Ok(()) => {
                failures = 0;
                store.clear_blocker(&route.name);
            }
            Err(e) => {
                let quiet = is_quiet(&e);
                if !quiet {
                    failures = failures.saturating_add(1);
                    store.record_blocker(&route.name, &e.to_string(), failures);
                    warn!(
                        route = %route.name,
                        error = %e,
                        consecutive_failures = failures,
                        retry_in_secs = backoff(tick, failures).as_secs(),
                        "scan failed"
                    );
                } else {
                    failures = 0;
                }
            }
        }
        tokio::time::sleep(backoff(tick, failures)).await;
    }
}

/// Prove and submit whatever the scanner has staged.
async fn prove_loop(
    route: RouteConfig,
    store: Arc<ProofStore>,
    cpu: Arc<Semaphore>,
    tick: Duration,
) {
    let mut failures: u32 = 0;
    loop {
        match prove_staged(&route, &store, &cpu).await {
            Ok(Some(height)) => {
                failures = 0;
                store.clear_blocker(&route.name);
                info!(route = %route.name, height, "batch delivered");
                // Straight round again: if the scanner has already staged the next batch
                // there is no reason to wait a tick to notice.
                continue;
            }
            Ok(None) => failures = 0,
            Err(e) => {
                failures = failures.saturating_add(1);
                store.record_blocker(&route.name, &e.to_string(), failures);
                warn!(
                    route = %route.name,
                    error = %e,
                    consecutive_failures = failures,
                    retry_in_secs = backoff(tick, failures).as_secs(),
                    "submit failed"
                );
            }
        }
        tokio::time::sleep(backoff(tick, failures)).await;
    }
}

/// Wait before the next attempt: the normal tick while healthy, doubling per consecutive
/// failure up to a ceiling. The ceiling matters more than the curve - a route that is broken
/// should still notice within the hour when whatever broke it is fixed.
fn backoff(tick: Duration, failures: u32) -> Duration {
    if failures == 0 {
        return tick;
    }
    let doublings = failures.min(MAX_BACKOFF_DOUBLINGS);
    // The ceiling is never below the normal cadence: backing off must not make a route retry
    // sooner than it would have while healthy.
    let ceiling = MAX_BACKOFF.max(tick);
    tick.saturating_mul(1 << doublings).min(ceiling)
}

const MAX_BACKOFF_DOUBLINGS: u32 = 8;
const MAX_BACKOFF: Duration = Duration::from_secs(30 * 60);

/// "Nothing dispatched" is the common case and not worth a warning every tick.
fn is_quiet(error: &anyhow::Error) -> bool {
    let text = error.to_string();
    text.contains("nothing to attest")
        || text.contains("has not advanced")
        || text.contains("has not passed")
}

async fn attest_once(
    route: &RouteConfig,
    store: &ProofStore,
    lag: u64,
) -> Result<()> {
    let trusted = read_ism_state(&route.destination, &route.ism_id)?;

    let attestation = store.staging(&route.name, "attestation.json");
    let attested = match &route.origin {
        ChainConfig::Celestia {
            rpc,
            archive_rpc,
            merkle_tree_hook_id,
            ..
        } => {
            commands::attest_celestia(
                rpc,
                archive_rpc.as_deref(),
                &route.tee_node_url,
                &trusted,
                route.destination.domain(),
                &route.routers,
                merkle_tree_hook_id,
                lag,
                Some(path_string(&attestation)),
            )
            .await
        }
        ChainConfig::Ethereum {
            execution_rpc,
            archive_rpc,
            beacon_rpc,
            mailbox,
            merkle_tree_hook,
            merkle_tree_base_slot,
            ..
        } => {
            let origin_field =
                |name: &str| format!("an Ethereum origin needs `{name}` in its route config");
            let beacon_rpc = beacon_rpc
                .as_deref()
                .with_context(|| origin_field("beacon_rpc"))?;
            let merkle_tree_hook = merkle_tree_hook
                .as_deref()
                .with_context(|| origin_field("merkle_tree_hook"))?;
            let merkle_tree_base_slot =
                merkle_tree_base_slot.with_context(|| origin_field("merkle_tree_base_slot"))?;
            commands::attest_ethereum(
                beacon_rpc,
                execution_rpc,
                archive_rpc.as_deref(),
                &route.tee_node_url,
                route.checkpoint.as_deref(),
                &trusted,
                route.destination.domain(),
                &route.routers,
                merkle_tree_hook,
                mailbox,
                merkle_tree_base_slot,
                Some(path_string(&attestation)),
            )
            .await
        }
        // An L2 origin is Ethereum's flow plus the storage proof that says which L2 block
        // Ethereum has confirmed.
        ChainConfig::EthereumL2 {
            l2_rpc,
            logs_rpc,
            l1,
            rollup,
            l1_anchor_contract,
            mailbox,
            merkle_tree_hook,
            merkle_tree_base_slot,
            ..
        } => {
            let ChainConfig::Ethereum {
                execution_rpc,
                beacon_rpc,
                archive_rpc,
                ..
            } = &**l1
            else {
                anyhow::bail!("an L2 origin's `l1` must be an Ethereum chain")
            };
            let beacon_rpc = beacon_rpc
                .as_deref()
                .context("an L2 origin's `l1` needs `beacon_rpc`")?;
            // The rollup's storage is proven at the finalized L1 block, which is already
            // outside a public node's window.
            let l1_execution = archive_rpc.as_deref().unwrap_or(execution_rpc);
            commands::attest_l2(
                rollup.parse()?,
                beacon_rpc,
                l1_execution,
                l2_rpc,
                logs_rpc.as_deref(),
                &route.tee_node_url,
                &trusted,
                route.destination.domain(),
                &route.routers,
                l1_anchor_contract,
                merkle_tree_hook,
                mailbox,
                *merkle_tree_base_slot,
                route.checkpoint.as_deref(),
                Some(path_string(&attestation)),
            )
            .await
        }
    };

    attested?;
    Ok(())
}

/// Prove whatever is staged and submit it. Runs alongside the scan, never in front of it.
async fn prove_staged(
    route: &RouteConfig,
    store: &ProofStore,
    cpu: &Arc<Semaphore>,
) -> Result<Option<u64>> {
    // A batch left over from a crash is finished first. Attesting past it would strand its
    // messages permanently - see the module comment.
    if let Some(height) = finish_staged_batch(route, store)? {
        return Ok(Some(height));
    }

    let attestation = store.staging(&route.name, "attestation.json");
    if !attestation.exists() {
        return Ok(None);
    }

    // Hours of CPU are about to be spent on a batch that is no use unless it can be
    // submitted, so establish that it can before spending them rather than after.
    Destination::new(route.destination.clone(), route.ism_id.clone()).preflight()?;

    let proved = store.staging(&route.name, "proved.json");

    if route.attest_only {
        // A TEE ISM verifies the quote itself, so there is nothing to prove and no reason to
        // queue for a core. The staged attestation is already what the destination reads.
        std::fs::rename(&attestation, &proved)?;
    } else {
        // Proving is minutes of CPU, so routes take turns rather than competing for cores.
        info!(route = %route.name, "waiting for the prover");
        let _permit = cpu.acquire().await?;
        // Only one route holds this at a time, so the marker is what lets the dashboard say
        // "proving" about the one that is, and "queued" about the ones that merely want to
        // be.
        let _proving = store.mark_proving(&route.name);
        commands::prove_for_route(
            &route.name,
            &path_string(&attestation),
            &elf_dir(),
            &path_string(&proved),
        )
        .await?;
    }

    let height = submit_and_file(route, store, &proved)?;
    Ok(Some(height))
}

/// Submit a proved batch, move it into the finished directory where the API serves it, and
/// clear the staging area.
///
/// Clearing matters for more than tidiness: a leftover `attestation.json` is what the status
/// endpoint reads to say a route is proving, so one left behind reports a finished batch as
/// still in flight.
fn submit_and_file(route: &RouteConfig, store: &ProofStore, proved: &Path) -> Result<u64> {
    info!(route = %route.name, destination = route.destination.domain(), "submitting");
    Destination::new(route.destination.clone(), route.ism_id.clone()).submit(proved)?;

    let height = std::fs::read(proved)
        .ok()
        .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok())
        .and_then(|record| record["height"].as_u64())
        .unwrap_or_default();

    std::fs::rename(proved, store.path_for(&route.name, height))?;
    let _ = std::fs::remove_file(store.staging(&route.name, "attestation.json"));
    Ok(height)
}

/// Resubmit a batch that was proved but never finished. Returns its height if there was one.
fn finish_staged_batch(route: &RouteConfig, store: &ProofStore) -> Result<Option<u64>> {
    let proved = store.staging(&route.name, "proved.json");
    if !proved.exists() {
        return Ok(None);
    }
    warn!(route = %route.name, "resuming a batch left unfinished by an earlier run");
    submit_and_file(route, store, &proved).map(Some)
}


fn elf_dir() -> String {
    std::env::var("TEE_HYPERLANE_ELF_DIR").unwrap_or_else(|_| "../tee-circuit/elf".to_string())
}

fn path_string(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_then_stops() {
        let tick = Duration::from_secs(120);
        assert_eq!(
            backoff(tick, 0),
            tick,
            "a healthy route keeps its normal cadence"
        );
        assert_eq!(backoff(tick, 1), Duration::from_secs(240));
        assert_eq!(backoff(tick, 3), Duration::from_secs(960));

        // Capped, so a route broken overnight still retries within the hour once it is fixed.
        assert_eq!(backoff(tick, 20), MAX_BACKOFF);
    }

    #[test]
    fn a_long_tick_is_never_shortened_by_backing_off() {
        let tick = Duration::from_secs(45 * 60);
        assert_eq!(backoff(tick, 5), tick.max(MAX_BACKOFF));
    }
}
