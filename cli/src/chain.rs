//! The bridge's own Celestia chain: genesis and node (`celestia up`), Hyperlane core
//! (`celestia core`), and the Celestia side of the warp routes (`celestia warp`).

use anyhow::{bail, Context, Result};

use crate::build;
use crate::celestia::{self, Appd};
use crate::chains::celestia as cel;
use crate::ctx::Ctx;
use crate::out;
use crate::run::Cmd;
use crate::tomlx::{set_toml, tv};

/// Genesis balances, in utia.
const VALIDATOR_COINS: &str = "1000000000000000utia"; // 1B TIA
const VALIDATOR_STAKE: &str = "5000000000utia";
/// Pays for every Celestia transaction the relayer sends.
const RELAYER_COINS: &str = "1000000000000utia"; // 1M TIA
/// The account the faucet pays from, and the one a wallet importing the mnemonic lands on.
const USER_COINS: &str = "1000000000000000utia"; // 1B TIA

/// Keys derived from CELESTIA_MNEMONIC, by account index. 0 is what Keplr selects when the
/// phrase is imported. 3 is the paymaster's `bridge` key, added by `teeism paymaster`.
const KEYS: [(&str, u32); 4] = [("user", 0), ("relayer", 1), ("validator", 2), ("smoke", 5)];
pub const BRIDGE_KEY: (&str, u32) = ("bridge", 3);

pub const CONTAINER: &str = "teeism-celestia";

/// teeUSD: a dollar token we issue, with a fixed supply minted once.
const TEEUSD_SUPPLY: u128 = 1_000_000_000_000_000; // 1B at 6 decimals
/// A made-up origin that mints the supply in one message and is then removed. "teeU".
const MINT_DOMAIN: u32 = 1952802133;
const MINT_ROUTER: &str = "0x0000000000000000000000000000000000000000000000000000000074656555";

/// The mnemonic every genesis key derives from, generated once and kept in devnet/.env, so a
/// wallet that imported it still holds funds after the chain is rebuilt.
fn mnemonic(ctx: &mut Ctx) -> Result<String> {
    if let Some(m) = ctx.var("CELESTIA_MNEMONIC") {
        return Ok(m);
    }
    let m = Cmd::new("generating a genesis mnemonic", ctx.appd()).args(["keys", "mnemonic"]).output()?;
    let m = m.lines().map(str::trim).find(|l| l.split_whitespace().count() >= 12).context("celestia-appd printed no mnemonic")?.to_string();
    ctx.set_env("CELESTIA_MNEMONIC", &m)?;
    out::info(format!("generated a mnemonic and wrote it to {} as CELESTIA_MNEMONIC", ctx.env_file().display()));
    Ok(m)
}

fn genesis(ctx: &mut Ctx, appd: &Appd) -> Result<()> {
    let home = ctx.celestia_home();
    let h = home.to_string_lossy().into_owned();
    let mnemonic = mnemonic(ctx)?;
    out::step(format!("creating the genesis of {} in {h}", cel::CHAIN_ID));
    Cmd::new("initialising the chain's home", appd.bin())
        .args(["init", cel::CHAIN_ID, "--chain-id", cel::CHAIN_ID, "--home", &h])
        .output()?;
    for (key, account) in KEYS {
        appd.recover_key(key, account, &mnemonic)?;
    }
    for (key, coins) in [("validator", VALIDATOR_COINS), ("relayer", RELAYER_COINS), ("user", USER_COINS)] {
        let addr = appd.address(key)?;
        Cmd::new(format!("funding {key} at genesis"), appd.bin())
            .args(["genesis", "add-genesis-account", &addr, coins, "--home", &h])
            .output()?;
    }
    // The gentx is metered like any other transaction during InitGenesis, so a fee-less one
    // aborts the chain before its first block.
    Cmd::new("creating the validator's gentx", appd.bin())
        .args([
            "genesis", "gentx", "validator", VALIDATOR_STAKE, "--keyring-backend", "test",
            "--chain-id", cel::CHAIN_ID, "--fees", "2000utia", "--home", &h,
        ])
        .output()?;
    Cmd::new("collecting gentxs", appd.bin()).args(["genesis", "collect-gentxs", "--home", &h]).output()?;

    let cfg = home.join("config/config.toml");
    let app = home.join("config/app.toml");
    // Answer outside the container.
    set_toml(&cfg, "rpc", "laddr", tv("tcp://0.0.0.0:26657"))?;
    // The relayer finds dispatches through indexed tx search; without the indexer it sees none.
    set_toml(&cfg, "tx_index", "indexer", tv("kv"))?;
    let mut origins = toml_edit::Array::new();
    origins.push("*");
    set_toml(&cfg, "rpc", "cors_allowed_origins", tv(origins))?;
    match set_toml(&cfg, "consensus", "timeout_commit", tv("1s")) {
        Ok(()) => {}
        Err(e) => out::warn(format!("block time left at the default: {e:#}")),
    }
    set_toml(&app, "api", "enable", tv(true))?;
    set_toml(&app, "api", "swagger", tv(true))?;
    set_toml(&app, "api", "address", tv("tcp://0.0.0.0:1317"))?;
    set_toml(&app, "api", "enabled-unsafe-cors", tv(true))?;
    set_toml(&app, "grpc", "address", tv("0.0.0.0:9090"))?;
    set_toml(&app, "", "minimum-gas-prices", tv("0.002utia"))?;
    // Never prune. An ISM advances only when a batch is delivered, so a quiet route's trusted
    // height can fall far behind the head, and the light client proves forward from the
    // commit at that height. Once it is pruned the route cannot advance again.
    set_toml(&app, "", "pruning", tv("nothing"))?;
    set_toml(&app, "", "min-retain-blocks", tv(0i64))?;
    Ok(())
}

