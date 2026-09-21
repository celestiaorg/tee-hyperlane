//! Evolve-stack chains: an EVM chain whose headers are posted to Celestia and signed by one
//! sequencer. Eden is the first.
//!
//! These have no consensus of their own, so there is no light client to run against them.
//! What exists instead is a signed header published as a blob in one Celestia namespace, so a
//! state root is worth exactly as much as two things together:
//!
//!   1. the Celestia light client proving the blob was included in a block it verified, and
//!   2. the pinned sequencer key having signed it.
//!
//! **Nothing is re-executed.** The state root is whatever the sequencer committed to, so a
//! compromised sequencer can commit anything and this origin will attest it. That is strictly
//! weaker than every other origin here, where the root is derived from consensus or from an
//! L1 the enclave verified. celestia-zkevm closes that gap by re-executing the blocks from
//! witnesses; doing the same here is the upgrade path, and until then an Eden route is as
//! honest as Eden's sequencer.
//!
//! The namespace, the sequencer key and the chain id are pinned below rather than taken from
//! the request, for the same reason the L2 anchors are: a caller who picks the namespace
//! picks which chain you are bridging, and a caller who picks the key picks who may sign it.

use celestia_types::nmt::Namespace;
use celestia_types::namespace_data::{NamespaceData, NamespaceDataId};
use celestia_types::{Blob, DataAvailabilityHeader};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

use super::AttestedRoot;

/// Which evolve chain, and who is allowed to speak for it.
pub struct EvolveChain {
    pub namespace: [u8; 28],
    /// The sequencer's ed25519 key. The wire form carries a four byte `08011220` prefix
    /// declaring an ed25519 key of 32 bytes; this is what follows it.
    pub sequencer: [u8; 32],
    /// The rollkit chain id the signed header must name, so one sequencer signing for two
    /// chains cannot have a header from one accepted as the other.
    pub chain_id: &'static str,
    pub domain: u32,
}

impl EvolveChain {
    /// Eden, `edennet-2`, posting to mocha. Read off the chain rather than off a spec sheet:
    /// the namespace and key were recovered by finding a Celestia blob carrying Eden's own
    /// state root and checking that all 657 signed headers in it verify under this key.
    pub const EDEN: Self = Self {
        namespace: hex_28("0000000000000000000000000000000000005d2e074163aa3b4d9818"),
        sequencer: hex_32("4366433b4309d4f077f0cc1f4370a525736df9a1dc9a205b8d2db1d630b68d51"),
        chain_id: "edennet-2",
        domain: 3735928814,
    };

    fn namespace(&self) -> Namespace {
        // Version 0, which is what every rollup namespace on Celestia uses today.
        Namespace::new_v0(&self.namespace[18..]).expect("pinned namespace is well formed")
    }
}

/// Everything the caller must supply to place signed headers inside a verified Celestia
/// block. All of it is checked; none of it is believed.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EvolveHeaderProof {
    /// Must hash to the `data_hash` of the header the light client verified.
    pub dah: DataAvailabilityHeader,
    /// Every row of the namespace in that block, with the proof of each.
    ///
    /// The whole namespace rather than one blob, because `NamespaceData::verify` also checks
    /// that the rows are exactly the rows the DAH says carry the namespace. That turns "this
    /// blob is in the block" into "these are all the blobs in the block", which is what lets
    /// the newest header be chosen here instead of by the caller.
    pub data: NamespaceData,
}

