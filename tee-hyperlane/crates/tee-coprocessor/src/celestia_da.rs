//! Reading blobs and their inclusion proofs from a celestia-node.
//!
//! The consensus RPC cannot serve these. A block there carries a `data_hash` but not the row
//! roots behind it, and the blob bytes are in the square rather than in the transaction list
//! in any form that is cheap to pull apart. So an evolve origin needs a DA node as well as a
//! consensus endpoint.
//!
//! Nothing here is trusted. The enclave re-derives `dah.hash()` and checks it against the
//! `data_hash` in the header its own light client verified, so a node that lies about row
//! roots, blobs or proofs produces an attestation that fails rather than one that is wrong.

use anyhow::{Context, Result};
use celestia_types::nmt::{Namespace, NamespaceProof};
use celestia_types::{Blob, DataAvailabilityHeader};
use serde::Deserialize;
use serde_json::json;

pub struct DaClient {
    url: String,
    http: reqwest::Client,
}

#[derive(Debug, Deserialize)]
struct ExtendedHeader {
    dah: DataAvailabilityHeader,
}

impl DaClient {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(60))
                .build()
                .expect("http client"),
        }
    }

    async fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<T> {
        let body = json!({"jsonrpc":"2.0","id":1,"method":method,"params":params});
        let res = self
            .http
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .with_context(|| format!("{method} to the DA node"))?;
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("{method}: DA node returned {status}: {}", first_line(&text));
        }
        let parsed: serde_json::Value =
            serde_json::from_str(&text).with_context(|| format!("{method}: not json"))?;
        if let Some(err) = parsed.get("error") {
            anyhow::bail!("{method}: {}", first_line(&err.to_string()));
        }
        let result = parsed
            .get("result")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("{method}: no result"))?;
        serde_json::from_value(result).with_context(|| format!("{method}: unexpected shape"))
    }

    /// The data availability header for a Celestia block: the row roots the namespace proofs
    /// are verified against.
    pub async fn dah(&self, height: u64) -> Result<DataAvailabilityHeader> {
        let header: ExtendedHeader = self.call("header.GetByHeight", json!([height])).await?;
        Ok(header.dah)
    }

    /// Every blob in one namespace at one height, in square order.
    ///
    /// An evolve chain posts one blob per block, so a Celestia block that covers a minute of
    /// Eden holds hundreds of them.
    pub async fn blobs(&self, height: u64, ns: &Namespace) -> Result<Vec<Blob>> {
        // `GetAll` answers with null rather than an empty list when the namespace is absent
        // from the block, which is a normal thing for it to be.
        let blobs: Option<Vec<Blob>> = self.call("blob.GetAll", json!([height, [ns]])).await?;
        Ok(blobs.unwrap_or_default())
    }

    /// The namespace proofs placing one blob in that block's rows.
    pub async fn proof(&self, height: u64, ns: &Namespace, commitment: &[u8]) -> Result<Vec<NamespaceProof>> {
        self.call("blob.GetProof", json!([height, ns, commitment]))
            .await
    }

    pub async fn head(&self) -> Result<u64> {
        #[derive(Deserialize)]
        struct H {
            header: Inner,
        }
        #[derive(Deserialize)]
        struct Inner {
            height: String,
        }
        let h: H = self.call("header.LocalHead", json!([])).await?;
        Ok(h.header.height.parse()?)
    }
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").chars().take(200).collect()
}
