//! Gathers what the enclave needs to verify Arbitrum: a feed message its sequencer signed, the
//! header of the block it produced, and tree proofs.
//!
//! The sequencer feed is a websocket the sequencer itself serves. A background task keeps the
//! newest signed messages; each tick attests the newest one whose block the RPC can prove.

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::Engine as _;
use futures::StreamExt;
use serde_json::{json, Value};
use tee_node::chains::l2::arbitrum::{
    signing_hash, FeedMessage, Header, ARBITRUM, SEQUENCER, TREE_SLOT,
};
use tee_node::state::IsmState;
use tracing::{debug, info, warn};

use super::sequenced::{Config, Recent, Sequenced, SignedHead, TRIES};
use crate::origin::{Indexer, Message, Step};

/// One feed connection per process, however many routes start from Arbitrum.
static FEED: OnceLock<Arc<Recent<FeedMessage>>> = OnceLock::new();

pub struct Arbitrum {
    chain: Sequenced,
    url: String,
}

impl Arbitrum {
    pub fn new(config: Config) -> Result<Self> {
        anyhow::ensure!(
            config.domain == ARBITRUM.domain,
            "chain `arbitrum` has domain {}, but the enclave attests Arbitrum as {}",
            config.domain,
            ARBITRUM.domain
        );
        let url = config
            .feed
            .clone()
            .context("an Arbitrum origin needs `feed`, the sequencer feed's websocket")?;
        Ok(Self {
            chain: Sequenced::new(&ARBITRUM, TREE_SLOT, &config),
            url,
        })
    }

    /// The feed's newest messages, connecting on first use: that is inside the runtime, which
    /// building the config is not.
    fn feed(&self) -> &Recent<FeedMessage> {
        FEED.get_or_init(|| listen(self.url.clone()))
    }

    /// The newest signed messages above `height`, each with its block's header.
    async fn heads(&self, height: u64) -> Result<Vec<SignedHead>> {
        Ok(self.heads_or_last_error(height).await?.0)
    }

    /// `heads`, with the RPC's last error for a block it could not serve, so a caller that got
    /// none can say why.
    async fn heads_or_last_error(
        &self,
        height: u64,
    ) -> Result<(Vec<SignedHead>, Option<anyhow::Error>)> {
        let mut heads = Vec::new();
        let mut last_error = None;
        for message in self.feed().above(0, TRIES) {
            let (rlp, _) = match self.chain.rpc().header_rlp(message.block_hash).await {
                Ok(h) => h,
                Err(e) => {
                    debug!(block = %message.block_hash, error = %e, "the RPC does not have this block yet");
                    last_error = Some(e.context(format!("block {}", message.block_hash)));
                    continue;
                }
            };
            let header = Header::decode(&rlp)?;
            if header.number <= height {
                break;
            }
            heads.push(SignedHead {
                height: header.number,
                state_root: header.state_root,
                timestamp: header.timestamp,
                input: json!({ "message": message, "header_rlp": format!("0x{}", hex::encode(rlp)) }),
            });
        }
        Ok((heads, last_error))
    }
}

#[async_trait]
impl Indexer for Arbitrum {
    async fn gather(&self, trusted: &IsmState) -> Result<Step> {
        self.feed().check_alive("arbitrum sequencer feed")?;
        let heads = self.heads(trusted.height).await?;
        self.chain.step(trusted, &heads).await
    }

    async fn index(&self, from: u64, to: u64) -> Result<Vec<Message>> {
        self.chain.index(from, to).await
    }

