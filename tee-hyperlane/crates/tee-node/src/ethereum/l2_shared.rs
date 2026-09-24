//! What Arbitrum and Base share: both verify Ethereum first, then read their own root out of
//! the L1 state root it returns, and both finish at an L2 block header.

use alloy_primitives::B256;
use serde_json::Value;

use crate::origin::Head;

/// An L2 step: Ethereum's step, untouched, and the proof that reads the L2's root out of L1.
#[derive(serde::Deserialize)]
pub struct Input<P> {
    pub ethereum: Value,
    pub proof: P,
}

/// An L2 head. The ISM's commitment is Ethereum's, since Ethereum's light client is the thing
/// with state worth remembering, and the L1 head dates the attestation: the L2's confirmed
/// head is older by its challenge window, which is how rollups work, not a sign of staleness.
pub fn l2_head(l1: Head, l2: Header) -> Head {
    Head {
        root: l2.state_root,
        height: l2.number,
        timestamp: l2.timestamp,
        store_commit: l1.store_commit,
        attested_at: l1.timestamp,
    }
}

/// The three header fields an L2 root needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub state_root: B256,
    pub number: u64,
    pub timestamp: u64,
}

impl Header {
    /// A block header is an RLP list: state root is item 3, number item 8, timestamp item 11.
    pub fn decode(rlp: &[u8]) -> anyhow::Result<Self> {
        let malformed = || anyhow::anyhow!("L2 block header RLP is malformed");
        let mut slice = rlp;
        let alloy_rlp::PayloadView::List(items) =
            alloy_rlp::Header::decode_raw(&mut slice).map_err(|_| malformed())?
        else {
            return Err(malformed());
        };
        anyhow::ensure!(items.len() >= 12, malformed());
        let field = |i: usize| {
            let mut item = items[i];
            alloy_rlp::Header::decode_bytes(&mut item, false).map_err(|_| malformed())
        };
        let uint = |i: usize| -> anyhow::Result<u64> {
            let bytes = field(i)?;
            anyhow::ensure!(bytes.len() <= 8, malformed());
            let mut out = [0u8; 8];
            out[8 - bytes.len()..].copy_from_slice(bytes);
            Ok(u64::from_be_bytes(out))
        };
        let root = field(3)?;
        anyhow::ensure!(root.len() == 32, malformed());
        Ok(Self {
            state_root: B256::from_slice(root),
            number: uint(8)?,
            timestamp: uint(11)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Header;
    use alloy_primitives::{keccak256, B256};

    #[derive(serde::Deserialize)]
    struct Fixture {
        rlp: String,
        hash: String,
        #[serde(rename = "stateRoot")]
        state_root: String,
        number: u64,
        timestamp: u64,
    }

    /// Checked against a real Arbitrum Sepolia block: the RLP must hash to the block hash,
    /// so a wrong field set or order fails here.
    #[test]
    fn a_live_l2_header_decodes_at_the_expected_positions() {
        let f: Fixture =
            serde_json::from_str(include_str!("../../testdata/arbitrum_header.json")).unwrap();
        let rlp = hex::decode(f.rlp.trim_start_matches("0x")).unwrap();
        assert_eq!(keccak256(&rlp), f.hash.parse::<B256>().unwrap());
        let header = Header::decode(&rlp).unwrap();
        assert_eq!(header.state_root, f.state_root.parse::<B256>().unwrap());
        assert_eq!((header.number, header.timestamp), (f.number, f.timestamp));
        assert!(Header::decode(&rlp[..rlp.len() / 2]).is_err());
        assert!(Header::decode(b"").is_err());
    }
}
