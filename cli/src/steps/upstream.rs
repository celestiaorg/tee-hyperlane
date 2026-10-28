//! `teeism upstream`: watch the chains and projects this bridge depends on for changes that
//! could break it, before they activate.
//!
//! Two kinds of source, both listed in deploy/upstream.toml:
//! - Schedules and keys read from the chains: Sepolia's beacon spec (consensus forks), Base's
//!   upgrade table, ArbOS versions and scheduled upgrades, and the Base and Arbitrum sequencer
//!   keys against the ones the enclave pins. These are exact, so a rule decides the verdict.
//! - GitHub releases of the clients, specs and contracts we depend on. OpenAI judges each one
//!   against the manifest in deploy/upstream.toml and says whether it breaks us.
//!
//! Every item is kept in devnet/.state/upstream.json with its verdict, so nothing is judged or
//! posted twice. The dashboard on :3001 shows that file; Slack gets the breaking and maybe ones.

use std::collections::BTreeSet;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::shared::print;
use crate::shared::settings::Ctx;
use crate::shared::{chains, print::Incomplete};

const ARBSYS: &str = "0x0000000000000000000000000000000000000064";
const ARB_OWNER_PUBLIC: &str = "0x000000000000000000000000000000000000006b";
/// ArbSys.arbOSVersion() reports the version plus this.
const ARBOS_OFFSET: u64 = 55;
const DEFAULT_MODEL: &str = "gpt-5";
/// Release notes past this are cut before judging; the start says what matters.
const MAX_BODY: usize = 20_000;

#[derive(Deserialize)]
struct Watch {
    since_days: u64,
    ethereum_forks: Vec<String>,
    base_config_url: String,
    base_upgrades: Vec<String>,
    arbos_versions: Vec<u64>,
    base_system_config: String,
    arbitrum_sequencer_inbox: String,
    releases: Vec<Release>,
    manifest: Manifest,
}

#[derive(Deserialize)]
struct Release {
    repo: String,
    chain: String,
    #[serde(default)]
    tag_prefix: Option<String>,
}

#[derive(Deserialize)]
struct Manifest {
    text: String,
}

#[derive(Serialize, Deserialize, Default)]
pub struct State {
    pub checked_at: Option<u64>,
    /// Sources the last check could not read, so a gap is visible rather than silent.
    #[serde(default)]
    pub errors: Vec<String>,
    #[serde(default)]
    pub items: Vec<Item>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Item {
    pub id: String,
    pub chain: String,
    /// `schedule`, `key` or `release`.
    pub source: String,
    pub title: String,
    pub url: Option<String>,
    pub published_at: Option<u64>,
    pub found_at: u64,
    /// `breaking`, `maybe`, `irrelevant`, or `unjudged` until the judge has seen it.
    pub verdict: String,
    /// `rule` for schedules and keys, `filter` for releases no keyword matched, else the model.
    pub judged_by: Option<String>,
    pub component: Option<String>,
    pub summary: Option<String>,
    pub reasoning: Option<String>,
    pub quote: Option<String>,
    pub affects: Option<String>,
    pub action: Option<String>,
    pub activation: Option<String>,
    pub activates_at: Option<u64>,
    /// Release notes, kept only until judged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub alerted_at: Option<u64>,
    pub acked_at: Option<u64>,
    pub note: Option<String>,
}

impl Item {
    fn open(&self) -> bool {
        self.acked_at.is_none() && matches!(self.verdict.as_str(), "breaking" | "maybe")
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn state_path(ctx: &Ctx) -> std::path::PathBuf {
    ctx.state_dir.join("upstream.json")
}

fn load_state(ctx: &Ctx) -> Result<State> {
    let path = state_path(ctx);
    match std::fs::read(&path) {
        Ok(raw) => serde_json::from_slice(&raw).with_context(|| format!("reading {}", path.display())),
        Err(_) => Ok(State::default()),
    }
}

fn save_state(ctx: &Ctx, state: &State) -> Result<()> {
    let path = state_path(ctx);
    std::fs::create_dir_all(&ctx.state_dir)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(state)?)
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))
}