/// The node's image, tagged by the celestia-app commit it is built from, so a new commit is a
/// new image rather than a stale one reused.
fn image(ctx: &Ctx) -> Result<String> {
    let app = build::check_celestia_app(ctx)?;
    let commit = Cmd::new("reading celestia-app's commit", "git").arg("-C").arg(&app).args(["rev-parse", "--short=12", "HEAD"]).output()?;
    let tag = format!("celestia-app-teeism:{}", commit.trim());
    let (status, _, _) = Cmd::new("checking for the node image", "docker").args(["image", "inspect", &tag]).capture()?;
    if !status.success() {
        out::step(format!("building {tag} from {} (a few minutes)", app.display()));
        Cmd::new(format!("building {tag}"), "docker")
            .args(["build", "-t", &tag, "-f"])
            .arg(app.join("docker/standalone.Dockerfile"))
            .arg(&app)
            .run()?;
    }
    Ok(tag)
}

fn uid() -> Result<String> {
    let u = Cmd::new("reading this user's uid", "id").arg("-u").output()?;
    let g = Cmd::new("reading this user's gid", "id").arg("-g").output()?;
    Ok(format!("{}:{}", u.trim(), g.trim()))
}

pub fn compose(ctx: &Ctx, image: &str) -> Result<Cmd> {
    Ok(Cmd::new("running the chain's container", "docker")
        .args(["compose", "-f"])
        .arg(ctx.devnet().join("celestia/docker-compose.yml"))
        .env("CELESTIA_IMAGE", image)
        .env("CELESTIA_UID", uid()?)
        .env("CELESTIA_HOME", ctx.celestia_home().to_string_lossy()))
}

/// `teeism celestia up`
pub async fn up(ctx: &mut Ctx, new_image: bool) -> Result<()> {
    let appd = Appd::new(ctx)?;
    if !ctx.celestia_home().join("config/genesis.json").is_file() {
        genesis(ctx, &appd)?;
    } else {
        out::info(format!("genesis already in {}", ctx.celestia_home().display()));
    }
    let image = image(ctx)?;
    // A running node on another image is a binary change on a live chain: a state machine
    // change there without a coordinated upgrade can halt it. Only on request.
    let (s, running, _) = Cmd::new("inspecting the running node", "docker")
        .args(["inspect", "-f", "{{.Config.Image}}", CONTAINER])
        .capture()?;
    let running = running.trim();
    if s.success() && running != image && !new_image {
        bail!(
            "{CONTAINER} runs {running}, but the celestia-app checkout builds {image}. Restarting a live chain \
             on a different binary can halt it if the state machine changed. Pass --new-image to do it anyway"
        );
    }
    out::step(format!("starting {CONTAINER} ({image})"));
    compose(ctx, &image)?.args(["up", "-d"]).run()?;
    let h = celestia::wait_for_chain(ctx).await?;
    out::info(format!("{} is at height {h}", cel::CHAIN_ID));

    let mut d = ctx.deployment()?;
    d.celestia.user = Some(appd.address("user")?);
    d.celestia.relayer = Some(appd.address("relayer")?);
    d.celestia.smoke = Some(appd.address("smoke")?);
    ctx.save(&d)?;
    out::kv("user", d.celestia.user.as_deref().unwrap_or(""));
    out::kv("relayer", d.celestia.relayer.as_deref().unwrap_or(""));
    Ok(())
}