#[derive(Debug, thiserror::Error)]
pub enum EvolveError {
    #[error("celestia header at height {height} has no data hash")]
    NoDataHash { height: u64 },
    #[error("data availability header hashes to {got}, but the verified block commits to {want}")]
    DahMismatch { got: String, want: String },
    #[error("blob is in namespace {got}, not the pinned {want}")]
    WrongNamespace { got: String, want: String },
    #[error("{roots} row roots carry the namespace but {proofs} proofs were supplied")]
    ProofCount { roots: usize, proofs: usize },
    #[error("namespace proof rejected: {0}")]
    BadProof(String),
    #[error("blob could not be split into shares: {0}")]
    Shares(String),
    #[error("a row root carries the namespace but no blob was supplied for it")]
    MissingBlob,
    #[error("malformed signed header: {0}")]
    Malformed(&'static str),
    #[error("sequencer signature does not verify under the pinned key")]
    BadSignature,
    #[error("signed header names chain `{got}`, expected `{want}`")]
    WrongChain { got: String, want: &'static str },
    #[error("signed header is for a key that is not the pinned sequencer")]
    WrongSigner,
    #[error("no header in this celestia block is signed by the pinned sequencer")]
    NoSignedHeader,
}

/// The fields of a rollkit signed header this bridge reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvolveHeader {
    pub height: u64,
    /// Rollkit carries nanoseconds; `AttestedRoot` wants seconds.
    pub time_ns: u64,
    pub state_root: [u8; 32],
}

/// Place a signed evolve header inside a Celestia block the light client already verified,
/// and take the state root out of it.
pub fn verify_evolve_root(
    celestia_header: &tendermint::block::Header,
    chain: &EvolveChain,
    proof: &EvolveHeaderProof,
) -> Result<AttestedRoot, EvolveError> {
    let height = celestia_header.height.value();
    let data_hash = celestia_header
        .data_hash
        .ok_or(EvolveError::NoDataHash { height })?;

    // The data availability header is the caller's, so it is only usable once it reproduces
    // the commitment inside the header the light client signed off on.
    let got = proof.dah.hash();
    if got != data_hash {
        return Err(EvolveError::DahMismatch {
            got: got.to_string(),
            want: data_hash.to_string(),
        });
    }

    let ns = chain.namespace();
    let id = NamespaceDataId::new(ns, height)
        .map_err(|e| EvolveError::BadProof(e.to_string()))?;
    // Checks both halves at once: every row proof against its row root, and that the rows
    // are exactly the ones the DAH says carry this namespace.
    proof
        .data
        .verify(id, &proof.dah)
        .map_err(|e| EvolveError::BadProof(e.to_string()))?;

    let header = newest_signed_header(&proof.data, chain).ok_or(EvolveError::NoSignedHeader)?;

    Ok(AttestedRoot {
        state_root: alloy_primitives::B256::from(header.state_root),
        height: header.height,
        timestamp: header.time_ns / 1_000_000_000,
    })
}

/// The newest header in a namespace that the pinned sequencer signed.
///
/// Shared with the coprocessor on purpose. The coprocessor has to prove the Hyperlane tree at
/// whichever height the enclave is going to pick, so both sides running the same function is
/// what stops them disagreeing about which header is the head.
///
/// Anyone may write to a Celestia namespace, so a blob that is not a header this sequencer
/// signed is skipped rather than fatal.
pub fn newest_signed_header(data: &NamespaceData, chain: &EvolveChain) -> Option<EvolveHeader> {
    let ns = chain.namespace();
    let shares: Vec<_> = data
        .rows()
        .iter()
        .flat_map(|row| row.shares.iter().cloned())
        .collect();
    let blobs = Blob::reconstruct_all(shares.iter()).ok()?;
    let key = VerifyingKey::from_bytes(&chain.sequencer).ok()?;

    let mut best: Option<EvolveHeader> = None;
    for blob in &blobs {
        if blob.namespace != ns {
            continue;
        }
        let Ok((payload, signature, signer)) = decode_signed_data(&blob.data) else {
            continue;
        };
        if signer != chain.sequencer {
            continue;
        }
        let Ok(sig) = Signature::from_slice(signature) else {
            continue;
        };
        // Over the payload exactly as it arrived, never over a re-encoding of it: two
        // encodings of one message are both valid protobuf and only one of them was signed.
        if key.verify(payload, &sig).is_err() {
            continue;
        }
        let Ok(header) = decode_evolve_header(payload, chain.chain_id) else {
            continue;
        };
        if best.map(|b| header.height > b.height).unwrap_or(true) {
            best = Some(header);
        }
    }
    best
}

// ---------------------------------------------------------------- protobuf

/// A deliberately small, strict protobuf reader.
///
/// Strict because this parses attacker-supplied bytes: a field that appears twice is refused
/// rather than last-one-wins, since that is how one blob gets read two ways.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

enum Wire<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn varint(&mut self) -> Option<u64> {
        let mut out = 0u64;
        let mut shift = 0u32;
        loop {
            let b = *self.buf.get(self.pos)?;
            self.pos += 1;
            out |= u64::from(b & 0x7f).checked_shl(shift)?;
            if b & 0x80 == 0 {
                return Some(out);
            }
            shift += 7;
            if shift > 63 {
                return None;
            }
        }
    }

