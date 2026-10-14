//! `teeism warp evm`: the EVM side of every warp route, and enrollment in both directions.
//!
//! Both assets start on Celestia and are synthetic on every EVM chain: TIA (collateral on
//! Celestia) and teeUSD (a fixed supply minted on Celestia).

use alloy::primitives::{Address, FixedBytes, U256};
use alloy::sol;
use alloy::sol_types::{SolCall, SolValue};
use anyhow::{bail, Context, Result};
use std::str::FromStr;

use crate::shared::celestia_cli::{self, Appd};
use crate::shared::chains::{self, celestia as cel};
use crate::shared::evm::{Artifact, Chain};
use crate::shared::print;
use crate::shared::settings::Ctx;
use crate::steps::isms;

sol! {
    #[sol(rpc)]
    interface IRouter {
        function initialize(uint256 _totalSupply, string memory _name, string memory _symbol, address _hook, address _interchainSecurityModule, address _owner) external;
        function enrollRemoteRouter(uint32 _domain, bytes32 _router) external;
        function routers(uint32 _domain) external view returns (bytes32);
        function interchainSecurityModule() external view returns (address);
        function setInterchainSecurityModule(address _module) external;
        function owner() external view returns (address);
        function symbol() external view returns (string memory);
        function balanceOf(address account) external view returns (uint256);
        function quoteGasPayment(uint32 _destinationDomain) external view returns (uint256);
        function transferRemote(uint32 _destination, bytes32 _recipient, uint256 _amountOrId) external payable returns (bytes32);
    }
}

/// Destination gas a warp delivery is enrolled with, which the paymaster quotes against.
const WARP_DEST_GAS: &str = "50000";

pub struct Asset {
    pub symbol: &'static str,
    pub name: &'static str,
    pub decimals: u8,
}

pub const ASSETS: [Asset; 2] = [
    Asset {
        symbol: "TIA",
        name: "Celestia TIA",
        decimals: 6,
    },
    Asset {
        symbol: "teeUSD",
        name: "tee USD",
        decimals: 6,
    },
];

fn celestia_token(d: &crate::shared::record::Deployment, symbol: &str) -> Option<String> {
    match symbol {
        "TIA" => d.celestia.tia_token.clone(),
        _ => d.celestia.teeusd_token.clone(),
    }
}

fn router_slot<'a>(e: &'a mut crate::shared::record::Evm, symbol: &str) -> &'a mut Option<Address> {
    match symbol {
        "TIA" => &mut e.tia_router,
        _ => &mut e.teeusd_router,
    }
}

fn bytes32(s: &str) -> Result<FixedBytes<32>> {
    FixedBytes::from_str(s).with_context(|| format!("{s} is not a 32-byte id"))
}

/// `teeism warp evm`
pub async fn evm(ctx: &Ctx, selection: &[String]) -> Result<()> {
    for chain in chains::evm_selection(selection)? {
        print::step(format!("warp routers on {}", chain.label));
        let c = Chain::connect(ctx, chain).await?;
        let mut d = ctx.deployment()?;
        let ism = d
            .evm(chain.name)
            .and_then(|e| e.ism)
            .with_context(|| format!("no ISM on {}; run `teeism isms evm`", chain.label))?;
        let mailbox = isms::mailbox_of(&d, chain)?;

        for asset in &ASSETS {
            let token = celestia_token(&d, asset.symbol).with_context(|| {
                format!(
                    "no {} token on Celestia; run `teeism celestia warp`",
                    asset.symbol
                )
            })?;
            let recorded = *router_slot(d.evm_mut(chain.name), asset.symbol);
            let router = match recorded {
                Some(r) if c.has_code(r).await? => {
                    reuse(&c, r, ism, asset.symbol).await?;
                    r
                }
                Some(r) => {
                    // Only a recorded address with no code at all is replaced: anything else
                    // that goes wrong stops the run, because deploying over a live router
                    // orphans every token it minted.
                    print::warn(format!(
                        "the recorded {} router {r} has no code on {}; deploying one",
                        asset.symbol, chain.label
                    ));
                    deploy_router(ctx, &c, mailbox, ism, &token, asset).await?
                }
                None => deploy_router(ctx, &c, mailbox, ism, &token, asset).await?,
            };
            *router_slot(d.evm_mut(chain.name), asset.symbol) = Some(router);
            ctx.save(&d)?;

            // EVM -> Celestia: the router knows the Celestia token as its remote.
            let r = IRouter::new(router, &c.p);
            let got = r
                .routers(cel::DOMAIN)
                .call()
                .await
                .with_context(|| format!("reading {router}'s remote routers"))?;
            if got != bytes32(&token)? {
                let data = IRouter::enrollRemoteRouterCall {
                    _domain: cel::DOMAIN,
                    _router: bytes32(&token)?,
                }
                .abi_encode();
                c.send(
                    &format!("enrolling the Celestia {} token", asset.symbol),
                    router,
                    data.into(),
                )
                .await?;
            }
            print::info(format!("{} {router} (ism {ism})", asset.symbol));
        }
    }

    // Celestia -> EVM: read what the chain holds rather than a saved marker. The chain refuses
    // to enroll a domain twice, so a changed router is unrolled first.
    print::step("enrolling the EVM routers on Celestia");
    celestia_cli::wait_for_chain(ctx).await?;
    let appd = Appd::new(ctx)?;
    let d = ctx.deployment()?;
    for asset in &ASSETS {
        let token = celestia_token(&d, asset.symbol).context("no Celestia token")?;
        let enrolled = celestia_cli::remote_routers(&appd, &token)?;
        for chain in chains::evm_selection(selection)? {
            let Some(router) = d.evm(chain.name).and_then(|e| {
                if asset.symbol == "TIA" {
                    e.tia_router
                } else {
                    e.teeusd_router
                }
            }) else {
                bail!("no {} router on {}", asset.symbol, chain.label);
            };
            let want = celestia_cli::pad32(&router);
            match enrolled.iter().find(|(dom, _)| *dom == chain.domain) {
                Some((_, got)) if *got == want => {
                    print::info(format!("{} on {}: already enrolled", asset.symbol, chain.label));
                    continue;
                }
                Some((_, got)) => {
                    print::info(format!("{} on {}: replacing {got}", asset.symbol, chain.label));
                    appd.tx(
                        &format!("unrolling the old {} router for {}", asset.symbol, chain.label),
                        "relayer",
                        &["warp", "unroll-remote-router", &token, &chain.domain.to_string()],
                    )?;
                }
                None => {}
            }
            // The 32-byte form: Hyperlane addresses are 32 bytes everywhere, and this CLI
            // rejects a bare 20-byte address rather than padding it.
            appd.tx(
                &format!("enrolling the {} router on {}", asset.symbol, chain.label),
                "relayer",
                &[
                    "warp",
                    "enroll-remote-router",
                    &token,
                    &chain.domain.to_string(),
                    &want,
                    WARP_DEST_GAS,
                ],
            )?;
            print::info(format!("{} on {}: enrolled {want}", asset.symbol, chain.label));
        }
    }
    Ok(())
}