/// `teeism celestia core`: mailbox, merkle tree hook, and the noop ISM and hook it starts
/// with. All permissionless: the same sequence anyone would run against a real chain.
pub async fn core(ctx: &Ctx) -> Result<()> {
    celestia::wait_for_chain(ctx).await?;
    let appd = Appd::new(ctx)?;
    let mut d = ctx.deployment()?;
    let c = &mut d.celestia;

    // A noop ISM gives the mailbox a default before the TEE ISMs exist; those pin a light
    // client checkpoint, which needs the chain running.
    if c.noop_ism.is_none() {
        let tx = appd.tx("creating a noop ISM", "relayer", &["hyperlane", "ism", "create-noop"])?;
        c.noop_ism = Some(tx.attr("hyperlane.core.interchain_security.v1.EventCreateNoopIsm", "ism_id")?);
    }
    out::kv("noop ism", c.noop_ism.as_deref().unwrap_or(""));
    if c.mailbox.is_none() {
        let tx = appd.tx(
            "creating the mailbox",
            "relayer",
            &["hyperlane", "mailbox", "create", c.noop_ism.as_deref().unwrap_or(""), &cel::DOMAIN.to_string()],
        )?;
        c.mailbox = Some(tx.attr("hyperlane.core.v1.EventCreateMailbox", "mailbox_id")?);
    }
    let mailbox = c.mailbox.clone().unwrap_or_default();
    out::kv("mailbox", &mailbox);
    if c.merkle_hook.is_none() {
        let tx = appd.tx("creating the merkle tree hook", "relayer", &["hyperlane", "hooks", "merkle", "create", &mailbox])?;
        c.merkle_hook = Some(tx.attr("hyperlane.core.post_dispatch.v1.EventCreateMerkleTreeHook", "merkle_tree_hook_id")?);
    }
    let merkle = c.merkle_hook.clone().unwrap_or_default();
    out::kv("merkle tree hook", &merkle);
    if c.noop_hook.is_none() {
        let tx = appd.tx("creating a noop hook", "relayer", &["hyperlane", "hooks", "noop", "create"])?;
        c.noop_hook = Some(tx.attr("hyperlane.core.post_dispatch.v1.EventCreateNoopHook", "noop_hook_id")?);
    }
    let noop_hook = c.noop_hook.clone().unwrap_or_default();
    out::kv("noop hook", &noop_hook);
    ctx.save(&d)?;

    // The merkle tree hook is the default because its tree is what the enclave proves. Warp
    // refuses to dispatch without a required hook; noop keeps dispatch free until
    // `teeism paymaster` puts the gas paymaster there.
    let m = appd.query("reading the mailbox", &["hyperlane", "mailbox", &mailbox])?;
    let default_hook = m["mailbox"]["default_hook"].as_str().unwrap_or("");
    let required_hook = m["mailbox"]["required_hook"].as_str().unwrap_or("");
    if default_hook.is_empty() || required_hook.is_empty() {
        appd.tx(
            "pointing the mailbox at the merkle tree hook",
            "relayer",
            &["hyperlane", "mailbox", "set", &mailbox, "--default-hook", &merkle, "--required-hook", &noop_hook],
        )?;
    }
    out::ok("hyperlane core is up");
    Ok(())
}

