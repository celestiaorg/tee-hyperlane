//! Intel's PCS: the collateral every TDX quote is checked against, fetched and parsed. Every
//! artifact is signed by a chain ending in Intel SGX Root CA, so nothing here is trusted: the
//! DAOs on chain check each signature.

use anyhow::{bail, Context, Result};
use std::collections::BTreeSet;

pub const SGX_API: &str = "https://api.trustedservices.intel.com/sgx/certification/v4";
pub const TDX_API: &str = "https://api.trustedservices.intel.com/tdx/certification/v4";
const ROOT_CRL_FALLBACK: &str = "https://certificates.trustedservices.intel.com/IntelSGXRootCA.der";

/// A signed JSON object exactly as Intel served it, and its signature.
pub struct Signed {
    pub body: String,
    pub signature: Vec<u8>,
}

pub struct Intel {
    http: reqwest::Client,
}

pub struct Fetched {
    pub body: Vec<u8>,
    pub headers: reqwest::header::HeaderMap,
}

impl Intel {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
        }
    }

    /// GET with retries: Intel rate-limits, which is the common failure for a scheduled run.
    pub async fn get(&self, what: &str, url: &str) -> Result<Fetched> {
        let mut last = String::new();
        for attempt in 0..4 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(2 * attempt)).await;
            }
            match self
                .http
                .get(url)
                .timeout(std::time::Duration::from_secs(60))
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => {
                    let headers = r.headers().clone();
                    let body = r
                        .bytes()
                        .await
                        .with_context(|| format!("reading {what} from {url}"))?
                        .to_vec();
                    return Ok(Fetched { body, headers });
                }
                Ok(r) => last = format!("HTTP {}", r.status()),
                Err(e) => last = e.to_string(),
            }
        }
        bail!("could not fetch {what} from Intel ({url}): {last}")
    }

    pub async fn json(&self, what: &str, url: &str) -> Result<(String, reqwest::header::HeaderMap)> {
        let f = self.get(what, url).await?;
        let s = String::from_utf8(f.body).with_context(|| format!("{what} from {url} is not text"))?;
        Ok((s, f.headers))
    }

    /// The TDX evaluation numbers Intel publishes.
    pub async fn tdx_evals(&self) -> Result<(BTreeSet<u32>, String)> {
        let (raw, _) = self
            .json(
                "Intel's TDX evaluation numbers",
                &format!("{TDX_API}/tcbevaluationdatanumbers"),
            )
            .await?;
        let v: serde_json::Value =
            serde_json::from_str(&raw).context("Intel's evaluation-number list is not JSON")?;
        let list = v["tcbEvaluationDataNumbers"]["tcbEvalNumbers"]
            .as_array()
            .context("Intel's evaluation-number list has no tcbEvalNumbers")?;
        let set = list
            .iter()
            .filter_map(|x| x["tcbEvaluationDataNumber"].as_u64().map(|n| n as u32))
            .collect();
        Ok((set, raw))
    }

    /// The evaluation number Intel calls standard today: the one the verifier reads.
    pub async fn standard(&self, fmspc: &str) -> Result<u32> {
        let (raw, _) = self
            .json("TCB info", &format!("{TDX_API}/tcb?fmspc={fmspc}"))
            .await?;
        let v: serde_json::Value = serde_json::from_str(&raw).context("Intel's TCB info is not JSON")?;
        v["tcbInfo"]["tcbEvaluationDataNumber"]
            .as_u64()
            .map(|n| n as u32)
            .context("Intel's TCB info names no tcbEvaluationDataNumber")
    }
}

/// The evaluation numbers a new deployment needs: Intel's standard today, and every number
/// Intel already publishes after it, so the day the next one becomes standard is covered.
pub async fn evals_to_deploy(intel: &Intel, fmspc: &str) -> Result<BTreeSet<u32>> {
    let standard = intel.standard(fmspc).await?;
    let (published, _) = intel.tdx_evals().await?;
    let mut out: BTreeSet<u32> = published.into_iter().filter(|n| *n >= standard).collect();
    out.insert(standard);
    Ok(out)
}

/// The certificates in an issuer-chain header: URL-escaped PEM, leaf first, root last.
pub fn issuer_chain(headers: &reqwest::header::HeaderMap, name: &str) -> Result<Vec<Vec<u8>>> {
    let raw = headers
        .get(name)
        .with_context(|| format!("Intel's response carried no {name} header"))?
        .to_str()
        .with_context(|| format!("{name} is not text"))?;
    let pem = percent_encoding::percent_decode_str(raw)
        .decode_utf8()
        .context("issuer chain is not UTF-8")?;
    let mut certs = Vec::new();
    for block in pem.split("-----BEGIN CERTIFICATE-----").skip(1) {
        let body = block.split("-----END CERTIFICATE-----").next().unwrap_or("");
        let b64: String = body.chars().filter(|c| !c.is_whitespace()).collect();
        use base64::Engine;
        certs.push(
            base64::engine::general_purpose::STANDARD
                .decode(b64)
                .context("a certificate in the chain is not base64")?,
        );
    }
    if certs.len() < 2 {
        bail!(
            "{name} has {} certificate(s), expected the issuer and the root",
            certs.len()
        );
    }
    Ok(certs)
}

/// The root CA CRL's location, from the root certificate's distribution point.
pub fn crl_url(root: &[u8]) -> String {
    use x509_parser::extensions::{DistributionPointName, GeneralName, ParsedExtension};
    if let Ok((_, cert)) = x509_parser::parse_x509_certificate(root) {
        for ext in cert.extensions() {
            if let ParsedExtension::CRLDistributionPoints(points) = ext.parsed_extension() {
                for p in points.iter() {
                    if let Some(DistributionPointName::FullName(names)) = &p.distribution_point {
                        for n in names {
                            if let GeneralName::URI(uri) = n {
                                return uri.to_string();
                            }
                        }
                    }
                }
            }
        }
    }
    ROOT_CRL_FALLBACK.to_string()
}

/// The object under `key`, lifted from Intel's response as the exact bytes it served. The
/// signature covers those bytes, so it is never re-serialised.
pub fn signed_object(raw: &str, key: &str) -> Result<Signed> {
    let at = raw
        .find(&format!("\"{key}\""))
        .with_context(|| format!("no \"{key}\" in Intel's response"))?;
    let open = at
        + raw[at..]
            .find('{')
            .with_context(|| format!("\"{key}\" is not an object"))?;
    let mut depth = 0usize;
    let mut end = None;
    for (i, ch) in raw[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(open + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end.with_context(|| format!("\"{key}\" never closes"))?;
    let v: serde_json::Value = serde_json::from_str(raw).context("Intel's response is not JSON")?;
    let sig = v["signature"]
        .as_str()
        .context("Intel's response has no signature")?;
    Ok(Signed {
        body: raw[open..=end].to_string(),
        signature: hex::decode(sig).context("Intel's signature is not hex")?,
    })
}