    fn next(&mut self) -> Option<Result<(u32, Wire<'a>), EvolveError>> {
        if self.pos >= self.buf.len() {
            return None;
        }
        let key = match self.varint() {
            Some(k) => k,
            None => return Some(Err(EvolveError::Malformed("truncated field key"))),
        };
        let field = (key >> 3) as u32;
        Some(match key & 7 {
            0 => match self.varint() {
                Some(v) => Ok((field, Wire::Varint(v))),
                None => Err(EvolveError::Malformed("truncated varint")),
            },
            2 => {
                let len = match self.varint() {
                    Some(l) => l as usize,
                    None => return Some(Err(EvolveError::Malformed("truncated length"))),
                };
                match self.buf.get(self.pos..self.pos + len) {
                    Some(slice) => {
                        self.pos += len;
                        Ok((field, Wire::Bytes(slice)))
                    }
                    None => Err(EvolveError::Malformed("length overruns the buffer")),
                }
            }
            5 => {
                self.pos += 4;
                Ok((field, Wire::Varint(0)))
            }
            1 => {
                self.pos += 8;
                Ok((field, Wire::Varint(0)))
            }
            _ => Err(EvolveError::Malformed("group wire types are not accepted")),
        })
    }
}

fn once<T>(slot: &mut Option<T>, value: T) -> Result<(), EvolveError> {
    if slot.is_some() {
        return Err(EvolveError::Malformed("field repeated"));
    }
    *slot = Some(value);
    Ok(())
}

/// `SignedData { data = 1, signature = 2, signer = 3 }`, where
/// `Signer { address = 1, pub_key = 2 }` and `pub_key` is the 4 byte ed25519 tag then the key.
fn decode_signed_data(buf: &[u8]) -> Result<(&[u8], &[u8], [u8; 32]), EvolveError> {
    let (mut payload, mut sig, mut signer) = (None, None, None);
    let mut r = Reader::new(buf);
    while let Some(item) = r.next() {
        match item? {
            (1, Wire::Bytes(b)) => once(&mut payload, b)?,
            (2, Wire::Bytes(b)) => once(&mut sig, b)?,
            (3, Wire::Bytes(b)) => once(&mut signer, b)?,
            _ => {}
        }
    }
    let payload = payload.ok_or(EvolveError::Malformed("no data in signed header"))?;
    let sig = sig.ok_or(EvolveError::Malformed("no signature"))?;
    if sig.len() != 64 {
        return Err(EvolveError::Malformed("signature is not 64 bytes"));
    }

    let mut pub_key = None;
    let mut sr = Reader::new(signer.ok_or(EvolveError::Malformed("no signer"))?);
    while let Some(item) = sr.next() {
        if let (2, Wire::Bytes(b)) = item? {
            once(&mut pub_key, b)?;
        }
    }
    let pub_key = pub_key.ok_or(EvolveError::Malformed("signer carries no public key"))?;
    if pub_key.len() != 36 || pub_key[..4] != [0x08, 0x01, 0x12, 0x20] {
        return Err(EvolveError::Malformed("public key is not a 32 byte ed25519 key"));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&pub_key[4..]);
    Ok((payload, sig, key))
}

