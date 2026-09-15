//! A route proves for its own transfers, not for everything aimed at its chain.
//!
//! The destination domain alone is too coarse. An origin's tree is shared by every outbound
//! message, and on a canonical mailbox most of that traffic belongs to other bridges: one
//! Celestia transfer to Sepolia woke all three Celestia routes, and any stranger's
//! Celestia-bound message on Sepolia started ours. Matching the recipient against the routers
//! we actually serve is what narrows it.
//!
//! The property these tests protect is that narrowing the *trigger* never narrows the
//! *batch*. A message that starts no proof is still carried and authorised by the next proof
//! that runs, and the twelve-hour heartbeat bounds that wait. Gating costs latency, never
//! delivery.

use hyperlane_types::{encode_hyperlane_message, HyperlaneMessage};
use tee_coprocessor::commands::any_for_route;

const CELESTIA: u32 = 1297040200;
const SEPOLIA: u32 = 11155111;
const BASE: u32 = 84532;

/// Our TIA router on Sepolia, as the bridge deploys it.
const OUR_SEPOLIA_ROUTER: &str = "0xFeA14C1444A7a8beAb7122fdE5A168212D7185bE";
const SOMEONE_ELSES: &str = "0x00000000000000000000000000000000DeaDBeef";

fn padded(addr: &str) -> [u8; 32] {
    let raw = hex::decode(addr.trim_start_matches("0x")).unwrap();
    let mut out = [0u8; 32];
    out[32 - raw.len()..].copy_from_slice(&raw);
    out
}

fn message(destination: u32, recipient: &str) -> Vec<u8> {
    encode_hyperlane_message(&HyperlaneMessage {
        version: 3,
        nonce: 7,
        origin: CELESTIA,
        sender: [1u8; 32],
        destination,
        recipient: padded(recipient),
        body: vec![9u8; 64],
    })
}

#[test]
fn our_own_transfer_starts_a_proof() {
    let batch = vec![message(SEPOLIA, OUR_SEPOLIA_ROUTER)];
    assert!(any_for_route(&batch, SEPOLIA, &[OUR_SEPOLIA_ROUTER.to_string()]));
}

#[test]
fn a_strangers_transfer_to_our_chain_does_not() {
    // The case that made Sepolia's canonical mailbox expensive: right domain, not our router.
    let batch = vec![message(SEPOLIA, SOMEONE_ELSES)];
    assert!(!any_for_route(&batch, SEPOLIA, &[OUR_SEPOLIA_ROUTER.to_string()]));
}

#[test]
fn a_transfer_to_another_chain_does_not() {
    let batch = vec![message(BASE, OUR_SEPOLIA_ROUTER)];
    assert!(!any_for_route(&batch, SEPOLIA, &[OUR_SEPOLIA_ROUTER.to_string()]));
}

#[test]
fn one_of_ours_among_many_is_enough() {
    let batch = vec![
        message(SEPOLIA, SOMEONE_ELSES),
        message(BASE, OUR_SEPOLIA_ROUTER),
        message(SEPOLIA, OUR_SEPOLIA_ROUTER),
    ];
    assert!(any_for_route(&batch, SEPOLIA, &[OUR_SEPOLIA_ROUTER.to_string()]));
}

#[test]
fn a_route_told_nothing_falls_back_to_the_domain() {
    // Never conclude "none of this is mine" from an absent configuration. Without routers the
    // old, looser behaviour stands: wasteful, but it cannot skip our own message.
    let batch = vec![message(SEPOLIA, SOMEONE_ELSES)];
    assert!(any_for_route(&batch, SEPOLIA, &[]));
}

#[test]
fn an_unparseable_router_falls_back_rather_than_excluding_everything() {
    // A typo in config must not silently stop a route proving.
    let batch = vec![message(SEPOLIA, SOMEONE_ELSES)];
    assert!(any_for_route(&batch, SEPOLIA, &["not-an-address".to_string()]));
}

#[test]
fn a_router_given_as_20_bytes_matches_the_padded_recipient() {
    // Hyperlane addresses recipients as 32 bytes; config carries EVM addresses as 20.
    let batch = vec![message(SEPOLIA, OUR_SEPOLIA_ROUTER)];
    assert!(any_for_route(&batch, SEPOLIA, &[OUR_SEPOLIA_ROUTER.to_string()]));
    let full = format!("0x{}", hex::encode(padded(OUR_SEPOLIA_ROUTER)));
    assert!(any_for_route(&batch, SEPOLIA, &[full]));
}