fn load_watch(ctx: &Ctx) -> Result<Watch> {
    let path = ctx.repo.join("deploy/upstream.toml");
    let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

fn http() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent("teeism-upstream")
        .timeout(Duration::from_secs(60))
        .build()?)
}

// ---------------------------------------------------------------- teeism upstream check

pub async fn check(ctx: &Ctx, judge_releases: bool) -> Result<()> {
    let watch = load_watch(ctx)?;
    let mut state = load_state(ctx)?;
    let mut errors = Vec::new();
    let mut found = Vec::new();

    print::step("reading upgrade schedules and sequencer keys from the chains");
    let reads = [
        ("ethereum forks", ethereum_forks(ctx, &watch).await),
        ("base upgrades", base_upgrades(&watch).await),
        ("arbos", arbos(ctx, &watch).await),
        ("sequencer keys", sequencer_keys(ctx, &watch).await),
    ];
    for (what, read) in reads {
        match read {
            Ok(items) => found.extend(items),
            Err(e) => errors.push(format!("{what}: {e:#}")),
        }
    }

    print::step("reading releases");
    for r in &watch.releases {
        match releases(ctx, r, watch.since_days).await {
            Ok(items) => found.extend(items),
            Err(e) => errors.push(format!("{}: {e:#}", r.repo)),
        }
    }

    let known: BTreeSet<String> = state.items.iter().map(|i| i.id.clone()).collect();
    for item in found {
        if !known.contains(&item.id) {
            print::info(format!("new: {} ({})", item.title, item.verdict));
            state.items.push(item);
        }
    }

    // Releases: a keyword pass first, then the judge for whatever it lets through.
    let filter = relevance_filter()?;
    let pending: Vec<usize> = (0..state.items.len())
        .filter(|&i| state.items[i].verdict == "unjudged")
        .collect();
    let mut judge = None;
    for i in pending {
        let item = &mut state.items[i];
        let text = format!("{}\n{}", item.title, item.body.as_deref().unwrap_or(""));
        if !filter.is_match(&text) {
            item.verdict = "irrelevant".into();
            item.judged_by = Some("filter".into());
            item.summary = Some("No keyword that could concern this stack appears in it.".into());
            item.body = None;
            continue;
        }
        if !judge_releases {
            continue;
        }
        if judge.is_none() {
            judge = Some(Judge::new(ctx)?);
        }
        let j = judge.as_ref().unwrap();
        print::info(format!("judging {}", item.title));
        match j.judge(&watch.manifest.text, item).await {
            Ok(v) => {
                v.apply(item, &j.model);
                print::kv(&item.verdict, item.summary.as_deref().unwrap_or(""));
            }
            Err(e) => errors.push(format!("judging {}: {e:#}", item.title)),
        }
    }

    alert(ctx, &mut state, &mut errors).await;

    state.checked_at = Some(now());
    state.errors = errors.clone();
    save_state(ctx, &state)?;
    let unjudged = state.items.iter().filter(|i| i.verdict == "unjudged").count();
    print_open(&state, false);
    if unjudged > 0 && !judge_releases {
        print::info(format!(
            "{unjudged} release(s) left unjudged (--no-judge); the next check judges them"
        ));
    }
    if !errors.is_empty() {
        return Err(Incomplete(errors).into());
    }
    Ok(())
}

/// Words that appear in anything that could concern this stack. Broad on purpose: it only
/// keeps obviously unrelated releases (a metrics fix, a CLI flag) away from the judge.
fn relevance_filter() -> Result<Regex> {
    Ok(Regex::new(
        r"(?i)fork|upgrade|breaking|consensus|header|gossip|p2p|feed|signer|signature|sequencer|light.?client|sync.?committee|rlp|ssz|payload|deprecat|remov|rename|migrat|activat|mainnet|sepolia|testnet|mocha|namespace|blob|quote|tdx|attestation|dstack|kms|event.?log|tcb|pccs|collateral|mailbox|merkle|\bism\b|interchain|osaka|gloas|heze|amsterdam|arbos|denim|everest|schedul|protocol|spec",
    )?)
}

// ---------------------------------------------------------------- schedules and keys