/// A live router: point it at the current ISM if it has drifted. A router's ISM is the
/// owner's to change, so a rotation repoints rather than redeploys.
async fn reuse(c: &Chain, router: Address, ism: Address, symbol: &str) -> Result<()> {
    let r = IRouter::new(router, &c.p);
    let cur = r
        .interchainSecurityModule()
        .call()
        .await
        .with_context(|| format!("reading {router}'s ISM"))?;
    if cur == ism {
        return Ok(());
    }
    let owner = r
        .owner()
        .call()
        .await
        .with_context(|| format!("reading {router}'s owner"))?;
    if owner != c.from {
        bail!("the {symbol} router {router} is owned by {owner}, not {}; its owner has to point it at {ism}. Not deploying a replacement", c.from);
    }
    print::info(format!("repointing the {symbol} router {router}: {cur} -> {ism}"));
    let data = IRouter::setInterchainSecurityModuleCall { _module: ism }.abi_encode();
    c.send(&format!("repointing the {symbol} router"), router, data.into())
        .await?;
    let after = r.interchainSecurityModule().call().await?;
    if after != ism {
        bail!("{router} still reports ISM {after} after repointing to {ism}");
    }
    Ok(())
}

/// A synthetic HypERC20 for one asset, its ISM set at initialize. No hook: dispatch goes
/// through the mailbox's default hook, whose merkle tree is the one the enclave proves.
async fn deploy_router(
    ctx: &Ctx,
    c: &Chain,
    mailbox: Address,
    ism: Address,
    token: &str,
    asset: &Asset,
) -> Result<Address> {
    let art = Artifact::load(&ctx.contracts_dir(), "HypERC20", "HypERC20")?;
    // decimals, scale numerator, scale denominator, mailbox
    let args = (U256::from(asset.decimals), U256::from(1), U256::from(1), mailbox).abi_encode_params();
    let router = c
        .deploy(
            &format!("deploying the {} router", asset.symbol),
            art.initcode(&args).into(),
        )
        .await?;
    let data = IRouter::initializeCall {
        _totalSupply: U256::ZERO,
        _name: asset.name.into(),
        _symbol: asset.symbol.into(),
        _hook: Address::ZERO,
        _interchainSecurityModule: ism,
        _owner: c.from,
    }
    .abi_encode();
    c.send(
        &format!("initializing the {} router", asset.symbol),
        router,
        data.into(),
    )
    .await?;
    let data = IRouter::enrollRemoteRouterCall {
        _domain: cel::DOMAIN,
        _router: bytes32(token)?,
    }
    .abi_encode();
    c.send(
        &format!("enrolling the Celestia {} token", asset.symbol),
        router,
        data.into(),
    )
    .await?;
    Ok(router)
}
