//! A merkle-patricia trie read and rewritten through a witness.
//!
//! The enclave holds no state, only the nodes `debug_executionWitness` gives for the paths
//! one block touched. Reads are authenticated because every walk starts at a trusted root,
//! and the new root is rebuildable because an untouched subtree keeps its existing hash.
//!
//! Not reth's trie, which updates a database rather than a witness.

use std::collections::HashMap;

use alloy_primitives::{keccak256, B256};

/// An empty trie: `keccak(rlp(""))`.
pub const EMPTY_ROOT: B256 = B256::new([
    0x56, 0xe8, 0x1f, 0x17, 0x1b, 0xcc, 0x55, 0xa6, 0xff, 0x83, 0x45, 0xe6, 0x92, 0xc0, 0xf8, 0x6e,
    0x5b, 0x48, 0xe0, 0x1b, 0x99, 0x6c, 0xad, 0xc0, 0x01, 0x62, 0x2f, 0xb5, 0xe3, 0x63, 0xb4, 0x21,
]);

#[derive(Debug, thiserror::Error)]
pub enum TrieError {
    #[error("the witness has no node for hash {0}")]
    MissingNode(B256),
    #[error("malformed trie node: {0}")]
    Malformed(&'static str),
    #[error("a trie node reference is {0} bytes, which is neither inline nor a hash")]
    BadReference(usize),
}

/// Every trie node the witness carries, indexed the way a reference names it.
///
/// One map for the account trie and every storage trie, which is how the witness arrives.
/// Safe to mix, because the index is the node's own hash.
#[derive(Default, Clone)]
pub struct Witness {
    nodes: HashMap<B256, Vec<u8>>,
}

impl Witness {
    pub fn new<I, T>(nodes: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: AsRef<[u8]>,
    {
        let mut map = HashMap::new();
        for node in nodes {
            let bytes = node.as_ref().to_vec();
            map.insert(keccak256(&bytes), bytes);
        }
        Self { nodes: map }
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    fn node(&self, hash: B256) -> Result<&[u8], TrieError> {
        self.nodes
            .get(&hash)
            .map(Vec::as_slice)
            .ok_or(TrieError::MissingNode(hash))
    }

    /// The value at `key` under `root`, or `None` when the trie proves its absence.
    ///
    /// `key` is the already-hashed key, which is what the trie is actually keyed by.
    pub fn get(&self, root: B256, key: B256) -> Result<Option<Vec<u8>>, TrieError> {
        if root == EMPTY_ROOT {
            return Ok(None);
        }
        let path = nibbles(key.as_slice());
        self.get_at(&Ref::Hash(root), &path)
    }

    fn get_at(&self, node: &Ref, path: &[u8]) -> Result<Option<Vec<u8>>, TrieError> {
        let decoded = match node {
            Ref::Hash(h) => decode_node(self.node(*h)?)?,
            Ref::Inline(raw) => decode_node(raw)?,
        };
        match decoded {
            Node::Leaf { path: suffix, value } => {
                Ok(if suffix == path { Some(value) } else { None })
            }
            Node::Extension { path: prefix, child } => {
                if path.len() >= prefix.len() && path[..prefix.len()] == prefix[..] {
                    self.get_at(&child, &path[prefix.len()..])
                } else {
                    Ok(None)
                }
            }
            Node::Branch { children, value } => match path.split_first() {
                None => Ok(value),
                Some((&nib, rest)) => match &children[nib as usize] {
                    None => Ok(None),
                    Some(child) => self.get_at(child, rest),
                },
            },
        }
    }

    /// Apply every change and return the new root.
    ///
    /// `changes` are (hashed key, new value); `None` deletes. A subtree nothing in `changes`
    /// reaches is never decoded, so a witness that only covers the touched paths is enough.
    pub fn update(
        &self,
        root: B256,
        changes: &[(B256, Option<Vec<u8>>)],
    ) -> Result<B256, TrieError> {
        // A key named twice takes its last value, which is what a caller folding a block's
        // writes together would expect. Collapsing before sorting keeps that order.
        let mut folded: HashMap<B256, Option<Vec<u8>>> = HashMap::new();
        for (k, v) in changes {
            folded.insert(*k, v.clone());
        }
        let mut work: Vec<(Vec<u8>, Option<Vec<u8>>)> = folded
            .into_iter()
            .map(|(k, v)| (nibbles(k.as_slice()), v))
            .collect();
        work.sort_by(|a, b| a.0.cmp(&b.0));

        let start = if root == EMPTY_ROOT {
            None
        } else {
            Some(Arm::Kept(Ref::Hash(root)))
        };
        match self.update_at(start, &work)? {
            None => Ok(EMPTY_ROOT),
            Some(node) => Ok(keccak256(encode_node(&node))),
        }
    }

    /// Rewrite one subtree. `None` in, `None` out means the subtree stays empty.
    fn update_at(
        &self,
        arm: Option<Arm>,
        changes: &[(Vec<u8>, Option<Vec<u8>>)],
    ) -> Result<Option<Node>, TrieError> {
        if changes.is_empty() {
            // Nothing below here moved, so whatever names this subtree still stands.
            return Ok(match arm {
                None => None,
                Some(a) => Some(self.open(a)?),
            });
        }
        let Some(arm) = arm else {
            return Ok(build_from(changes));
        };
        match self.open(arm)? {
            Node::Leaf { path, value } => self.update_leaf(path, value, changes),
            // An extension is a branch with one arm and a long neck. Saying so out loud is
            // what removes the special cases: that arm may now split, deepen or vanish, and
            // `update_branch` already knows how to do each of those.
            Node::Extension { path, child } => {
                let Some((head, tail)) = path.split_first() else {
                    return Err(TrieError::Malformed("extension with an empty path"));
                };
                let inner = if tail.is_empty() {
                    Arm::Kept(child)
                } else {
                    Arm::New(Node::Extension {
                        path: tail.to_vec(),
                        child,
                    })
                };
                let mut arms: [Option<Arm>; 16] = Default::default();
                arms[*head as usize] = Some(inner);
                self.update_branch(arms, None, changes)
            }
            Node::Branch { children, value } => {
                self.update_branch(children.map(|c| c.map(Arm::Kept)), value, changes)
            }
        }
    }

    fn update_leaf(
        &self,
        path: Vec<u8>,
        value: Vec<u8>,
        changes: &[(Vec<u8>, Option<Vec<u8>>)],
    ) -> Result<Option<Node>, TrieError> {
        // The leaf already here is just another entry once it is written down as one.
        let mut entries: Vec<(Vec<u8>, Option<Vec<u8>>)> = vec![(path, Some(value))];
        for (k, v) in changes {
            match entries.iter_mut().find(|(ek, _)| ek == k) {
                Some(slot) => slot.1 = v.clone(),
                None => entries.push((k.clone(), v.clone())),
            }
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(build_from(&entries))
    }

    fn update_branch(
        &self,
        mut arms: [Option<Arm>; 16],
        value: Option<Vec<u8>>,
        changes: &[(Vec<u8>, Option<Vec<u8>>)],
    ) -> Result<Option<Node>, TrieError> {
        let mut value = value;
        // An arm is either a reference from the witness or a node just built. Keep them
        // apart: a rebuilt node has no hash to look up if the branch later collapses.
        for nib in 0..16usize {
            let below: Vec<_> = changes
                .iter()
                .filter(|(k, _)| k.first() == Some(&(nib as u8)))
                .map(|(k, v)| (k[1..].to_vec(), v.clone()))
                .collect();
            if below.is_empty() {
                continue;
            }
            arms[nib] = self.update_at(arms[nib].take(), &below)?.map(Arm::New);
        }
        for (k, v) in changes {
            if k.is_empty() {
                value = v.clone();
            }
        }

        let live: Vec<usize> = (0..16).filter(|&n| arms[n].is_some()).collect();
        match (live.len(), &value) {
            (0, None) => Ok(None),
            // A branch holding one value and no arms is just that value at this depth.
            (0, Some(v)) => Ok(Some(Node::Leaf {
                path: Vec::new(),
                value: v.clone(),
            })),
            (1, None) => {
                // One arm left and no value: the branch is redundant and folds into whatever
                // is below it, which has to be decoded to take its prefix. That is why a
                // witness must carry the siblings of anything a block may delete.
                let nib = live[0];
                let inner = self.open(arms[nib].take().expect("checked"))?;
                Ok(Some(join(&[nib as u8], inner)))
            }
            _ => Ok(Some(Node::Branch {
                children: arms.map(|a| a.map(Arm::into_ref)),
                value,
            })),
        }
    }

    /// The node an arm stands for: decoded out of the witness, or already in hand.
    fn open(&self, arm: Arm) -> Result<Node, TrieError> {
        match arm {
            Arm::New(node) => Ok(node),
            Arm::Kept(Ref::Hash(h)) => decode_node(self.node(h)?),
            Arm::Kept(Ref::Inline(raw)) => decode_node(&raw),
        }
    }
}

/// One arm of a branch mid-update: untouched and still named by the witness, or rebuilt and
/// held here in full.
enum Arm {
    Kept(Ref),
    New(Node),
}

impl Arm {
    fn into_ref(self) -> Ref {
        match self {
            Arm::Kept(r) => r,
            Arm::New(node) => inline_or_hash(node),
        }
    }
}

/// How a parent names a child: by hash when the child's encoding reaches 32 bytes, and by
/// carrying the encoding itself when it does not.
#[derive(Debug, Clone)]
pub enum Ref {
    Hash(B256),
    Inline(Vec<u8>),
}

#[derive(Debug, Clone)]
pub enum Node {
    Leaf { path: Vec<u8>, value: Vec<u8> },
    Extension { path: Vec<u8>, child: Ref },
    Branch { children: [Option<Ref>; 16], value: Option<Vec<u8>> },
}

fn inline_or_hash(node: Node) -> Ref {
    let encoded = encode_node(&node);
    if encoded.len() < 32 {
        Ref::Inline(encoded)
    } else {
        Ref::Hash(keccak256(&encoded))
    }
}

/// Put `prefix` in front of a node, which is an extension unless the prefix is empty.
fn join(prefix: &[u8], node: Node) -> Node {
    if prefix.is_empty() {
        return node;
    }
    match node {
        // Two prefixes in a row are one prefix.
        Node::Leaf { path, value } => Node::Leaf {
            path: [prefix, &path].concat(),
            value,
        },
        Node::Extension { path, child } => Node::Extension {
            path: [prefix, &path].concat(),
            child,
        },
        branch => Node::Extension {
            path: prefix.to_vec(),
            child: inline_or_hash(branch),
        },
    }
}

/// Build a subtree from scratch out of sorted entries. Deletions among them are nothing.
fn build_from(entries: &[(Vec<u8>, Option<Vec<u8>>)]) -> Option<Node> {
    let live: Vec<(&[u8], &Vec<u8>)> = entries
        .iter()
        .filter_map(|(k, v)| v.as_ref().map(|v| (k.as_slice(), v)))
        .collect();
    build_live(&live)
}

fn build_live(entries: &[(&[u8], &Vec<u8>)]) -> Option<Node> {
    match entries.len() {
        0 => None,
        1 => Some(Node::Leaf {
            path: entries[0].0.to_vec(),
            value: entries[0].1.clone(),
        }),
        _ => {
            let shared = entries
                .iter()
                .skip(1)
                .fold(entries[0].0.len(), |acc, (k, _)| {
                    acc.min(common_prefix(entries[0].0, k))
                });
            let mut children: [Option<Ref>; 16] = Default::default();
            let mut value = None;
            for nib in 0..16u8 {
                let below: Vec<(&[u8], &Vec<u8>)> = entries
                    .iter()
                    .filter(|(k, _)| k.get(shared) == Some(&nib))
                    .map(|(k, v)| (&k[shared + 1..], *v))
                    .collect();
                if let Some(node) = build_live(&below) {
                    children[nib as usize] = Some(inline_or_hash(node));
                }
            }
            for (k, v) in entries {
                if k.len() == shared {
                    value = Some((*v).clone());
                }
            }
            let branch = Node::Branch { children, value };
            Some(join(&entries[0].0[..shared], branch))
        }
    }
}

// ---------------------------------------------------------------- encoding

fn nibbles(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(b >> 4);
        out.push(b & 0x0f);
    }
    out
}

fn common_prefix(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).take_while(|(x, y)| x == y).count()
}

/// Hex-prefix encoding: one flag nibble saying leaf or extension and odd or even, then the
/// path, padded to whole bytes.
fn hex_prefix(path: &[u8], leaf: bool) -> Vec<u8> {
    let flag = if leaf { 2u8 } else { 0u8 };
    let odd = path.len() % 2 == 1;
    let mut nibs = Vec::with_capacity(path.len() + 2);
    if odd {
        nibs.push(flag + 1);
        nibs.extend_from_slice(path);
    } else {
        nibs.push(flag);
        nibs.push(0);
        nibs.extend_from_slice(path);
    }
    nibs.chunks(2).map(|c| (c[0] << 4) | c[1]).collect()
}

fn from_hex_prefix(bytes: &[u8]) -> Result<(Vec<u8>, bool), TrieError> {
    let Some(&first) = bytes.first() else {
        return Err(TrieError::Malformed("empty hex-prefix path"));
    };
    let flag = first >> 4;
    let leaf = flag & 2 != 0;
    let odd = flag & 1 != 0;
    let mut path = nibbles(bytes);
    path.drain(..if odd { 1 } else { 2 });
    Ok((path, leaf))
}

fn decode_node(raw: &[u8]) -> Result<Node, TrieError> {
    let items = rlp_list(raw)?;
    match items.len() {
        2 => {
            let (path, leaf) = from_hex_prefix(&items[0])?;
            if leaf {
                Ok(Node::Leaf {
                    path,
                    value: items[1].clone(),
                })
            } else {
                Ok(Node::Extension {
                    path,
                    child: decode_ref(&items[1])?,
                })
            }
        }
        17 => {
            let mut children: [Option<Ref>; 16] = Default::default();
            for (i, slot) in children.iter_mut().enumerate() {
                if !items[i].is_empty() {
                    *slot = Some(decode_ref(&items[i])?);
                }
            }
            let value = if items[16].is_empty() {
                None
            } else {
                Some(items[16].clone())
            };
            Ok(Node::Branch { children, value })
        }
        _ => Err(TrieError::Malformed("a trie node is 2 or 17 items")),
    }
}

/// A child reference as it sits inside its parent.
///
/// `rlp_list` has already stripped one layer, so a 32-byte hash arrives as exactly 32 bytes
/// and an inline node arrives as its own encoding, which is a list.
fn decode_ref(raw: &[u8]) -> Result<Ref, TrieError> {
    if raw.len() == 32 {
        Ok(Ref::Hash(B256::from_slice(raw)))
    } else if raw.len() < 32 {
        Ok(Ref::Inline(raw.to_vec()))
    } else {
        Err(TrieError::BadReference(raw.len()))
    }
}

fn encode_node(node: &Node) -> Vec<u8> {
    match node {
        Node::Leaf { path, value } => {
            rlp_encode_list(&[rlp_string(&hex_prefix(path, true)), rlp_string(value)])
        }
        Node::Extension { path, child } => {
            rlp_encode_list(&[rlp_string(&hex_prefix(path, false)), encode_ref(child)])
        }
        Node::Branch { children, value } => {
            let mut items: Vec<Vec<u8>> = Vec::with_capacity(17);
            for child in children {
                items.push(match child {
                    None => vec![0x80],
                    Some(r) => encode_ref(r),
                });
            }
            items.push(match value {
                None => vec![0x80],
                Some(v) => rlp_string(v),
            });
            rlp_encode_list(&items)
        }
    }
}

/// A child as its parent writes it: a 32-byte string for a hash, the node itself when short.
fn encode_ref(child: &Ref) -> Vec<u8> {
    match child {
        Ref::Hash(h) => rlp_string(h.as_slice()),
        Ref::Inline(raw) => raw.clone(),
    }
}

// ---------------------------------------------------------------- minimal rlp
//
// Only what trie nodes need: a list of strings, any of which may be an inlined node. Walks
// bytes rather than using alloy-rlp, because the shape is what is being discovered.

fn rlp_string(bytes: &[u8]) -> Vec<u8> {
    if bytes.len() == 1 && bytes[0] < 0x80 {
        return bytes.to_vec();
    }
    let mut out = rlp_length_prefix(bytes.len(), 0x80);
    out.extend_from_slice(bytes);
    out
}

fn rlp_encode_list(items: &[Vec<u8>]) -> Vec<u8> {
    let body: Vec<u8> = items.concat();
    let mut out = rlp_length_prefix(body.len(), 0xc0);
    out.extend_from_slice(&body);
    out
}

fn rlp_length_prefix(len: usize, short: u8) -> Vec<u8> {
    if len < 56 {
        return vec![short + len as u8];
    }
    let be = len.to_be_bytes();
    let start = be.iter().position(|&b| b != 0).unwrap_or(be.len() - 1);
    let mut out = vec![short + 55 + (be.len() - start) as u8];
    out.extend_from_slice(&be[start..]);
    out
}

/// Split an RLP list into its elements, each returned as the bytes a child reference needs:
/// the payload for a string, the whole encoding for a nested list.
fn rlp_list(raw: &[u8]) -> Result<Vec<Vec<u8>>, TrieError> {
    let (payload, rest) = rlp_split(raw, true)?;
    if !rest.is_empty() {
        return Err(TrieError::Malformed("trailing bytes after a trie node"));
    }
    let mut out = Vec::new();
    let mut cursor = payload;
    while !cursor.is_empty() {
        let is_list = cursor[0] >= 0xc0;
        let (item, rest) = rlp_split(cursor, is_list)?;
        out.push(if is_list {
            cursor[..cursor.len() - rest.len()].to_vec()
        } else {
            item.to_vec()
        });
        cursor = rest;
    }
    Ok(out)
}

/// Return an item's payload and whatever follows it.
fn rlp_split(raw: &[u8], want_list: bool) -> Result<(&[u8], &[u8]), TrieError> {
    let Some(&first) = raw.first() else {
        return Err(TrieError::Malformed("rlp ran out"));
    };
    let (offset, len) = match first {
        0x00..=0x7f if !want_list => (0usize, 1usize),
        0x80..=0xb7 if !want_list => (1, (first - 0x80) as usize),
        0xb8..=0xbf if !want_list => {
            let n = (first - 0xb7) as usize;
            (1 + n, be_len(raw.get(1..1 + n).ok_or(SHORT)?)?)
        }
        0xc0..=0xf7 if want_list => (1, (first - 0xc0) as usize),
        0xf8..=0xff if want_list => {
            let n = (first - 0xf7) as usize;
            (1 + n, be_len(raw.get(1..1 + n).ok_or(SHORT)?)?)
        }
        _ => return Err(TrieError::Malformed("rlp item is not the expected kind")),
    };
    let end = offset.checked_add(len).ok_or(SHORT)?;
    if end > raw.len() {
        return Err(SHORT);
    }
    Ok((&raw[offset..end], &raw[end..]))
}

const SHORT: TrieError = TrieError::Malformed("rlp item runs past the end");

fn be_len(bytes: &[u8]) -> Result<usize, TrieError> {
    if bytes.is_empty() || bytes.len() > 8 {
        return Err(TrieError::Malformed("rlp length header out of range"));
    }
    Ok(bytes.iter().fold(0usize, |acc, &b| (acc << 8) | b as usize))
}

/// Build a whole trie from every entry, returning its root and all of its nodes.
///
/// For tests only: `update` is checked against a trie built this way, which is what covers
/// branches collapsing and leaves splitting.
pub fn build(entries: &[(B256, Vec<u8>)]) -> (B256, Vec<Vec<u8>>) {
    let mut folded: HashMap<B256, Vec<u8>> = HashMap::new();
    for (k, v) in entries {
        folded.insert(*k, v.clone());
    }
    let mut keyed: Vec<(Vec<u8>, Vec<u8>)> = folded
        .into_iter()
        .map(|(k, v)| (nibbles(k.as_slice()), v))
        .collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    let refs: Vec<(&[u8], &Vec<u8>)> = keyed.iter().map(|(k, v)| (k.as_slice(), v)).collect();
    let mut nodes = Vec::new();
    match build_nodes(&refs, &mut nodes) {
        None => (EMPTY_ROOT, Vec::new()),
        // A trie small enough to inline still has a root hash, and the witness still needs
        // the bytes it is the hash of.
        Some(Ref::Inline(raw)) => {
            let hash = keccak256(&raw);
            nodes.push(raw);
            (hash, nodes)
        }
        Some(Ref::Hash(hash)) => (hash, nodes),
    }
}

fn record(node: Node, out: &mut Vec<Vec<u8>>) -> Ref {
    let encoded = encode_node(&node);
    if encoded.len() < 32 {
        Ref::Inline(encoded)
    } else {
        let hash = keccak256(&encoded);
        out.push(encoded);
        Ref::Hash(hash)
    }
}

fn build_nodes(entries: &[(&[u8], &Vec<u8>)], out: &mut Vec<Vec<u8>>) -> Option<Ref> {
    match entries.len() {
        0 => None,
        1 => Some(record(
            Node::Leaf {
                path: entries[0].0.to_vec(),
                value: entries[0].1.clone(),
            },
            out,
        )),
        _ => {
            let shared = entries
                .iter()
                .skip(1)
                .fold(entries[0].0.len(), |acc, (k, _)| {
                    acc.min(common_prefix(entries[0].0, k))
                });
            let mut children: [Option<Ref>; 16] = Default::default();
            let mut value = None;
            for nib in 0..16u8 {
                let below: Vec<(&[u8], &Vec<u8>)> = entries
                    .iter()
                    .filter(|(k, _)| k.get(shared) == Some(&nib))
                    .map(|(k, v)| (&k[shared + 1..], *v))
                    .collect();
                children[nib as usize] = build_nodes(&below, out);
            }
            for (k, v) in entries {
                if k.len() == shared {
                    value = Some((*v).clone());
                }
            }
            let branch = record(Node::Branch { children, value }, out);
            if shared == 0 {
                Some(branch)
            } else {
                Some(record(
                    Node::Extension {
                        path: entries[0].0[..shared].to_vec(),
                        child: branch,
                    },
                    out,
                ))
            }
        }
    }
}

/// The root of a trie keyed by the RLP of an index, which is how a block commits to its
/// transactions and its receipts.
pub fn ordered_trie_root(items: &[Vec<u8>]) -> B256 {
    if items.is_empty() {
        return EMPTY_ROOT;
    }
    let mut entries: Vec<(Vec<u8>, Vec<u8>)> = items
        .iter()
        .enumerate()
        .map(|(i, item)| (nibbles(&rlp_index(i)), item.clone()))
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let refs: Vec<(&[u8], &Vec<u8>)> = entries.iter().map(|(k, v)| (k.as_slice(), v)).collect();
    match build_live(&refs) {
        None => EMPTY_ROOT,
        Some(node) => keccak256(encode_node(&node)),
    }
}

fn rlp_index(i: usize) -> Vec<u8> {
    if i == 0 {
        return vec![0x80];
    }
    let be = i.to_be_bytes();
    let start = be.iter().position(|&b| b != 0).unwrap_or(be.len() - 1);
    rlp_string(&be[start..])
}
