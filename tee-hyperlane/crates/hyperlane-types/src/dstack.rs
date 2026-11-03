//! dstack's TDX event log: replaying it into the RTMRs a quote carries, and reading the named
//! application values (compose hash, OS image, key provider) out of it by the rules `x/teeism`'s
//! `GetEventValue` pins with. Shared by the coprocessor, which reads an enclave's identity off
//! it, and by every ISM implemented in Rust, which pins that identity.

use serde::Deserialize;
use sha2::{Digest, Sha384};

/// dstack records the application's identity in RTMR 3.
pub const APPLICATION_IMR: u32 = 3;

/// One entry of dstack's event log, as the enclave reports it.
#[derive(Clone, Debug, Deserialize)]
pub struct EventLog {
    pub imr: u32,
    pub event_type: u32,
    #[serde(deserialize_with = "digest")]
    pub digest: [u8; 48],
    pub event: String,
    #[serde(deserialize_with = "bytes")]
    pub event_payload: Vec<u8>,
}

/// Fold the log into RTMR 0..3. IMR 3 entries without a digest have it recomputed, as dstack's
/// own replay does; entries in IMR 0..2 without one contribute nothing.
pub fn replay(log: &[EventLog]) -> [[u8; 48]; 4] {
    let mut rtmrs = [[0u8; 48]; 4];
    for (i, mr) in rtmrs.iter_mut().enumerate() {
        for e in log.iter().filter(|e| e.imr as usize == i) {
            let d = if e.digest != [0u8; 48] {
                e.digest
            } else if e.imr == APPLICATION_IMR {
                sha384(&preimage_v1(e))
            } else {
                continue;
            };
            *mr = Sha384::new()
                .chain_update(*mr)
                .chain_update(d)
                .finalize()
                .into();
        }
    }
    rtmrs
}

/// A named value, by the rules `x/teeism`'s `GetEventValue` pins with:
/// - only from IMR 3, the register the replay binds it into;
/// - exactly one entry carries the name;
/// - every IMR 3 entry with a digest commits to its own text, so a measured entry cannot be
///   relabelled out of the way of a forged one;
/// - no IMR 3 entry is named `name:…`: the v1 preimage does not mark where a name ends, so such
///   an entry hashes the same as `name` with a `:` in its payload.
pub fn value<'a>(log: &'a [EventLog], name: &str) -> Option<&'a [u8]> {
    let resplit = format!("{name}:");
    let mut found = None;
    for e in log {
        if e.imr == APPLICATION_IMR && e.digest != [0u8; 48] && !commits(e) {
            return None;
        }
        if e.imr == APPLICATION_IMR && e.event.starts_with(&resplit) {
            return None;
        }
        if e.event != name {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(e);
    }
    // An IMR 3 entry either has no digest, so the replay recomputed one that binds its text,
    // or passed the commitment check above.
    let e = found?;
    (e.imr == APPLICATION_IMR).then_some(e.event_payload.as_slice())
}

/// Whether an entry's digest is the hash of its own text, in either dstack format.
fn commits(e: &EventLog) -> bool {
    e.digest == sha384(&preimage_v1(e)) || e.digest == sha384(&preimage_v2(e))
}

/// dstack v1: `event_type_le ":" name ":" payload`.
pub fn preimage_v1(e: &EventLog) -> Vec<u8> {
    [
        &e.event_type.to_le_bytes()[..],
        b":",
        e.event.as_bytes(),
        b":",
        &e.event_payload,
    ]
    .concat()
}

/// dstack v2: canonical JSON of `{name, payload, type}`, keys sorted.
pub fn preimage_v2(e: &EventLog) -> Vec<u8> {
    serde_json::json!({ "name": e.event, "payload": hex::encode(&e.event_payload), "type": e.event_type })
        .to_string()
        .into_bytes()
}

pub fn sha384(data: &[u8]) -> [u8; 48] {
    Sha384::digest(data).into()
}