    async fn bootstrap(&self, identity: [u8; 32], _height: Option<u64>) -> Result<IsmState> {
        self.feed()
            .first(Duration::from_secs(60))
            .await
            .context("the Arbitrum sequencer feed (`feed`) sent nothing")?;
        // The feed runs ahead of every RPC, so its first blocks are often not served yet: retry
        // until the RPC catches up, and say what it answered if it never does.
        let until = Instant::now() + Duration::from_secs(60);
        loop {
            let (heads, error) = self.heads_or_last_error(0).await?;
            if let Some(head) = heads.into_iter().next() {
                return Ok(self.chain.genesis(&head, identity));
            }
            if Instant::now() >= until {
                let why = error.map_or("no feed message to look up".into(), |e| format!("{e:#}"));
                anyhow::bail!(
                    "the Arbitrum RPC (`rpc` of [chains.arbitrum]) served none of the feed's newest \
                     blocks within 60s; its last answer: {why}"
                );
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
}

/// Keep the feed's newest signed messages, reconnecting whenever it drops. Each connection runs
/// in its own task, so a panic in it is logged and retried rather than ending the feed.
fn listen(url: String) -> Arc<Recent<FeedMessage>> {
    crate::install_tls_provider();
    let recent = Arc::new(Recent::default());
    let out = recent.clone();
    tokio::spawn(async move {
        loop {
            let (url, out) = (url.clone(), out.clone());
            match tokio::spawn(async move { read_feed(&url, &out).await }).await {
                Ok(Err(e)) => {
                    warn!(error = %format!("{e:#}"), "arbitrum feed dropped; reconnecting")
                }
                Err(e) => tracing::error!(error = %e, "arbitrum feed task panicked; reconnecting"),
                Ok(Ok(())) => {}
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
    recent
}

/// The feed sends messages every few hundred milliseconds, and pings between them. A socket
/// quiet for this long is dead, whatever TCP says: Cloudflare once dropped the stream without
/// a close, and the reader waited on it for fifteen hours.
const FEED_SILENCE: Duration = Duration::from_secs(60);
/// A connect that has not completed by then never will.
const FEED_CONNECT: Duration = Duration::from_secs(30);
/// How often failed messages are summarised at warn: a feed format change and a dead feed must
/// not look alike in the log.
const PARSE_WARN_EVERY: Duration = Duration::from_secs(60);

async fn read_feed(url: &str, recent: &Recent<FeedMessage>) -> Result<()> {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = url.into_client_request()?;
    // Version 2 is the one that carries `signatureV2` and the block hash.
    request
        .headers_mut()
        .insert("Arbitrum-Feed-Client-Version", "2".parse()?);
    let (socket, _) = tokio::time::timeout(FEED_CONNECT, tokio_tungstenite::connect_async(request))
        .await
        .map_err(|_| anyhow::anyhow!("no connection within {}s", FEED_CONNECT.as_secs()))?
        .context("connecting to the arbitrum feed")?;
    info!("reading the arbitrum sequencer feed");
    read_frames(socket, recent, FEED_SILENCE).await
}

/// Read until the feed closes or goes quiet for `silence`; either way the caller reconnects.
async fn read_frames<S>(
    mut socket: S,
    recent: &Recent<FeedMessage>,
    silence: Duration,
) -> Result<()>
where
    S: futures::Stream<
            Item = Result<
                tokio_tungstenite::tungstenite::Message,
                tokio_tungstenite::tungstenite::Error,
            >,
        > + Unpin,
{
    let (mut failed, mut last_error, mut warned) = (0u64, String::new(), Instant::now());
    loop {
        let frame = match tokio::time::timeout(silence, socket.next()).await {
            Ok(Some(frame)) => frame?,
            Ok(None) => anyhow::bail!("the feed closed"),
            Err(_) => anyhow::bail!("nothing from the feed in {}s", silence.as_secs()),
        };
        // Pings arrive as non-text frames; tungstenite answers them on the next read.
        if !frame.is_text() {
            continue;
        }
        let batch: Value = serde_json::from_str(frame.to_text()?)?;
        for raw in batch["messages"].as_array().into_iter().flatten() {
            match feed_message(raw) {
                Ok(m) => recent.insert(m.sequence_number, m),
                Err(e) => {
                    failed += 1;
                    last_error = format!("{e:#}");
                }
            }
        }
        if failed > 0 && warned.elapsed() >= PARSE_WARN_EVERY {
            warn!(
                failed,
                last_error, "arbitrum feed messages that did not parse in the last minute"
            );
            (failed, warned) = (0, Instant::now());
        }
    }
}

/// A feed message as the enclave's input, if the pinned sequencer signed it.
fn feed_message(raw: &Value) -> Result<FeedMessage> {
    let b64 = |v: &Value| -> Result<alloy_primitives::Bytes> {
        Ok(match v.as_str() {
            Some(s) => base64::engine::general_purpose::STANDARD.decode(s)?.into(),
            None => Default::default(),
        })
    };
    let outer = &raw["message"];
    let header = &outer["message"]["header"];
    let message = FeedMessage {
        sequence_number: raw["sequenceNumber"].as_u64().context("sequenceNumber")?,
        block_hash: raw["blockHash"].as_str().context("no blockHash")?.parse()?,
        block_metadata: b64(&raw["blockMetadata"])?,
        delayed_messages_read: outer["delayedMessagesRead"]
            .as_u64()
            .context("delayedMessagesRead")?,
        kind: header["kind"].as_u64().context("kind")? as u8,
        sender: header["sender"].as_str().context("sender")?.parse()?,
        l1_block_number: header["blockNumber"].as_u64().context("blockNumber")?,
        timestamp: header["timestamp"].as_u64().context("timestamp")?,
        request_id: header["requestId"].as_str().map(str::parse).transpose()?,
        l1_base_fee: match &header["baseFeeL1"] {
            Value::Null => None,
            v => Some(v.to_string().trim_matches('"').parse()?),
        },
        l2_msg: b64(&outer["message"]["l2Msg"])?,
        signature: b64(&raw["signatureV2"])?,
    };
    tee_node::chains::l2::sequencer::verify(signing_hash(&message), &message.signature, SEQUENCER)?;
    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A message exactly as the feed sends it, checked against the sequencer the enclave pins.
    #[test]
    fn a_raw_feed_message_is_read_and_verified() {
        let raw: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/arbitrum_feed_raw.json"
        ))
        .unwrap();
        let m = feed_message(&raw).unwrap();
        assert_eq!(m.sequence_number, raw["sequenceNumber"].as_u64().unwrap());
        let mut tampered = raw.clone();
        tampered["message"]["delayedMessagesRead"] =
            json!(raw["message"]["delayedMessagesRead"].as_u64().unwrap() + 1);
        assert!(feed_message(&tampered).is_err());
    }

    #[tokio::test]
    async fn a_silent_feed_is_dropped() {
        let silent = futures::stream::pending();
        let e = read_frames(silent, &Recent::default(), Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("nothing from the feed"), "{e}");
    }

    #[tokio::test]
    async fn a_closed_feed_is_dropped() {
        let closed = futures::stream::empty();
        let e = read_frames(closed, &Recent::default(), FEED_SILENCE)
            .await
            .unwrap_err();
        assert!(e.to_string().contains("the feed closed"), "{e}");
    }

    #[tokio::test]
    async fn pings_then_silence_is_dropped() {
        use tokio_tungstenite::tungstenite::Message;
        let pings = futures::stream::iter(vec![
            Ok(Message::Ping(vec![1].into())),
            Ok(Message::Ping(vec![2].into())),
        ])
        .chain(futures::stream::pending());
        let e = read_frames(pings, &Recent::default(), Duration::from_millis(50))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("nothing from the feed"), "{e}");
    }
}
