//! Reads an enclave's identity (measurements, compose hash, KMS) from its quote and event log.

use anyhow::{Context, Result};
use hyperlane_types::dstack::{replay, value, EventLog};
use serde::Serialize;

/// The fields the Celestia ISMs pin, as `x/teeism`'s `create` reads them. The KMS is pinned
/// through `key_provider`: dstack 0.6 no longer records an `mr-kms` event.
#[derive(Debug, Serialize)]
pub struct Identity {
    pub mr_td: String,
    /// The boot chain: VM configuration and ACPI tables, kernel, command line and initrd. mr_td
    /// covers only the firmware, so without these any kernel could replay the pinned events.
    pub rtmr0: String,
    pub rtmr1: String,
    pub rtmr2: String,
    pub os_image_hash: String,
    pub compose_hash: String,
    pub key_provider: String,
}

impl Identity {
    /// Read from a quote and its event log, refusing a log the quote did not measure.
    pub fn from_quote(quote: &str, event_log: &str) -> Result<Self> {
        let quote = dcap_qvl::quote::Quote::parse(&hex::decode(quote.trim_start_matches("0x"))?)
            .map_err(|e| anyhow::anyhow!("quote does not parse: {e:?}"))?;
        let td = quote.report.as_td10().context("not a TDX quote")?;
        let log: Vec<EventLog> = serde_json::from_str(event_log).context("event log")?;
        anyhow::ensure!(
            replay(&log) == [td.rt_mr0, td.rt_mr1, td.rt_mr2, td.rt_mr3],
            "event log does not replay to the quote's RTMRs"
        );
        let field = |name: &str| -> Result<String> {
            value(&log, name).map(hex::encode).with_context(|| {
                format!("event `{name}` missing, duplicated or not self-consistent")
            })
        };
        Ok(Identity {
            mr_td: hex::encode(td.mr_td),
            rtmr0: hex::encode(td.rt_mr0),
            rtmr1: hex::encode(td.rt_mr1),
            rtmr2: hex::encode(td.rt_mr2),
            os_image_hash: field("os-image-hash")?,
            compose_hash: field("compose-hash")?,
            key_provider: field("key-provider")?,
        })
    }

    /// Read a live enclave's `/identity`.
    pub async fn fetch(url: &str) -> Result<Self> {
        let body: serde_json::Value =
            reqwest::get(format!("{}/identity", url.trim_end_matches('/')))
                .await?
                .error_for_status()?
                .json()
                .await?;
        Self::from_quote(
            body["quote"].as_str().context("no quote in /identity")?,
            body["event_log"]
                .as_str()
                .context("no event log in /identity")?,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ET: u32 = 0x0800_0001;

    /// A real `/identity` response from one of our CVMs, which must read as it did under
    /// `circuit-tool identity`.
    #[test]
    fn a_live_enclave_reads_back() {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/identity.json")).unwrap();
        let id = Identity::from_quote(
            v["quote"].as_str().unwrap(),
            v["event_log"].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(id.mr_td.len(), 96);
        assert_eq!(id.compose_hash.len(), 64);
        for rtmr in [&id.rtmr0, &id.rtmr1, &id.rtmr2] {
            assert_eq!(rtmr.len(), 96);
        }
    }

    /// A v5 quote puts a body descriptor between the header and the same report body. Phala's
    /// hosts emit v4 today; this is what a host upgrade to v5 would hand us.
    #[test]
    fn a_v5_quote_reads_the_same_identity() {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/identity.json")).unwrap();
        let v4 = hex::decode(v["quote"].as_str().unwrap().trim_start_matches("0x")).unwrap();
        let mut v5 = vec![5, 0];
        v5.extend_from_slice(&v4[2..48]);
        v5.extend_from_slice(&2u16.to_le_bytes()); // TD 1.0 body
        v5.extend_from_slice(&584u32.to_le_bytes());
        v5.extend_from_slice(&v4[48..]);
        let log = v["event_log"].as_str().unwrap();
        let a = Identity::from_quote(&hex::encode(&v4), log).unwrap();
        let b = Identity::from_quote(&hex::encode(&v5), log).unwrap();
        assert_eq!(
            serde_json::to_value(a).unwrap(),
            serde_json::to_value(b).unwrap()
        );
    }

    #[test]
    fn a_log_the_quote_did_not_measure_is_refused() {
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/identity.json")).unwrap();
        let mut log: Vec<serde_json::Value> =
            serde_json::from_str(v["event_log"].as_str().unwrap()).unwrap();
        log.push(
            serde_json::json!({ "imr": 3, "event_type": ET, "digest": "",
                                     "event": "extra", "event_payload": "00" }),
        );
        let err = Identity::from_quote(
            v["quote"].as_str().unwrap(),
            &serde_json::to_string(&log).unwrap(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("does not replay"), "{err}");
    }
}
