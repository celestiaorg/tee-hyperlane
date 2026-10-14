//! `teeism celestia up`: create the genesis (first run only) on the host, then run the node.
//! Also the keys every step signs with, and editing a node's TOML config.

use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::shared::chains::celestia as cel;
use crate::shared::print;
use crate::shared::settings::Ctx;
use crate::shared::shell::Cmd;
use crate::steps::build;
use crate::steps::celestia::appd::{self, Appd};

pub const CONTAINER: &str = "teeism-celestia";

/// Keys derived from CELESTIA_MNEMONIC, by account index. 0 is what Keplr selects when the
/// phrase is imported. 3 is the paymaster's `bridge` key, added by `teeism paymaster`.
const KEYS: [(&str, u32); 4] = [("user", 0), ("relayer", 1), ("validator", 2), ("smoke", 5)];
pub const BRIDGE_KEY: (&str, u32) = ("bridge", 3);

/// Genesis balances, in utia.
const VALIDATOR_COINS: &str = "1000000000000000utia"; // 1B TIA
const VALIDATOR_STAKE: &str = "5000000000utia";
/// Pays for every Celestia transaction the relayer sends.
const RELAYER_COINS: &str = "1000000000000utia"; // 1M TIA
/// The account the faucet pays from, and the one a wallet importing the mnemonic lands on.
const USER_COINS: &str = "1000000000000000utia"; // 1B TIA

/// The mnemonic every genesis key derives from, generated once and kept in devnet/.env, so a
/// wallet that imported it still holds funds after the chain is rebuilt.
fn mnemonic(ctx: &mut Ctx) -> Result<String> {
    if let Some(m) = ctx.var("CELESTIA_MNEMONIC") {
        return Ok(m);
    }
    let m = Cmd::new("generating a genesis mnemonic", ctx.appd())
        .args(["keys", "mnemonic"])
        .output()?;
    let m = m
        .lines()
        .map(str::trim)
        .find(|l| l.split_whitespace().count() >= 12)
        .context("celestia-appd printed no mnemonic")?
        .to_string();
    ctx.set_env("CELESTIA_MNEMONIC", &m)?;
    print::info(format!(
        "generated a mnemonic and wrote it to {} as CELESTIA_MNEMONIC",
        ctx.env_file().display()
    ));
    Ok(m)
}

fn genesis(ctx: &mut Ctx, appd: &Appd) -> Result<()> {
    let home = ctx.celestia_home();
    let h = home.to_string_lossy().into_owned();
    let mnemonic = mnemonic(ctx)?;
    print::step(format!("creating the genesis of {} in {h}", cel::CHAIN_ID));
    Cmd::new("initialising the chain's home", appd.bin())
        .args(["init", cel::CHAIN_ID, "--chain-id", cel::CHAIN_ID, "--home", &h])
        .output()?;
    for (key, account) in KEYS {
        appd.recover_key(key, account, &mnemonic)?;
    }
    for (key, coins) in [
        ("validator", VALIDATOR_COINS),
        ("relayer", RELAYER_COINS),
        ("user", USER_COINS),
    ] {
        let addr = appd.address(key)?;
        Cmd::new(format!("funding {key} at genesis"), appd.bin())
            .args(["genesis", "add-genesis-account", &addr, coins, "--home", &h])
            .output()?;
    }
    // The gentx is metered like any other transaction during InitGenesis, so a fee-less one
    // aborts the chain before its first block.
    Cmd::new("creating the validator's gentx", appd.bin())
        .args([
            "genesis",
            "gentx",
            "validator",
            VALIDATOR_STAKE,
            "--keyring-backend",
            "test",
            "--chain-id",
            cel::CHAIN_ID,
            "--fees",
            "2000utia",
            "--home",
            &h,
        ])
        .output()?;
    Cmd::new("collecting gentxs", appd.bin())
        .args(["genesis", "collect-gentxs", "--home", &h])
        .output()?;

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
        Err(e) => print::warn(format!("block time left at the default: {e:#}")),
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
    let commit = Cmd::new("reading celestia-app's commit", "git")
        .arg("-C")
        .arg(&app)
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()?;
    let tag = format!("celestia-app-teeism:{}", commit.trim());
    let (status, _, _) = Cmd::new("checking for the node image", "docker")
        .args(["image", "inspect", &tag])
        .capture()?;
    if !status.success() {
        print::step(format!("building {tag} from {} (a few minutes)", app.display()));
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
pub async fn run(ctx: &mut Ctx, new_image: bool) -> Result<()> {
    let appd = Appd::new(ctx)?;
    if !ctx.celestia_home().join("config/genesis.json").is_file() {
        genesis(ctx, &appd)?;
    } else {
        print::info(format!("genesis already in {}", ctx.celestia_home().display()));
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
    print::step(format!("starting {CONTAINER} ({image})"));
    compose(ctx, &image)?.args(["up", "-d"]).run()?;
    let h = appd::wait_for_chain(ctx).await?;
    print::info(format!("{} is at height {h}", cel::CHAIN_ID));

    let mut d = ctx.deployment()?;
    d.celestia.user = Some(appd.address("user")?);
    d.celestia.relayer = Some(appd.address("relayer")?);
    d.celestia.smoke = Some(appd.address("smoke")?);
    ctx.save(&d)?;
    print::kv("user", d.celestia.user.as_deref().unwrap_or(""));
    print::kv("relayer", d.celestia.relayer.as_deref().unwrap_or(""));
    Ok(())
}

// ---------------------------------------------------------------- editing a node's TOML config

/// Set `key` under the dotted `table` path ("" for the top level) in a TOML file, failing if
/// the key is not already there: a renamed setting must stop the run, not be silently added
/// under a name the node ignores.
pub fn set_toml(path: &Path, table: &str, key: &str, value: toml_edit::Item) -> Result<()> {
    let raw = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut doc: toml_edit::DocumentMut = raw
        .parse()
        .with_context(|| format!("{} is not TOML", path.display()))?;
    let mut t = doc.as_table_mut();
    if !table.is_empty() {
        for part in table.split('.') {
            t = t
                .get_mut(part)
                .and_then(|t| t.as_table_mut())
                .with_context(|| format!("{} has no [{table}] table", path.display()))?;
        }
    }
    if !t.contains_key(key) {
        bail!(
            "{} has no `{key}` under [{table}]; this version names it differently",
            path.display()
        );
    }
    t[key] = value;
    std::fs::write(path, doc.to_string()).with_context(|| format!("writing {}", path.display()))
}

pub fn tv(v: impl Into<toml_edit::Value>) -> toml_edit::Item {
    toml_edit::Item::Value(v.into())
}