/// Every consensus fork in the Sepolia beacon spec that `ethereum_forks` does not list.
async fn ethereum_forks(ctx: &Ctx, w: &Watch) -> Result<Vec<Item>> {
    let beacon = ctx
        .var("SEPOLIA_BEACON")
        .unwrap_or_else(|| "https://ethereum-sepolia-beacon-api.publicnode.com".into());
    let get = |path: &'static str| {
        let url = format!("{}{path}", beacon.trim_end_matches('/'));
        async move {
            let v: Value = http()?.get(&url).send().await?.error_for_status()?.json().await?;
            Ok::<_, anyhow::Error>(v["data"].clone())
        }
    };
    let spec = get("/eth/v1/config/spec").await.context("the beacon spec")?;
    let genesis = get("/eth/v1/beacon/genesis")
        .await
        .context("the beacon genesis")?;
    let num = |v: &Value| v.as_str().and_then(|s| s.parse::<u64>().ok());
    let genesis_time = num(&genesis["genesis_time"]).context("no genesis_time")?;
    let epoch_secs = num(&spec["SECONDS_PER_SLOT"]).context("no SECONDS_PER_SLOT")?
        * num(&spec["SLOTS_PER_EPOCH"]).context("no SLOTS_PER_EPOCH")?;
    let forks = spec.as_object().context("the beacon spec is not an object")?;
    let mut out = Vec::new();
    for (key, value) in forks {
        let Some(name) = key.strip_suffix("_FORK_EPOCH") else {
            continue;
        };
        let name = name.to_lowercase();
        if w.ethereum_forks.contains(&name) {
            continue;
        }
        let epoch = num(value).with_context(|| format!("{key} is not a number"))?;
        let upper = name.to_uppercase();
        if epoch == u64::MAX {
            out.push(rule_item(
                format!("eth-fork:{name}:unscheduled"),
                "ethereum",
                "schedule",
                format!("Ethereum Sepolia: the next consensus fork, {upper}, is in the beacon spec, not scheduled yet"),
                "maybe",
                "enclave image",
                None,
                format!("Read the {upper} consensus-specs changes: fork version, light-client data, execution header. Our light client needs every fork, as Gloas did."),
            ));
        } else {
            let at = genesis_time + epoch * epoch_secs;
            out.push(rule_item(
                format!("eth-fork:{name}:{epoch}"),
                "ethereum",
                "schedule",
                format!("Ethereum Sepolia: {upper} activates at epoch {epoch} ({})", date(at)),
                "breaking",
                "enclave image",
                Some(at),
                format!("Teach the enclave's light client {upper} (fork version, light-client data, header changes), build new images and re-pin before it activates; then add {name} to ethereum_forks in deploy/upstream.toml."),
            ));
        }
    }
    Ok(out)
}

