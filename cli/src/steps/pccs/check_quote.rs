//! `teeism pccs check-quote`: run a TDX quote through each chain's DCAP verifier, exactly as its
//! ISM will. Read only.

use anyhow::{bail, Context, Result};

use super::IEntrypoint;
use crate::shared::chains;
use crate::shared::evm;
use crate::shared::print;
use crate::shared::record::Pccs;
use crate::shared::settings::Ctx;

/// `teeism pccs check-quote`: a quote from a file, or fresh from a recorded enclave.
pub async fn run(
    ctx: &Ctx,
    chains: &[String],
    quote: Option<std::path::PathBuf>,
    enclave: Option<String>,
) -> Result<()> {
    let d = ctx.deployment()?;
    let raw = match (quote, enclave) {
        (Some(path), _) => {
            let text =
                std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
            hex::decode(text.trim().trim_start_matches("0x"))
                .with_context(|| format!("{} is not hex", path.display()))?
        }
        (None, Some(e)) => {
            let url = d
                .enclaves
                .chains
                .get(&e)
                .map(|x| x.url.clone())
                .with_context(|| format!("no {e} enclave recorded"))?;
            crate::steps::enclaves::fetch_identity(&url).await?.quote
        }
        (None, None) => bail!("give --quote <file> or --enclave <chain>"),
    };
    for chain in chains::evm_selection(chains)? {
        let p = super::pccs_for(&d, chain)?;
        check_quote(&chain.rpc(ctx), p, &raw)
            .await
            .with_context(|| format!("on {}", chain.label))?;
        print::ok(format!("{} accepts the quote", chain.label));
    }
    Ok(())
}

/// Run a quote through the chain's DCAP verifier, as the ISM will. Returns why it was refused.
pub async fn check_quote(rpc: &str, p: &Pccs, quote: &[u8]) -> Result<()> {
    let r = evm::reader(rpc)?;
    let res = IEntrypoint::new(p.entrypoint, &r)
        .verifyAndAttestOnChain(quote.to_vec().into())
        .call()
        .await
        .map_err(|e| anyhow::anyhow!("the verifier reverted: {}", evm::explain_revert(&e.to_string())))?;
    if !res.success {
        let why = String::from_utf8_lossy(&res.output);
        bail!(
            "the verifier refused the quote: {why} (0x{})",
            hex::encode(&res.output)
        );
    }
    Ok(())
}