fn bytes<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
    let s = String::deserialize(d)?;
    hex::decode(s.trim_start_matches("0x")).map_err(serde::de::Error::custom)
}

/// dstack writes an empty string when it recorded no digest; that reads as zeros.
fn digest<'de, D: serde::Deserializer<'de>>(d: D) -> Result<[u8; 48], D::Error> {
    let b = bytes(d)?;
    if b.is_empty() {
        return Ok([0u8; 48]);
    }
    b.try_into()
        .map_err(|_| serde::de::Error::custom("digest must be 48 bytes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ET: u32 = 0x0800_0001;

    fn ev(name: &str, payload: &[u8]) -> EventLog {
        let mut e = EventLog {
            imr: APPLICATION_IMR,
            event_type: ET,
            digest: [0; 48],
            event: name.into(),
            event_payload: payload.to_vec(),
        };
        e.digest = sha384(&preimage_v1(&e));
        e
    }

    fn log() -> Vec<EventLog> {
        vec![
            ev("os-image-hash", &[0xbb; 32]),
            ev("compose-hash", &[0xaa; 32]),
            ev("instance-id", b"a"),
        ]
    }

    #[test]
    fn a_value_reads_only_from_imr3() {
        let mut l = log();
        assert_eq!(value(&l, "compose-hash"), Some(&[0xaa; 32][..]));
        l[1].imr = 2;
        assert_eq!(value(&l, "compose-hash"), None);
    }

    #[test]
    fn a_relabelled_imr3_entry_refuses_every_value() {
        let mut l = log();
        l[2].event = "other".into(); // its digest no longer commits to its text
        assert_eq!(value(&l, "compose-hash"), None);
    }

    #[test]
    fn a_resplit_name_refuses_the_value() {
        // `compose-hash:x` with payload `y` hashes like `compose-hash` with payload `x:y`.
        let mut l = log();
        l.push(ev("compose-hash:x", b"y"));
        assert_eq!(value(&l, "compose-hash"), None);
        assert_eq!(value(&l, "os-image-hash"), Some(&[0xbb; 32][..]));
    }

    #[test]
    fn a_duplicated_name_refuses_the_value() {
        let mut l = log();
        l.push(ev("compose-hash", &[0xcc; 32]));
        assert_eq!(value(&l, "compose-hash"), None);
    }

    #[test]
    fn relabelled_text_is_refused_although_the_rtmrs_still_match() {
        let good = log();
        let mut forged = good.clone();
        forged[2].event = "compose-hash".into();
        forged[2].event_payload = vec![0xde; 32];
        forged.remove(1);
        assert_eq!(value(&forged, "compose-hash"), None);
        assert_eq!(value(&good, "compose-hash"), Some(&[0xaa; 32][..]));
    }

    #[test]
    fn a_digest_that_does_not_commit_to_its_payload_is_refused() {
        let mut l = log();
        l[1].event_payload = vec![0xde; 32];
        assert_eq!(value(&l, "compose-hash"), None);
    }

    #[test]
    fn both_dstack_digest_versions_are_accepted() {
        let mut e = ev("app-id", &[0xde, 0xad]);
        assert!(value(std::slice::from_ref(&e), "app-id").is_some(), "v1");
        e.digest = sha384(&preimage_v2(&e));
        assert!(value(std::slice::from_ref(&e), "app-id").is_some(), "v2");
        assert_eq!(
            String::from_utf8(preimage_v2(&e)).unwrap(),
            r#"{"name":"app-id","payload":"dead","type":134217729}"#
        );
    }

    #[test]
    fn an_empty_digest_reads_as_zeros_and_is_recomputed() {
        let log: Vec<EventLog> = serde_json::from_str(
            r#"[{"imr":3,"event_type":134217729,"digest":"","event":"app-id","event_payload":"dead"}]"#,
        )
        .unwrap();
        assert_eq!(log[0].digest, [0; 48]);
        assert_ne!(replay(&log)[3], [0; 48]);
    }
}