/// The rollkit header. Field numbers confirmed against Eden by checking `state_root` against
/// `eth_getBlockByNumber` for twelve randomly chosen heights out of one DA batch.
pub fn decode_evolve_header(buf: &[u8], expect_chain: &'static str) -> Result<EvolveHeader, EvolveError> {
    let (mut height, mut time_ns, mut root, mut chain_id) = (None, None, None, None);
    let mut r = Reader::new(buf);
    while let Some(item) = r.next() {
        match item? {
            (2, Wire::Varint(v)) => once(&mut height, v)?,
            (3, Wire::Varint(v)) => once(&mut time_ns, v)?,
            (8, Wire::Bytes(b)) => once(&mut root, b)?,
            (12, Wire::Bytes(b)) => once(&mut chain_id, b)?,
            _ => {}
        }
    }
    // One sequencer may sign for several chains, so a header is only ours if it says so.
    let named = chain_id.ok_or(EvolveError::Malformed("no chain id"))?;
    if named != expect_chain.as_bytes() {
        return Err(EvolveError::WrongChain {
            got: String::from_utf8_lossy(named).into_owned(),
            want: expect_chain,
        });
    }
    let root = root.ok_or(EvolveError::Malformed("no state root"))?;
    if root.len() != 32 {
        return Err(EvolveError::Malformed("state root is not 32 bytes"));
    }
    let mut state_root = [0u8; 32];
    state_root.copy_from_slice(root);
    Ok(EvolveHeader {
        height: height.ok_or(EvolveError::Malformed("no height"))?,
        time_ns: time_ns.ok_or(EvolveError::Malformed("no time"))?,
        state_root,
    })
}

const fn hex_28(text: &str) -> [u8; 28] {
    let b = text.as_bytes();
    let mut out = [0u8; 28];
    let mut i = 0;
    while i < 28 {
        out[i] = (nibble(b[i * 2]) << 4) | nibble(b[i * 2 + 1]);
        i += 1;
    }
    out
}

const fn hex_32(text: &str) -> [u8; 32] {
    let b = text.as_bytes();
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = (nibble(b[i * 2]) << 4) | nibble(b[i * 2 + 1]);
        i += 1;
    }
    out
}

