//! `teeism pccs status`: when each chain's collateral expires. Read only.

use alloy::primitives::{Address, FixedBytes, U256};
use anyhow::{bail, Context, Result};

use super::{ca, ICollateral};
use crate::shared::chains;
use crate::shared::evm;
use crate::shared::print;
use crate::shared::record::Pccs;
use crate::shared::settings::Ctx;

pub struct Validity {
    pub label: String,
    /// (not before, not after), or why it could not be read.
    pub window: Result<(u64, u64), String>,
}

/// When each piece of collateral the verifier reads stops being valid.
pub async fn validity(rpc: &str, p: &Pccs, fmspc: &str) -> Result<Vec<Validity>> {
    let r = evm::reader(rpc)?;
    let fm: [u8; 6] = hex::decode(fmspc)
        .ok()
        .and_then(|b| b.try_into().ok())
        .with_context(|| format!("FMSPC {fmspc} is not 6 bytes of hex"))?;
    let mut rows = Vec::new();
    let read = |dao: Address, key: Result<FixedBytes<32>, String>| {
        let r = r.clone();
        async move {
            let key = key?;
            ICollateral::new(dao, &r)
                .getCollateralValidity(key)
                .call()
                .await
                .map(|v| (v._0, v._1))
                .map_err(|e| e.to_string())
        }
    };
    let key = |res: Result<FixedBytes<32>, alloy::contract::Error>| {
        res.map_err(|e| format!("deriving the key: {e}"))
    };
    for (n, d) in &p.evals {
        let k = key(ICollateral::new(d.fmspc_tcb, &r)
            .FMSPC_TCB_KEY(1, fm.into(), 3)
            .call()
            .await);
        rows.push(Validity {
            label: format!("TDX TCB info ({n})"),
            window: read(d.fmspc_tcb, k).await,
        });
        let k = key(ICollateral::new(d.qe_identity, &r)
            .ENCLAVE_ID_KEY(U256::from(2), U256::from(4))
            .call()
            .await);
        rows.push(Validity {
            label: format!("TD_QE identity ({n})"),
            window: read(d.qe_identity, k).await,
        });
    }
    for (label, id) in [("SGX evaluation numbers", 0u8), ("TDX evaluation numbers", 1)] {
        let k = key(ICollateral::new(p.tcb_eval_dao, &r).TCB_EVAL_KEY(id).call().await);
        rows.push(Validity {
            label: label.into(),
            window: read(p.tcb_eval_dao, k).await,
        });
    }
    for (label, ca, crl) in [
        ("root CA CRL", ca::ROOT, true),
        ("PCK processor CRL", ca::PROCESSOR, true),
        ("PCK platform CRL", ca::PLATFORM, true),
        ("root CA certificate", ca::ROOT, false),
        ("TCB signing certificate", ca::SIGNING, false),
        ("PCK processor certificate", ca::PROCESSOR, false),
        ("PCK platform certificate", ca::PLATFORM, false),
    ] {
        let k = key(ICollateral::new(p.pcs_dao, &r).PCS_KEY(ca, crl).call().await);
        rows.push(Validity {
            label: label.into(),
            window: read(p.pcs_dao, k).await,
        });
    }
    Ok(rows)
}

/// `teeism pccs status`: when each chain's collateral expires.
pub async fn run(ctx: &Ctx, chains: &[String], fmspc: &str, warn_days: u64) -> Result<()> {
    let d = ctx.deployment()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    let mut problems = Vec::new();
    for chain in chains::evm_selection(chains)? {
        let Ok(p) = super::pccs_for(&d, chain) else {
            problems.push(format!("{}: no PCCS deployed", chain.label));
            continue;
        };
        print::step(chain.label);
        for row in validity(&chain.rpc(ctx), p, fmspc).await? {
            match row.window {
                Err(e) => {
                    print::bad(format!("{:<28} ERROR    {e}", row.label));
                    problems.push(format!("{}: {} unreadable", chain.label, row.label));
                }
                Ok((_, 0)) => {
                    print::bad(format!("{:<28} MISSING  nothing published", row.label));
                    problems.push(format!("{}: {} missing", chain.label, row.label));
                }
                Ok((_, until)) => {
                    let left = (until as i64 - now as i64) / 86400;
                    if until < now {
                        print::bad(format!("{:<28} EXPIRED  {}", row.label, date(until)));
                        problems.push(format!("{}: {} expired", chain.label, row.label));
                    } else if (left as u64) < warn_days {
                        print::bad(format!("{:<28} EXPIRING {} ({left}d)", row.label, date(until)));
                        problems.push(format!("{}: {} expires in {left}d", chain.label, row.label));
                    } else {
                        print::ok(format!("{:<28} {} ({left}d)", row.label, date(until)));
                    }
                }
            }
        }
    }
    if !problems.is_empty() {
        bail!(
            "collateral needs attention:\n  - {}\nEXPIRING, EXPIRED and MISSING are fixed by `teeism pccs seed`; \
             ERROR means the record or the endpoint is wrong, not the collateral",
            problems.join("\n  - ")
        );
    }
    print::ok(format!("all collateral valid for more than {warn_days} days"));
    Ok(())
}

/// A unix time as `YYYY-MM-DD`.
fn date(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    // Civil-from-days (Howard Hinnant), to avoid a date dependency for one format.
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{:04}-{:02}-{:02}", if m <= 2 { y + 1 } else { y }, m, d)
}
