//! The trie update, held against a trie built from every entry.
//!
//! `update` only ever sees the nodes on the paths a block touched, so the cases that break it
//! are the structural ones: a branch that loses its second-to-last arm and has to fold away,
//! a leaf that gains a neighbour and has to split, a key inserted where the trie only proved
//! an absence. Building the same trie from scratch and comparing roots covers all of them at
//! once, and randomising the keys covers them in combinations nobody would think to write
//! down.

use alloy_primitives::{keccak256, B256};
use tee_node::evm::mpt::{build, Witness, EMPTY_ROOT};

/// A deterministic stand-in for a random generator, so a failure is reproducible.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*, which is plenty for picking keys.
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn key(&mut self, space: u64) -> B256 {
        keccak256((self.next() % space).to_be_bytes())
    }

    fn value(&mut self) -> Vec<u8> {
        self.next().to_be_bytes().to_vec()
    }
}

fn root_of(entries: &[(B256, Vec<u8>)]) -> B256 {
    build(entries).0
}

#[test]
fn update_matches_a_trie_built_from_scratch() {
    for seed in 1..40u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15));
        // A small key space on purpose: it forces keys to share long prefixes, which is what
        // makes extensions and deep branches happen at all.
        let space = 24 + seed * 7;

        let mut entries: Vec<(B256, Vec<u8>)> = Vec::new();
        for _ in 0..(8 + seed % 30) {
            let key = rng.key(space);
            if entries.iter().any(|(k, _)| *k == key) {
                continue;
            }
            entries.push((key, rng.value()));
        }
        let (root, nodes) = build(&entries);
        assert_eq!(root, root_of(&entries));

        // Changes of every kind: overwrite, delete, insert, and delete something absent.
        let mut changes: Vec<(B256, Option<Vec<u8>>)> = Vec::new();
        let mut expected = entries.clone();
        for i in 0..entries.len() {
            match rng.next() % 4 {
                0 => {
                    let v = rng.value();
                    changes.push((entries[i].0, Some(v.clone())));
                    expected[i].1 = v;
                }
                1 => changes.push((entries[i].0, None)),
                _ => {}
            }
        }
        expected.retain(|(k, _)| !changes.iter().any(|(ck, cv)| ck == k && cv.is_none()));
        for _ in 0..(1 + seed % 5) {
            let key = rng.key(space * 3);
            if expected.iter().any(|(k, _)| *k == key) || changes.iter().any(|(k, _)| *k == key) {
                continue;
            }
            if rng.next() % 3 == 0 {
                // Deleting a key the trie never had must be a no-op, not a corruption.
                changes.push((key, None));
                continue;
            }
            let v = rng.value();
            changes.push((key, Some(v.clone())));
            expected.push((key, v));
        }

        let witness = Witness::new(nodes);
        let got = witness
            .update(root, &changes)
            .unwrap_or_else(|e| panic!("seed {seed}: {e}"));
        assert_eq!(got, root_of(&expected), "seed {seed}");
    }
}

#[test]
fn deleting_everything_empties_the_trie() {
    let mut rng = Rng(7);
    let mut entries: Vec<(B256, Vec<u8>)> = Vec::new();
    while entries.len() < 25 {
        let key = rng.key(60);
        if entries.iter().all(|(k, _)| *k != key) {
            entries.push((key, vec![entries.len() as u8]));
        }
    }
    let (root, nodes) = build(&entries);
    let changes: Vec<(B256, Option<Vec<u8>>)> = entries.iter().map(|(k, _)| (*k, None)).collect();
    let witness = Witness::new(nodes);
    assert_eq!(witness.update(root, &changes).unwrap(), EMPTY_ROOT);
}

#[test]
fn reads_walk_to_the_right_value() {
    let mut rng = Rng(11);
    let mut entries: Vec<(B256, Vec<u8>)> = Vec::new();
    while entries.len() < 40 {
        let key = rng.key(80);
        if entries.iter().all(|(k, _)| *k != key) {
            entries.push((key, rng.value()));
        }
    }
    let (root, nodes) = build(&entries);
    let witness = Witness::new(nodes);
    for (key, value) in &entries {
        assert_eq!(witness.get(root, *key).unwrap().as_ref(), Some(value));
    }
    assert_eq!(witness.get(root, keccak256(b"absent")).unwrap(), None);
}

#[test]
fn an_empty_trie_grows_from_nothing() {
    let mut rng = Rng(13);
    let mut entries: Vec<(B256, Vec<u8>)> = Vec::new();
    while entries.len() < 12 {
        let key = rng.key(30);
        if entries.iter().all(|(k, _)| *k != key) {
            entries.push((key, rng.value()));
        }
    }
    let witness = Witness::new(Vec::<Vec<u8>>::new());
    let changes: Vec<(B256, Option<Vec<u8>>)> =
        entries.iter().map(|(k, v)| (*k, Some(v.clone()))).collect();
    assert_eq!(witness.update(EMPTY_ROOT, &changes).unwrap(), root_of(&entries));
}
