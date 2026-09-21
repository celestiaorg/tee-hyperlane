//! Emit golden vectors for the Go decoder in celestia-app's x/teeism.
//!
//! The enclave encodes these formats in Rust and the chain decodes them in Go, so
//! a single byte of drift rejects every attestation the bridge submits. Running
//! this and checking the output into the Go test suite is what keeps the two
//! implementations honest about each other.

use tee_attestation::*;

fn state(seed: u8) -> IsmState {
    IsmState {
        state_root: [seed; 32],
        origin_domain: 0x4D3C2B1A,
        height: 0x0102030405060708 + seed as u64,
        timestamp: 1_800_000_000 + seed as u64,
        lc_store_commit: [seed.wrapping_add(0x40); 32],
        identity_digest: [seed.wrapping_add(0x80); 32],
    }
}

fn main() {
    let prev = state(1);
    let next = state(2);

    let update = AttestedUpdate {
        prev_state: prev,
        new_state: next,
        merkle_tree_address: [0xAB; 32],
        attested_at: 1_800_000_123,
        message_ids: vec![[0x11; 32], [0x22; 32], [0x33; 32]],
    };

    let empty = AttestedUpdate {
        prev_state: prev,
        new_state: next,
        merkle_tree_address: [0xCD; 32],
        attested_at: 1_800_000_456,
        message_ids: vec![],
    };

    let identity = EnclaveIdentity {
        mr_td: [0x11; 48],
        os_image_hash: vec![0xbb; 32],
        compose_hash: vec![0xaa; 32],
        mr_kms: vec![0xcc; 48],
        key_provider: b"kms-key-provider".to_vec(),
    };
    let policy = IdentityPolicy::Require(identity.clone());

    // An event log whose digests really commit to their own text, so the Go
    // replay and the Go digest check both have something real to chew on.
    let et = tee_attestation::attestation::DSTACK_RUNTIME_EVENT_TYPE;
    let ev = |imr: u32, name: &str, payload: &[u8]| {
        serde_json::json!({
            "imr": imr,
            "event_type": et,
            "digest": hex::encode(tee_attestation::attestation::sha384(
                &tee_attestation::attestation::event_preimage_v1(et, name, payload))),
            "event": name,
            "event_payload": hex::encode(payload),
        })
    };
    let log = serde_json::json!([
        ev(0, "boot", b"x"),
        ev(3, "os-image-hash", &identity.os_image_hash),
        ev(3, "compose-hash", &identity.compose_hash),
        ev(3, "mr-kms", &identity.mr_kms),
        ev(3, "key-provider", &identity.key_provider),
        ev(3, "instance-id", b"instance-a"),
    ]);
    let log_bytes = serde_json::to_vec(&log).unwrap();
    let parsed: Vec<EventLog> = serde_json::from_slice(&log_bytes).unwrap();
    let rtmrs = replay_event_logs(&parsed);

    let out = serde_json::json!({
        "ism_state": {
            "state_root": hex::encode(prev.state_root),
            "origin_domain": prev.origin_domain,
            "height": prev.height,
            "timestamp": prev.timestamp,
            "lc_store_commit": hex::encode(prev.lc_store_commit),
            "identity_digest": hex::encode(prev.identity_digest),
            "encoded": hex::encode(encode_ism_state(&prev)),
        },
        "attested_update": {
            "encoded": hex::encode(encode_attested_update(&update)),
            "hash": hex::encode(hash_attested_update(&update)),
            "merkle_tree_address": hex::encode(update.merkle_tree_address),
            "attested_at": update.attested_at,
            "message_ids": update.message_ids.iter().map(hex::encode).collect::<Vec<_>>(),
        },
        "attested_update_empty": {
            "encoded": hex::encode(encode_attested_update(&empty)),
            "hash": hex::encode(hash_attested_update(&empty)),
        },
        "identity": {
            "mr_td": hex::encode(identity.mr_td),
            "os_image_hash": hex::encode(&identity.os_image_hash),
            "compose_hash": hex::encode(&identity.compose_hash),
            "mr_kms": hex::encode(&identity.mr_kms),
            "key_provider": hex::encode(&identity.key_provider),
            "digest": hex::encode(policy.digest()),
        },
        "event_log": {
            "json": String::from_utf8(log_bytes).unwrap(),
            "rtmrs": rtmrs.iter().map(hex::encode).collect::<Vec<_>>(),
        },
        "max_quote_skew_secs": tee_attestation::attestation::MAX_QUOTE_SKEW_SECS,
        "ism_state_bytes": ISM_STATE_BYTES,
    });

    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}
