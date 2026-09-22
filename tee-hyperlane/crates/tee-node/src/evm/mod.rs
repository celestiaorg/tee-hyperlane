//! Re-executing an EVM block inside the enclave, against a witness rather than a database.
//!
//! Without this an Eden state root is only what its sequencer signed. With it the sequencer
//! still orders transactions but cannot invent a state they would not produce.

pub mod exec;
pub mod mpt;

pub use exec::{execute_block, BlockExec, ExecError};
pub use mpt::{ordered_trie_root, TrieError, Witness, EMPTY_ROOT};
