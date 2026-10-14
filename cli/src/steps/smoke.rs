//! `teeism smoke`: the end-to-end test. Real transfers on every route, both assets, both
//! directions, each waited on until it arrives:
//!
//!   1. Celestia -> each EVM chain: `user` sends to the EVM signer; its balance must rise.
//!   2. each EVM chain -> Celestia: the EVM signer sends back to the `smoke` key, which nothing
//!      else pays, so its balance rises only when a transfer lands.

use alloy::primitives::{Address, U256};
use alloy::sol_types::SolCall;
use anyhow::{bail, Context, Result};
use std::time::{Duration, Instant};

use crate::shared::celestia_cli::{self, Appd};
use crate::shared::chains::{self, celestia as cel, EvmChain};
use crate::shared::evm::Chain;
use crate::shared::print;
use crate::shared::settings::Ctx;
use crate::steps::warp::IRouter;

/// Ceiling on what a Celestia-origin transfer may pay the paymaster.
const MAX_FEE: &str = "10000000utia";

struct Leg {
    label: String,
    started: Instant,
    done: Option<Duration>,
}

fn router(d: &crate::shared::record::Deployment, chain: &EvmChain, symbol: &str) -> Result<Address> {
    let e = d
        .evm(chain.name)
        .with_context(|| format!("nothing deployed on {}", chain.label))?;
    let r = if symbol == "TIA" {
        e.tia_router
    } else {
        e.teeusd_router
    };
    r.with_context(|| format!("no {symbol} router on {}", chain.label))
}

pub async fn run(ctx: &Ctx, selection: &[String], amount: u128, timeout_mins: u64) -> Result<()> {
    celestia_cli::wait_for_chain(ctx).await?;
    let appd = Appd::new(ctx)?;
    let d = ctx.deployment()?;
    let chains = chains::evm_selection(selection)?;
    let deadline = Duration::from_secs(timeout_mins * 60);
    let smoke = d
        .celestia
        .smoke
        .clone()
        .context("no smoke key recorded; run `teeism celestia up`")?;
    let smoke32 = celestia_cli::bech32_to_hex32(&smoke)?;
    let tokens = [
        (
            "TIA",
            d.celestia.tia_token.clone().context("no TIA token")?,
            "utia".to_string(),
        ),
        (
            "teeUSD",
            d.celestia.teeusd_token.clone().context("no teeUSD token")?,
            d.teeusd_denom().unwrap_or_default(),
        ),
    ];

    let mut conns = Vec::new();
    for chain in &chains {
        conns.push(Chain::connect(ctx, chain).await?);
    }

    // ---------------------------------------------------------------- Celestia -> EVM
    print::step(format!(
        "Celestia -> EVM: {amount} base units of each asset to each chain"
    ));
    let mut pending = Vec::new();
    for c in &conns {
        for (symbol, token, _) in &tokens {
            let r = router(&d, c.chain, symbol)?;
            let before = IRouter::new(r, &c.p).balanceOf(c.from).call().await?;
            let recipient = celestia_cli::pad32(&c.from);
            appd.tx(
                &format!("sending {symbol} to {}", c.chain.label),
                "user",
                &[
                    "warp",
                    "transfer",
                    token,
                    &c.chain.domain.to_string(),
                    &recipient,
                    &amount.to_string(),
                    "--max-hyperlane-fee",
                    MAX_FEE,
                ],
            )?;
            print::info(format!("sent {symbol} to {}", c.chain.label));
            pending.push((
                c,
                *symbol,
                r,
                before,
                Leg {
                    label: format!("celestia-to-{} {symbol}", c.chain.name),
                    started: Instant::now(),
                    done: None,
                },
            ));
        }
    }
    let mut legs = Vec::new();
    while pending.iter().any(|p| p.4.done.is_none()) {
        for (c, _symbol, r, before, leg) in pending.iter_mut().filter(|p| p.4.done.is_none()) {
            let now = IRouter::new(*r, &c.p).balanceOf(c.from).call().await?;
            if now >= *before + U256::from(amount) {
                leg.done = Some(leg.started.elapsed());
                print::ok(format!(
                    "{} arrived after {}s",
                    leg.label,
                    leg.started.elapsed().as_secs()
                ));
            } else if leg.started.elapsed() > deadline {
                bail!("{} did not arrive within {timeout_mins} minutes; see `teeism status` and the relayer's /api/v1/inbox", leg.label);
            }
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
    legs.extend(pending.into_iter().map(|p| p.4));

    // ---------------------------------------------------------------- EVM -> Celestia
    print::step(format!(
        "EVM -> Celestia: {amount} of each asset back from each chain to {smoke}"
    ));
    let mut expected = std::collections::BTreeMap::new();
    for (symbol, _, denom) in &tokens {
        expected.insert(*symbol, (denom.clone(), appd.balance(&smoke, denom)?));
    }
    let mut back = Vec::new();
    for c in &conns {
        for (symbol, _, _) in &tokens {
            let r = router(&d, c.chain, symbol)?;
            let ir = IRouter::new(r, &c.p);
            let fee = ir
                .quoteGasPayment(cel::DOMAIN)
                .call()
                .await
                .with_context(|| format!("quoting delivery from {}", c.chain.label))?;
            let data = IRouter::transferRemoteCall {
                _destination: cel::DOMAIN,
                _recipient: smoke32.parse().context("smoke key as bytes32")?,
                _amountOrId: U256::from(amount),
            }
            .abi_encode();
            c.send_value(
                &format!("sending {symbol} from {} to Celestia", c.chain.label),
                r,
                data.into(),
                fee,
            )
            .await?;
            print::info(format!("sent {symbol} from {}", c.chain.label));
            back.push(Leg {
                label: format!("{}-to-celestia {symbol}", c.chain.name),
                started: Instant::now(),
                done: None,
            });
            if let Some(e) = expected.get_mut(symbol) {
                e.1 += amount;
            }
        }
    }
    // The smoke key's balance rises only as transfers land, so the legs are matched by count:
    // each route's arrival is the balance passing its sum.
    let started = Instant::now();
    loop {
        let mut all = true;
        for (symbol, (denom, want)) in &expected {
            let have = appd.balance(&smoke, denom)?;
            let missing = want.saturating_sub(have) / amount;
            if missing > 0 {
                all = false;
                if started.elapsed() > deadline {
                    let routes: Vec<&str> = back
                        .iter()
                        .filter(|l| l.label.ends_with(symbol))
                        .map(|l| l.label.as_str())
                        .collect();
                    bail!(
                        "{missing} {symbol} transfer(s) to Celestia did not arrive within {timeout_mins} minutes (of {}); \
                         the relayer's /api/v1/messages shows which",
                        routes.join(", ")
                    );
                }
            }
        }
        if all {
            break;
        }
        tokio::time::sleep(Duration::from_secs(15)).await;
    }
    print::ok(format!(
        "every EVM -> Celestia transfer arrived within {}s",
        started.elapsed().as_secs()
    ));
    for l in &legs {
        print::kv(&l.label, format!("{}s", l.done.map(|d| d.as_secs()).unwrap_or(0)));
    }
    print::ok(format!(
        "{} transfers on {} routes, both directions",
        legs.len() + back.len(),
        chains.len() * 2
    ));
    Ok(())
}
