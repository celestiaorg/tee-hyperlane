//! Re-executing an EVM block inside the enclave, against a witness rather than a database.
//!
//! This is what makes Eden's state root worth something. Without it the root is whatever the
//! sequencer signed, and a sequencer that signs a fabricated root mints whatever it likes on
//! the far side of the bridge. With it the sequencer still chooses which transactions run and
//! in what order, which is the ordering power every sequencer has, but it cannot invent a
//! state that executing those transactions would not produce.

pub mod exec;
pub mod mpt;

pub use exec::{execute_block, BlockExec, ExecError};
pub use mpt::{ordered_trie_root, TrieError, Witness, EMPTY_ROOT};