/// Every Base Sepolia upgrade in base/base's chain config that `base_upgrades` does not list.
async fn base_upgrades(w: &Watch) -> Result<Vec<Item>> {
    let text = http()?
        .get(&w.base_config_url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let start = text
        .find("const SEPOLIA: ChainConfig = ChainConfig {")
        .context("no `const SEPOLIA: ChainConfig` in base/base's config; the file changed, fix base_config_url or this reader")?;
    let block = &text[start..];
    let block = &block[..block.find("\n};").unwrap_or(block.len())];
    let field = Regex::new(r"(\w+)_timestamp:\s*(?:Some\(\s*([\d_]+)\s*\)|(None)|([\d_]+))")?;
    let mut out = Vec::new();
    let mut seen = 0;
    for c in field.captures_iter(block) {
        seen += 1;
        let name = c[1].to_string();
        if name.starts_with("blobs_") || w.base_upgrades.contains(&name) {
            continue;
        }
        let at = c
            .get(2)
            .or_else(|| c.get(4))
            .and_then(|m| m.as_str().replace('_', "").parse::<u64>().ok());
        let title = match at {
            Some(t) => format!("Base Sepolia: upgrade {name} activates {}", date(t)),
            None => format!("Base Sepolia: upgrade {name} is named, not scheduled yet"),
        };
        out.push(rule_item(
            format!("base-upgrade:{name}:{}", at.map_or("unscheduled".into(), |t| t.to_string())),
            "base",
            "schedule",
            title,
            "maybe",
            "enclave image",
            at,
            format!("Check what {name} changes in base/base for block gossip (topic, payload version, envelope, signing). Watch base-to-celestia when it activates and send a test transfer; then add {name} to base_upgrades in deploy/upstream.toml."),
        ));
    }
    anyhow::ensure!(
        seen > 0,
        "no upgrade timestamps in base/base's SEPOLIA config; the format changed"
    );
    Ok(out)
}

/// The ArbOS version Arbitrum Sepolia runs, and any upgrade it has scheduled.
async fn arbos(ctx: &Ctx, w: &Watch) -> Result<Vec<Item>> {
    let rpc = chains::evm("arbitrum")?.rpc(ctx);
    let version =
        word(&eth_call(&rpc, ARBSYS, "arbOSVersion()", &[]).await?, 0)?.saturating_sub(ARBOS_OFFSET);
    let scheduled = eth_call(&rpc, ARB_OWNER_PUBLIC, "getScheduledUpgrade()", &[]).await?;
    let (next, at) = (word(&scheduled, 0)?, word(&scheduled, 1)?);
    let mut out = Vec::new();
    if !w.arbos_versions.contains(&version) {
        out.push(rule_item(
            format!("arbos:running:{version}"),
            "arbitrum",
            "schedule",
            format!("Arbitrum Sepolia runs ArbOS {version}, which this stack was not checked against"),
            "maybe",
            "enclave image",
            None,
            format!("Check the ArbOS {version} release notes for feed message or signature changes and watch arbitrum-to-celestia; then add {version} to arbos_versions in deploy/upstream.toml."),
        ));
    }
    if next != 0 && !w.arbos_versions.contains(&next) {
        out.push(rule_item(
            format!("arbos:scheduled:{next}:{at}"),
            "arbitrum",
            "schedule",
            format!("Arbitrum Sepolia: ArbOS {next} scheduled for {}", date(at)),
            "maybe",
            "enclave image",
            Some(at),
            format!("Check the ArbOS {next} release notes for feed message or signature changes before {}; then add {next} to arbos_versions in deploy/upstream.toml.", date(at)),
        ));
    }
    Ok(out)
}

/// The Base and Arbitrum sequencer keys the enclave pins, against the ones the chains use now.
async fn sequencer_keys(ctx: &Ctx, w: &Watch) -> Result<Vec<Item>> {
    let pinned = |chain: &str| -> Result<String> {
        let path = ctx
            .repo
            .join(format!("tee-hyperlane/crates/tee-node/src/chains/l2/{chain}.rs"));
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        let re = Regex::new(r#"pub const SEQUENCER: Address = address!\("([0-9a-fA-F]{40})"\)"#)?;
        let c = re
            .captures(&text)
            .with_context(|| format!("no `SEQUENCER` address in {}", path.display()))?;
        Ok(format!("0x{}", c[1].to_lowercase()))
    };
    let l1 = chains::evm("sepolia")?.rpc(ctx);
    let mut out = Vec::new();

    let base = pinned("base")?;
    let signer = eth_call(&l1, &w.base_system_config, "unsafeBlockSigner()", &[]).await?;
    let signer = format!(
        "0x{}",
        hex::encode(signer.get(12..32).context("short unsafeBlockSigner answer")?)
    );
    if signer != base {
        out.push(rule_item(
            format!("base-signer:{signer}"),
            "base",
            "key",
            format!("Base Sepolia's unsafe block signer is now {signer}; the enclave pins {base}"),
            "breaking",
            "enclave image",
            None,
            "Set SEQUENCER in tee-node/src/chains/l2/base.rs to the new key, build new images and re-pin. base-to-celestia refuses every block until then.".into(),
        ));
    }

    let arb = pinned("arbitrum")?;
    let mut arg = [0u8; 32];
    arg[12..].copy_from_slice(&hex::decode(arb.trim_start_matches("0x"))?);
    let still = word(
        &eth_call(&l1, &w.arbitrum_sequencer_inbox, "isSequencer(address)", &arg).await?,
        0,
    )?;
    if still == 0 {
        out.push(rule_item(
            format!("arb-sequencer-removed:{arb}"),
            "arbitrum",
            "key",
            format!("Arbitrum Sepolia's SequencerInbox no longer lists {arb}, the sequencer the enclave pins"),
            "breaking",
            "enclave image",
            None,
            "Find the new sequencer key, set SEQUENCER in tee-node/src/chains/l2/arbitrum.rs, build new images and re-pin. arbitrum-to-celestia stops until then.".into(),
        ));
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn rule_item(
    id: String,
    chain: &str,
    source: &str,
    title: String,
    verdict: &str,
    component: &str,
    activates_at: Option<u64>,
    action: String,
) -> Item {
    Item {
        id,
        chain: chain.into(),
        source: source.into(),
        summary: Some(title.clone()),
        title,
        found_at: now(),
        verdict: verdict.into(),
        judged_by: Some("rule".into()),
        component: Some(component.into()),
        activation: Some(activates_at.map_or("not scheduled".into(), date)),
        activates_at,
        action: Some(action),
        ..Default::default()
    }
}

async fn eth_call(rpc: &str, to: &str, signature: &str, args: &[u8]) -> Result<Vec<u8>> {
    let mut data = alloy::primitives::keccak256(signature.as_bytes())[..4].to_vec();
    data.extend_from_slice(args);
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "eth_call",
        "params": [{"to": to, "data": format!("0x{}", hex::encode(data))}, "latest"]});
    let v: Value = http()?.post(rpc).json(&body).send().await?.json().await?;
    if let Some(e) = v.get("error") {
        bail!("{signature} on {to}: {e}");
    }
    let out = v["result"]
        .as_str()
        .with_context(|| format!("{signature} on {to}: no result in {v}"))?;
    Ok(hex::decode(out.trim_start_matches("0x"))?)
}

/// The `i`th 32-byte word of an ABI answer, as a u64.
fn word(data: &[u8], i: usize) -> Result<u64> {
    let w = data
        .get(i * 32..i * 32 + 32)
        .with_context(|| format!("the answer has no word {i}"))?;
    anyhow::ensure!(
        w[..24].iter().all(|b| *b == 0),
        "word {i} does not fit in 64 bits"
    );
    Ok(u64::from_be_bytes(w[24..].try_into()?))
}

// ---------------------------------------------------------------- releases

async fn releases(ctx: &Ctx, r: &Release, since_days: u64) -> Result<Vec<Item>> {
    let per_page = if r.tag_prefix.is_some() { 100 } else { 20 };
    let mut req = http()?
        .get(format!(
            "https://api.github.com/repos/{}/releases?per_page={per_page}",
            r.repo
        ))
        .header("Accept", "application/vnd.github+json");
    if let Some(token) = ctx.var("GITHUB_TOKEN") {
        req = req.bearer_auth(token);
    }
    let resp = req.send().await?;
    let status = resp.status();
    let v: Value = resp.json().await?;
    anyhow::ensure!(status.is_success(), "GitHub answered {status}: {}", v["message"]);
    let cutoff = now().saturating_sub(since_days * 86_400);
    let mut out = Vec::new();
    for rel in v.as_array().context("GitHub returned no list")? {
        let tag = rel["tag_name"].as_str().unwrap_or_default();
        if rel["draft"].as_bool() == Some(true)
            || r.tag_prefix.as_deref().is_some_and(|p| !tag.starts_with(p))
        {
            continue;
        }
        let published = rel["published_at"].as_str().and_then(parse_time);
        if published.is_some_and(|t| t < cutoff) {
            continue;
        }
        let name = rel["name"].as_str().filter(|n| !n.is_empty()).unwrap_or(tag);
        let mut body = rel["body"].as_str().unwrap_or_default().to_string();
        if body.len() > MAX_BODY {
            let mut cut = MAX_BODY;
            while !body.is_char_boundary(cut) {
                cut -= 1;
            }
            body.truncate(cut);
            body.push_str("\n[cut]");
        }
        out.push(Item {
            id: format!("gh:{}:{tag}", r.repo),
            chain: r.chain.clone(),
            source: "release".into(),
            title: format!("{} {name}", r.repo),
            url: rel["html_url"].as_str().map(str::to_string),
            published_at: published,
            found_at: now(),
            verdict: "unjudged".into(),
            body: Some(body),
            ..Default::default()
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------- the judge

struct Judge {
    key: String,
    model: String,
}

#[derive(Deserialize)]
struct Verdict {
    verdict: String,
    component: String,
    summary: String,
    reasoning: String,
    quote: String,
    affects: String,
    action: String,
    activation: String,
}

impl Verdict {
    fn apply(self, item: &mut Item, model: &str) {
        item.verdict = self.verdict;
        item.judged_by = Some(model.to_string());
        item.component = Some(self.component);
        item.summary = Some(self.summary);
        item.reasoning = Some(self.reasoning);
        item.quote = Some(self.quote).filter(|q| !q.is_empty());
        item.affects = Some(self.affects).filter(|a| !a.is_empty());
        item.action = Some(self.action).filter(|a| !a.is_empty());
        item.activation = Some(self.activation);
        item.body = None;
    }
}

const SYSTEM: &str = "You decide whether one update to a dependency could break a specific deployed bridge. \
You get the bridge's dependency manifest and one update (release notes, a spec release or a scheduled upgrade). \
Judge only against what the manifest says the bridge depends on, on the networks it uses: Ethereum Sepolia, \
Base Sepolia, Arbitrum Sepolia, Eden testnet, Celestia mocha, Phala Cloud. \
breaking: once deployed or active there, the update changes something the manifest lists, so the bridge stops working until we change our side. \
maybe: it could, or the text does not say enough to rule it out. \
irrelevant: nothing in it touches what the manifest lists (performance, metrics, CLI, docs, other networks only). \
When in doubt, choose maybe: a missed break costs days of downtime, a false alarm a minute of reading. \
quote: copy verbatim the sentences of the update that drive the verdict, empty for irrelevant. \
affects: the dependency from the manifest it concerns, in the manifest's words. \
action: what we would have to change, concretely, or empty. \
activation: when it takes effect on the networks we use, if the text says, else 'not stated'. \
summary: one sentence a busy operator can act on.";

impl Judge {
    fn new(ctx: &Ctx) -> Result<Self> {
        Ok(Self {
            key: ctx.require(
                "OPENAI_API_KEY",
                "it judges each release against deploy/upstream.toml",
            )?,
            model: ctx.var_or("OPENAI_MODEL", DEFAULT_MODEL),
        })
    }

    async fn judge(&self, manifest: &str, item: &Item) -> Result<Verdict> {
        let update = format!(
            "Update for {}: {}\nURL: {}\nPublished: {}\n\n{}",
            item.chain,
            item.title,
            item.url.as_deref().unwrap_or("none"),
            item.published_at.map_or("unknown".into(), date),
            item.body.as_deref().unwrap_or(""),
        );
        self.ask(manifest, &update).await
    }

    async fn ask(&self, manifest: &str, update: &str) -> Result<Verdict> {
        let text = |d: &str| json!({"type": "string", "description": d});
        let schema = json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["verdict", "component", "summary", "reasoning", "quote", "affects", "action", "activation"],
            "properties": {
                "verdict": {"type": "string", "enum": ["breaking", "maybe", "irrelevant"]},
                "component": {"type": "string", "enum": ["enclave image", "relayer", "ism contracts", "celestia-app", "pccs", "operations", "none"]},
                "summary": text("one sentence"),
                "reasoning": text("why, in a few sentences"),
                "quote": text("verbatim from the update"),
                "affects": text("the manifest dependency"),
                "action": text("what we would have to change"),
                "activation": text("when it takes effect on our networks"),
            }
        });
        let body = json!({
            "model": self.model,
            "messages": [
                {"role": "system", "content": SYSTEM},
                {"role": "user", "content": format!("Dependency manifest:\n{manifest}\n\n---\n\n{update}")},
            ],
            "response_format": {"type": "json_schema", "json_schema": {"name": "verdict", "strict": true, "schema": schema}},
        });
        let resp = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .build()?
            .post("https://api.openai.com/v1/chat/completions")
            .bearer_auth(&self.key)
            .json(&body)
            .send()
            .await
            .context("reaching api.openai.com")?;
        let status = resp.status();
        let v: Value = resp.json().await.context("OpenAI's answer is not JSON")?;
        if !status.is_success() {
            let msg = v["error"]["message"].as_str().unwrap_or("no message");
            if v["error"]["code"].as_str() == Some("model_not_found") {
                bail!(
                    "OpenAI has no model `{}` for this key ({msg}); set OPENAI_MODEL in devnet/.env",
                    self.model
                );
            }
            bail!("OpenAI answered {status}: {msg}");
        }
        let message = &v["choices"][0]["message"];
        if let Some(refusal) = message["refusal"].as_str() {
            bail!("the model refused: {refusal}");
        }
        let content = message["content"]
            .as_str()
            .context("OpenAI's answer has no content")?;
        let verdict: Verdict = serde_json::from_str(content)
            .with_context(|| format!("the verdict is not the schema: {content}"))?;
        anyhow::ensure!(
            matches!(verdict.verdict.as_str(), "breaking" | "maybe" | "irrelevant"),
            "unknown verdict {}",
            verdict.verdict
        );
        Ok(verdict)
    }
}

// ---------------------------------------------------------------- slack

/// Post every open item not posted yet. Without SLACK_BOT_TOKEN and SLACK_CHANNEL there is
/// nowhere to post, and the dashboard alone shows them.
async fn alert(ctx: &Ctx, state: &mut State, errors: &mut Vec<String>) {
    let (Some(token), Some(channel)) = (ctx.var("SLACK_BOT_TOKEN"), ctx.var("SLACK_CHANNEL")) else {
        return;
    };
    for item in state
        .items
        .iter_mut()
        .filter(|i| i.open() && i.alerted_at.is_none())
    {
        let mut text = format!("*Upstream, {}:* {}", item.verdict, item.title);
        if let Some(s) = item.summary.as_deref().filter(|s| *s != item.title) {
            text.push_str(&format!("\n{s}"));
        }
        if let Some(a) = &item.action {
            text.push_str(&format!("\nAction: {a}"));
        }
        if let Some(a) = &item.activation {
            text.push_str(&format!("\nActivates: {a}"));
        }
        if let Some(u) = &item.url {
            text.push_str(&format!("\n{u}"));
        }
        text.push_str(&format!("\nWhen handled: `teeism upstream ack {}`", item.id));
        let sent = async {
            let v: Value = http()?
                .post("https://slack.com/api/chat.postMessage")
                .bearer_auth(&token)
                .json(&json!({"channel": channel, "text": text, "unfurl_links": false}))
                .send()
                .await?
                .json()
                .await?;
            anyhow::ensure!(
                v["ok"].as_bool() == Some(true),
                "slack refused the post: {}",
                v["error"]
            );
            Ok::<_, anyhow::Error>(())
        };
        match sent.await {
            Ok(()) => item.alerted_at = Some(now()),
            Err(e) => {
                errors.push(format!("posting {} to slack: {e:#}", item.id));
                return;
            }
        }
    }
}

// ---------------------------------------------------------------- list, ack, eval

pub fn list(ctx: &Ctx, all: bool) -> Result<()> {
    let state = load_state(ctx)?;
    match state.checked_at {
        Some(t) => print::kv("last check", date(t)),
        None => print::warn("never checked; run `teeism upstream check`"),
    }
    for e in &state.errors {
        print::warn(format!("last check could not read {e}"));
    }
    print_open(&state, all);
    Ok(())
}

fn print_open(state: &State, all: bool) {
    let rank = |v: &str| match v {
        "breaking" => 0,
        "maybe" => 1,
        "unjudged" => 2,
        _ => 3,
    };
    let mut items: Vec<&Item> = state.items.iter().filter(|i| all || i.open()).collect();
    items.sort_by_key(|i| (rank(&i.verdict), i.activates_at.unwrap_or(u64::MAX)));
    if items.is_empty() {
        print::ok("nothing open upstream");
        return;
    }
    print::step(if all { "every item" } else { "open upstream items" });
    for i in items {
        let acked = if i.acked_at.is_some() { " (acked)" } else { "" };
        println!("  {:<10} {}{acked}", i.verdict, i.title);
        if let Some(s) = i.summary.as_deref().filter(|s| *s != i.title) {
            println!("             {s}");
        }
        if let Some(a) = &i.action {
            println!("             action: {a}");
        }
        println!("             id: {}", i.id);
    }
}

pub fn ack(ctx: &Ctx, id: &str, note: Option<String>) -> Result<()> {
    let mut state = load_state(ctx)?;
    let matches: Vec<usize> = (0..state.items.len())
        .filter(|&i| state.items[i].id == id || state.items[i].id.starts_with(id))
        .collect();
    let i = match matches.as_slice() {
        [i] => *i,
        [] => bail!("no upstream item {id}; `teeism upstream list --all` shows the ids"),
        _ => bail!(
            "{id} matches {} items; give more of the id:\n  {}",
            matches.len(),
            matches
                .iter()
                .map(|&i| state.items[i].id.as_str())
                .collect::<Vec<_>>()
                .join("\n  ")
        ),
    };
    state.items[i].acked_at = Some(now());
    state.items[i].note = note;
    save_state(ctx, &state)?;
    print::ok(format!("acked {}", state.items[i].title));
    Ok(())
}

#[derive(Deserialize)]
struct EvalSet {
    case: Vec<EvalCase>,
}

#[derive(Deserialize)]
struct EvalCase {
    title: String,
    chain: String,
    text: String,
    /// `relevant` (breaking or maybe is right) or `irrelevant`.
    expect: String,
}

/// Run the judge on known history, so a prompt or model change can be checked before it is
/// trusted with the real feed.
pub async fn eval(ctx: &Ctx) -> Result<()> {
    let watch = load_watch(ctx)?;
    let path = ctx.repo.join("deploy/upstream-eval.toml");
    let set: EvalSet = toml::from_str(
        &std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?,
    )
    .with_context(|| format!("parsing {}", path.display()))?;
    let judge = Judge::new(ctx)?;
    print::kv("model", &judge.model);
    let mut misses = Vec::new();
    for case in &set.case {
        let update = format!("Update for {}: {}\n\n{}", case.chain, case.title, case.text);
        let v = judge.ask(&watch.manifest.text, &update).await?;
        let right = match case.expect.as_str() {
            "relevant" => v.verdict != "irrelevant",
            "irrelevant" => v.verdict == "irrelevant",
            other => bail!(
                "{}: expect must be relevant or irrelevant, not {other}",
                case.title
            ),
        };
        if right {
            print::ok(format!("{:<10} {}", v.verdict, case.title));
        } else {
            print::bad(format!(
                "{:<10} {} (expected {})",
                v.verdict, case.title, case.expect
            ));
            misses.push(format!(
                "{}: judged {}, expected {}; {}",
                case.title, v.verdict, case.expect, v.reasoning
            ));
        }
    }
    if !misses.is_empty() {
        return Err(Incomplete(misses).into());
    }
    print::ok(format!("all {} cases judged as expected", set.case.len()));
    Ok(())
}

// ---------------------------------------------------------------- time

/// `2026-10-21 18:00 UTC`.
pub fn date(t: u64) -> String {
    let days = (t / 86_400) as i64;
    let secs = t % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02} UTC",
        secs / 3600,
        secs % 3600 / 60
    )
}

/// `2026-10-21T18:00:00Z` as unix seconds.
fn parse_time(s: &str) -> Option<u64> {
    let (date, time) = s.trim_end_matches('Z').split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, dd) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.split(':').map(|p| p.split('.').next()?.parse::<u64>().ok());
    let (h, mi, se) = (t.next()??, t.next()??, t.next().flatten().unwrap_or(0));
    let days = days_from_civil(y, m, dd);
    Some(days as u64 * 86_400 + h * 3600 + mi * 60 + se)
}

// Howard Hinnant's civil calendar conversions.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_round_trip() {
        assert_eq!(date(1_792_605_600), "2026-10-21 18:00 UTC");
        assert_eq!(parse_time("2026-10-21T18:00:00Z"), Some(1_792_605_600));
        assert_eq!(parse_time("2024-02-29T00:00:01Z"), Some(1_709_164_801));
    }

    #[test]
    fn the_filter_keeps_forks_and_drops_metrics_fixes() {
        let f = relevance_filter().unwrap();
        assert!(f.is_match("Schedule the Denim upgrade on Sepolia"));
        assert!(f.is_match("New gossip topic for v5 payloads"));
        assert!(!f.is_match("Fix a typo in the metrics dashboard"));
    }
}