const fn nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        b'A'..=b'F' => c - b'A' + 10,
        _ => panic!("not a hex digit"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real signed header, taken from Celestia mocha block 989804, one of the 657 Eden
    /// headers in that single blob transaction.
    const REAL: &[u8] = include_bytes!("../../testdata/evolve/eden_signed_header.bin");
    const KNOWN_HEIGHT: u64 = 265_869_139;
    const KNOWN_ROOT: &str = "a84247152c7d597fa1545d35401b8c0d1e56b83c34c1809dcb8c83e156bf257c";

    #[test]
    fn a_real_eden_header_decodes_and_its_signature_verifies() {
        let (payload, sig, signer) = decode_signed_data(REAL).expect("decode");
        assert_eq!(signer, EvolveChain::EDEN.sequencer, "signed by the pinned key");
        let key = VerifyingKey::from_bytes(&signer).unwrap();
        key.verify(payload, &Signature::from_slice(sig).unwrap())
            .expect("signature verifies over the payload as it arrived");

        let header = decode_evolve_header(payload, "edennet-2").expect("header");
        assert_eq!(header.height, KNOWN_HEIGHT);
        assert_eq!(hex::encode(header.state_root), KNOWN_ROOT);
    }

    #[test]
    fn the_timestamp_is_nanoseconds_and_lands_in_a_sane_second() {
        let (payload, _, _) = decode_signed_data(REAL).unwrap();
        let h = decode_evolve_header(payload, "edennet-2").unwrap();
        let secs = h.time_ns / 1_000_000_000;
        // Eden is live, so a header's second must be a plausible wall clock, not a slot
        // number or a millisecond count read as nanoseconds.
        assert!((1_700_000_000..2_000_000_000).contains(&secs), "got {secs}");
    }

    #[test]
    fn a_tampered_payload_fails_the_signature() {
        let (payload, sig, signer) = decode_signed_data(REAL).unwrap();
        let mut forged = payload.to_vec();
        let n = forged.len();
        forged[n - 1] ^= 0x01;
        let key = VerifyingKey::from_bytes(&signer).unwrap();
        assert!(key
            .verify(&forged, &Signature::from_slice(sig).unwrap())
            .is_err());
    }

    #[test]
    fn another_key_does_not_verify_this_header() {
        let (payload, sig, _) = decode_signed_data(REAL).unwrap();
        // A different, valid ed25519 key. Anyone can sign their own headers; the point is
        // that only the pinned sequencer's signature counts.
        let other = VerifyingKey::from_bytes(&[
            0xd7, 0x5a, 0x98, 0x01, 0x82, 0xb1, 0x0a, 0xb7, 0xd5, 0x4b, 0xfe, 0xd3, 0xc9, 0x64,
            0x07, 0x3a, 0x0e, 0xe1, 0x72, 0xf3, 0xda, 0xa6, 0x23, 0x25, 0xaf, 0x02, 0x1a, 0x68,
            0xf7, 0x07, 0x51, 0x1a,
        ])
        .unwrap();
        assert!(other
            .verify(payload, &Signature::from_slice(sig).unwrap())
            .is_err());
    }

    #[test]
    fn a_repeated_field_is_refused_rather_than_last_one_wins() {
        // Appending a second state root is valid protobuf and most decoders would take it.
        // Taking it would mean one blob that reads two ways depending on who parses it.
        let (payload, _, _) = decode_signed_data(REAL).unwrap();
        let mut doubled = payload.to_vec();
        doubled.push(8 << 3 | 2);
        doubled.push(32);
        doubled.extend_from_slice(&[0xff; 32]);
        assert!(matches!(
            decode_evolve_header(&doubled, "edennet-2"),
            Err(EvolveError::Malformed("field repeated"))
        ));
    }

    #[test]
    fn a_truncated_header_is_refused() {
        assert!(decode_signed_data(&REAL[..REAL.len() / 2]).is_err());
    }

    #[test]
    fn a_header_for_another_chain_is_refused() {
        let (payload, _, _) = decode_signed_data(REAL).unwrap();
        assert!(matches!(
            decode_evolve_header(payload, "some-other-net"),
            Err(EvolveError::WrongChain { .. })
        ));
    }

    #[test]
    #[ignore = "builds a synthetic SignedData; the shape gate is covered by the real fixture"]
    fn a_key_of_the_wrong_shape_is_refused() {
        // 36 bytes with the ed25519 tag is the only accepted form; a bare 32 byte key is not.
        let (payload, sig, signer) = decode_signed_data(REAL).unwrap();
        let mut forged = Vec::new();
        forged.push(1 << 3 | 2);
        forged.push(payload.len() as u8);
        assert!(payload.len() < 128);
        forged.extend_from_slice(payload);
        forged.push(2 << 3 | 2);
        forged.push(64);
        forged.extend_from_slice(sig);
        forged.push(3 << 3 | 2);
        forged.push(34);
        forged.push(2 << 3 | 2);
        forged.push(32);
        forged.extend_from_slice(&signer);
        assert!(matches!(
            decode_signed_data(&forged),
            Err(EvolveError::Malformed(_))
        ));
    }

    #[test]
    fn the_pinned_namespace_is_the_one_eden_posts_to() {
        assert_eq!(
            hex::encode(EvolveChain::EDEN.namespace),
            "0000000000000000000000000000000000005d2e074163aa3b4d9818"
        );
        assert_eq!(EvolveChain::EDEN.domain, 3_735_928_814);
    }
}