/// `teeism celestia warp`: the Celestia side of both assets.
///
///   TIA     native: collateral here, synthetic on every EVM chain.
///   teeUSD  ours: synthetic here too, with a fixed supply minted once below.
///
/// Remote routers are enrolled by `teeism warp evm`, which owns both directions of a route.
pub async fn warp(ctx: &Ctx) -> Result<()> {
    celestia::wait_for_chain(ctx).await?;
    let appd = Appd::new(ctx)?;
    let mut d = ctx.deployment()?;
    let mailbox = d.celestia.mailbox.clone().context("no mailbox yet; run `teeism celestia core`")?;

    if d.celestia.tia_token.is_none() {
        let tx = appd.tx(
            "creating the TIA collateral token",
            "relayer",
            &["warp", "create-collateral-token", &mailbox, cel::DENOM],
        )?;
        d.celestia.tia_token = Some(tx.attr("hyperlane.warp.v1.EventCreateCollateralToken", "token_id")?);
        ctx.save(&d)?;
    }
    out::kv("TIA token", d.celestia.tia_token.as_deref().unwrap_or(""));

    if d.celestia.teeusd_token.is_none() {
        let tx = appd.tx("creating the teeUSD synthetic token", "relayer", &["warp", "create-synthetic-token", &mailbox])?;
        d.celestia.teeusd_token = Some(tx.attr("hyperlane.warp.v1.EventCreateSyntheticToken", "token_id")?);
        ctx.save(&d)?;
    }
    let token = d.celestia.teeusd_token.clone().unwrap_or_default();
    let denom = d.teeusd_denom().unwrap_or_default();
    out::kv("teeUSD token", &token);

    // There is no mint module, so the supply arrives the way any synthetic's does: as a
    // Hyperlane message. The token briefly trusts the noop ISM and a made-up origin, takes one
    // message minting the supply to `user`, then drops that origin. With it gone nothing can
    // mint again. Each step checks the chain first, so an interrupted run is finished by
    // running it again.
    let mint_enrolled = || -> Result<bool> {
        Ok(celestia::remote_routers(&appd, &token)?.iter().any(|(dom, _)| *dom == MINT_DOMAIN))
    };
    let supply = appd.total_supply(&denom)?;
    if supply == 0 {
        let holder = d.celestia.user.clone().context("no user key recorded; run `teeism celestia up`")?;
        out::step(format!("minting {TEEUSD_SUPPLY} {denom} to {holder}"));
        let noop = d.celestia.noop_ism.clone().context("no noop ISM; run `teeism celestia core`")?;
        appd.tx("letting teeUSD trust the noop ISM for the mint", "relayer", &["warp", "set-token", &token, "--ism-id", &noop])?;
        if !mint_enrolled()? {
            appd.tx(
                "enrolling the mint origin",
                "relayer",
                &["warp", "enroll-remote-router", &token, &MINT_DOMAIN.to_string(), MINT_ROUTER, "0"],
            )?;
        }
        let msg = mint_message(&token, &celestia::bech32_to_hex32(&holder)?, TEEUSD_SUPPLY);
        appd.tx("delivering the mint message", "relayer", &["hyperlane", "mailbox", "process", &mailbox, "0x", &msg])?;
        let after = appd.total_supply(&denom)?;
        if after != TEEUSD_SUPPLY {
            bail!("teeUSD supply is {after} after the mint, expected {TEEUSD_SUPPLY}");
        }
    }
    if mint_enrolled()? {
        appd.tx("dropping the mint origin, fixing the supply", "relayer", &["warp", "unroll-remote-router", &token, &MINT_DOMAIN.to_string()])?;
    }
    out::kv("teeUSD supply", appd.total_supply(&denom)?);
    out::ok("celestia warp side ready; the tokens are pointed at the routing ISM by `teeism isms celestia`");
    Ok(())
}

/// A Hyperlane v3 message from the mint origin to the teeUSD token, whose body is a warp
/// transfer of `amount` to `recipient`.
fn mint_message(token: &str, recipient32: &str, amount: u128) -> String {
    let strip = |s: &str| s.trim_start_matches("0x").to_lowercase();
    format!(
        "0x03{:08x}{:08x}{}{:08x}{}{}{:064x}",
        0u32,
        MINT_DOMAIN,
        strip(MINT_ROUTER),
        cel::DOMAIN,
        strip(token),
        strip(recipient32),
        amount
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn mint_message_layout() {
        let m = super::mint_message(
            "0x726f757465725f61707000000000000000000000000000020000000000000002",
            "0x000000000000000000000000318d22faa1e0f29eac7ef644a8fac676f6688d1e",
            1,
        );
        // version(1) nonce(4) origin(4) sender(32) destination(4) recipient(32) body(64)
        assert_eq!(m.len(), 2 + 2 * (1 + 4 + 4 + 32 + 4 + 32 + 64));
        assert!(m.starts_with("0x0300000000746565550000"));
    }
}
