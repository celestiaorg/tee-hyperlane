//! `teeism isms celestia`: an x/teeism ISM per EVM origin on Celestia, the routing ISM over
//! them, and the tokens and mailbox pointed at it. Re-pins in place on an identity change.

use anyhow::{bail, Context, Result};
use base64::Engine;

use super::*;
use crate::shared::celestia_cli::{self, Appd};
use crate::shared::chains;
use crate::shared::incomplete::Incomplete;
use crate::shared::print;
use crate::shared::settings::Ctx;
use crate::steps::{config, enclaves};

struct CelestiaIsm {
    state: Vec<u8>,
    owner: String,
}

fn query_ism(appd: &Appd, id: &str) -> Result<CelestiaIsm> {
    let v = appd.query(&format!("reading ISM {id}"), &["teeism", "ism", id])?;
    let state = base64::engine::general_purpose::STANDARD
        .decode(
            v["ism"]["state"]
                .as_str()
                .with_context(|| format!("ISM {id} has no state: {v}"))?,
        )
        .with_context(|| format!("ISM {id}'s state is not base64"))?;
    Ok(CelestiaIsm {
        state,
        owner: v["ism"]["owner"].as_str().unwrap_or("").to_string(),
    })
}

/// `teeism isms celestia`
pub async fn run(ctx: &Ctx, selection: &[String]) -> Result<()> {
    celestia_cli::wait_for_chain(ctx).await?;
    let appd = Appd::new(ctx)?;
    config::write(ctx)?;
    let relayer = appd.address("relayer")?;
    let mut incomplete = Vec::new();

    for chain in chains::evm_selection(selection)? {
        let enclave = chains::enclave_for(chain.name);
        print::step(format!("{} origin ({enclave} enclave)", chain.label));
        let mut d = ctx.deployment()?;
        let identity = d
            .enclaves
            .chains
            .get(enclave)
            .and_then(|e| e.identity_digest.clone())
            .with_context(|| format!("no identity for the {enclave} enclave; run `teeism enclaves up`"))?;
        let identity_file = enclaves::identity_file(ctx, enclave);
        let want = hex::encode(b32(&identity)?);
        let mut genesis = None;

        if let Some(existing) = d.celestia.isms.get(chain.name).cloned() {
            let ism = query_ism(&appd, &existing)?;
            let pinned = hex::encode(&ism.state[ism.state.len().saturating_sub(32)..]);
            if pinned == want {
                print::info(format!("{existing} already pins this enclave"));
                continue;
            }
            if ism.owner == relayer {
                print::info(format!("re-pinning {existing}: 0x{pinned} -> 0x{want}"));
                appd.tx(
                    &format!("re-pinning the {} ISM", chain.name),
                    "relayer",
                    &[
                        "teeism",
                        "update-identity",
                        &existing,
                        &identity_file.to_string_lossy(),
                    ],
                )?;
                let after = query_ism(&appd, &existing)?;
                if hex::encode(&after.state[after.state.len() - 32..]) != want {
                    bail!("{existing} does not pin the new identity after the re-pin");
                }
                print::ok(format!("{existing} re-pinned"));
                continue;
            }
            print::warn(format!(
                "{existing} is owned by {}, not the relayer {relayer}; replacing it from its last state",
                ism.owner
            ));
            genesis = Some(resume_from(&ism.state, &identity)?);
        }

        // A bootstrap that fails (Eden before the mocha light node has synced, Base without an
        // archive key) leaves this origin for the next run, rather than stopping the others.
        let genesis = match genesis {
            Some(g) => g,
            None => match genesis_at_head(ctx, chain.name, &identity) {
                Ok(g) => g,
                Err(e) => {
                    print::warn(format!("{}: {e:#}", chain.name));
                    incomplete.push(format!(
                        "{}: could not build its genesis state (the reason is above)",
                        chain.name
                    ));
                    continue;
                }
            },
        };
        let identity_json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&identity_file)
                .with_context(|| format!("reading {}", identity_file.display()))?,
        )?;
        let tree = celestia_cli::pad32(&merkle_hook_of(&d, chain)?);
        let origin = serde_json::json!({ "state": format!("0x{}", hex::encode(&genesis)), "merkle_tree_address": tree, "identity": identity_json });
        let file = ctx
            .state_dir
            .join("identity")
            .join(format!("ism-{}-origin.json", chain.name));
        std::fs::write(&file, serde_json::to_string_pretty(&origin)?)?;
        let tx = appd.tx(
            &format!("creating the {} ISM", chain.name),
            "relayer",
            &["teeism", "create", &file.to_string_lossy()],
        )?;
        let id = tx
            .find_attr(|t| t.contains("teeism"), "id")
            .with_context(|| format!("tx {} created no teeism ISM id", tx.hash))?;
        d.celestia.isms.insert(chain.name.to_string(), id.clone());
        ctx.save(&d)?;
        print::ok(id);
    }

    // Four origins deliver into one token, and each ISM admits one origin domain, so the tokens
    // and the mailbox point at a routing ISM that sends each domain to its own.
    print::step("routing ISM");
    let mut d = ctx.deployment()?;
    if d.celestia.routing_ism.is_none() {
        let tx = appd.tx(
            "creating the routing ISM",
            "relayer",
            &["hyperlane", "ism", "create-routing"],
        )?;
        let id = tx
            .find_attr(|t| t.to_lowercase().contains("routing"), "ism_id")
            .or_else(|| tx.find_attr(|t| t.to_lowercase().contains("routing"), "id"))
            .with_context(|| format!("tx {} created no routing ISM id", tx.hash))?;
        d.celestia.routing_ism = Some(id);
        ctx.save(&d)?;
    }
    let routing = d.celestia.routing_ism.clone().unwrap_or_default();
    print::kv("routing ism", &routing);
    let current = appd
        .query("reading the routing ISM", &["hyperlane", "ism", "ism", &routing])
        .unwrap_or_default();
    let current = current.to_string();
    for chain in chains::evm_selection(selection)? {
        let Some(ism) = d.celestia.isms.get(chain.name) else {
            continue;
        };
        // Already routed: the domain and this ISM id appear together in the routing ISM.
        if current.contains(&ism.to_lowercase()) || current.contains(ism.as_str()) {
            print::info(format!("domain {} -> {ism}", chain.domain));
            continue;
        }
        // Remove, then set: a set on a domain already present has been seen to succeed and
        // change nothing. Removing an absent domain is a no-op.
        appd.tx(
            &format!("clearing domain {}", chain.domain),
            "relayer",
            &[
                "hyperlane",
                "ism",
                "remove-routing-ism-domain",
                &routing,
                &chain.domain.to_string(),
            ],
        )?;
        appd.tx(
            &format!("routing domain {} to {ism}", chain.domain),
            "relayer",
            &[
                "hyperlane",
                "ism",
                "set-routing-ism-domain",
                &routing,
                &chain.domain.to_string(),
                ism,
            ],
        )?;
        print::info(format!("domain {} -> {ism}", chain.domain));
    }

    print::step("pointing the tokens and the mailbox at the routing ISM");
    for (name, token) in [
        ("TIA", &d.celestia.tia_token),
        ("teeUSD", &d.celestia.teeusd_token),
    ] {
        let Some(token) = token else { continue };
        let t = appd.query(&format!("reading the {name} token"), &["warp", "token", token])?;
        if t["token"]["ism_id"].as_str() != Some(routing.as_str()) {
            appd.tx(
                &format!("pointing {name} at the routing ISM"),
                "relayer",
                &["warp", "set-token", token, "--ism-id", &routing],
            )?;
        }
        print::info(format!("{name} {token}"));
    }
    let mailbox = d
        .celestia
        .mailbox
        .clone()
        .context("no mailbox; run `teeism celestia core`")?;
    let m = appd.query("reading the mailbox", &["hyperlane", "mailbox", &mailbox])?;
    if m["mailbox"]["default_ism"].as_str() != Some(routing.as_str()) {
        appd.tx(
            "pointing the mailbox at the routing ISM",
            "relayer",
            &["hyperlane", "mailbox", "set", &mailbox, "--default-ism", &routing],
        )?;
    }
    print::info(format!("mailbox {mailbox}"));
    if !incomplete.is_empty() {
        return Err(Incomplete(incomplete).into());
    }
    Ok(())
}
