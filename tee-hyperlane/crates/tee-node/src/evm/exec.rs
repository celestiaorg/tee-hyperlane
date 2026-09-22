//! Execute one block against a witness and rebuild the state root it must produce.

use std::collections::HashMap;

use alloy_consensus::transaction::SignerRecoverable;
use alloy_consensus::{Header, Transaction, TxEnvelope};
use alloy_eips::eip2718::Decodable2718;
use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_rlp::Decodable;
use ev_revm::factory::BaseFeeRedirectSettings;
use ev_revm::{BaseFeeRedirect, EvTxEnv, EvTxEvmFactory};
use alloy_evm::{Evm, EvmEnv, EvmFactory};
use revm::context::result::ExecutionResult;
use revm::context::{BlockEnv, CfgEnv, TxEnv};
use revm::context_interface::block::BlobExcessGasAndPrice;
use revm::database::states::bundle_state::BundleRetention;
use revm::database::{BundleState, State};
use revm::database_interface::{DBErrorMarker, Database};
use revm::primitives::hardfork::SpecId;
use revm::primitives::{StorageKey, StorageValue};
use revm::state::{AccountInfo, Bytecode};
use revm::DatabaseCommit;

use super::mpt::{ordered_trie_root, TrieError, Witness, EMPTY_ROOT};

/// `keccak(rlp([]))`, the ommers hash every post-merge block carries.
const EMPTY_OMMERS: B256 = B256::new([
    0x1d, 0xcc, 0x4d, 0xe8, 0xde, 0xc7, 0x5d, 0x7a, 0xab, 0x85, 0xb5, 0x67, 0xb6, 0xcc, 0xd4, 0x1a,
    0xd3, 0x12, 0x45, 0x1b, 0x94, 0x8a, 0x74, 0x13, 0xf0, 0xa1, 0x42, 0xfd, 0x40, 0xd4, 0x93, 0x47,
]);

/// One block and everything needed to run it without a database.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BlockExec {
    /// RLP of the block header.
    pub header: Bytes,
    /// RLP of each transaction, in block order, as EIP-2718 envelopes.
    pub transactions: Vec<Bytes>,
    /// Trie nodes: `debug_executionWitness`'s `state`, for the account trie and every
    /// storage trie the block touched.
    pub state: Vec<Bytes>,
    /// Bytecode of every contract the block called.
    pub codes: Vec<Bytes>,
    /// RLP headers of recent ancestors, for `BLOCKHASH`.
    #[serde(default)]
    pub ancestors: Vec<Bytes>,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("block header does not decode: {0}")]
    Header(&'static str),
    #[error("transaction {index} does not decode: {reason}")]
    Transaction { index: usize, reason: String },
    #[error("transaction {index} has no recoverable sender")]
    Sender { index: usize },
    #[error("block {number} lists a transactions root of {want} but its transactions build {got}")]
    TransactionsRoot { number: u64, got: String, want: String },
    #[error("block {number} has ommers, which no post-merge block may")]
    Ommers { number: u64 },
    #[error("block {number} has withdrawals, which this executor does not run")]
    Withdrawals { number: u64 },
    #[error("block {number} claims parent state root {want}, but the chain is at {got}")]
    WrongParentState { number: u64, got: String, want: String },
    #[error("block {number} is numbered below its parent {parent}")]
    NotAscending { number: u64, parent: u64 },
    #[error("re-executing block {number} gives state root {got}, but it claims {want}")]
    StateRoot { number: u64, got: String, want: String },
    #[error("block {number} uses {got} gas but claims {want}")]
    GasUsed { number: u64, got: u64, want: u64 },
    #[error("executing transaction {index}: {reason}")]
    Evm { index: usize, reason: String },
    #[error("the witness is missing the code for hash {0}")]
    MissingCode(B256),
    #[error("the witness is missing the header for block {0}")]
    MissingAncestor(u64),
    #[error("account entry for {address} is not a four item list")]
    BadAccount { address: String },
    #[error(transparent)]
    Trie(#[from] TrieError),
}

/// Run one block on the given pre-state and return its header, having checked that the state
/// root it claims is the one execution produces.
///
/// Chains by state root rather than block hash, so the caller may skip any run of blocks that
/// changed nothing.
pub fn execute_block(
    parent_number: u64,
    parent_state_root: B256,
    input: &BlockExec,
) -> Result<Header, ExecError> {
    let header = Header::decode(&mut input.header.as_ref())
        .map_err(|_| ExecError::Header("not a block header"))?;

    if header.number <= parent_number {
        return Err(ExecError::NotAscending {
            number: header.number,
            parent: parent_number,
        });
    }
    if header.ommers_hash != EMPTY_OMMERS {
        return Err(ExecError::Ommers {
            number: header.number,
        });
    }
    // A withdrawal credits an account outside the transaction list, so ignoring one would
    // give a root short of it. Refuse rather than half-support the field.
    if !matches!(header.withdrawals_root, None | Some(EMPTY_ROOT)) {
        return Err(ExecError::Withdrawals {
            number: header.number,
        });
    }

    let txs: Vec<Vec<u8>> = input.transactions.iter().map(|t| t.to_vec()).collect();
    let built = ordered_trie_root(&txs);
    if built != header.transactions_root {
        return Err(ExecError::TransactionsRoot {
            number: header.number,
            got: built.to_string(),
            want: header.transactions_root.to_string(),
        });
    }

    let witness = Witness::new(input.state.iter().map(|n| n.to_vec()));
    let codes: HashMap<B256, Bytes> = input
        .codes
        .iter()
        .map(|c| (keccak256(c.as_ref()), c.clone()))
        .collect();
    let mut ancestors = HashMap::new();
    for raw in &input.ancestors {
        if let Ok(h) = Header::decode(&mut raw.as_ref()) {
            ancestors.insert(h.number, keccak256(raw.as_ref()));
        }
    }

    // revm's `State` merges the per-transaction diffs into one bundle with pre-state values
    // attached, which is what gets destroy-then-recreate right.
    let mut state = State::builder()
        .with_database(WitnessDb {
            witness: &witness,
            root: parent_state_root,
            codes: &codes,
            ancestors: &ancestors,
        })
        .with_bundle_update()
        .build();

    let mut cfg = CfgEnv::default();
    cfg.chain_id = CHAIN_ID;
    // Osaka, not Prague: P-256 verification is an Osaka precompile, and a DCAP verification
    // needs it. Under Prague that call returns nothing and the verifier reverts.
    cfg.spec = SpecId::OSAKA;
    let block = BlockEnv {
        number: U256::from(header.number),
        beneficiary: header.beneficiary,
        timestamp: U256::from(header.timestamp),
        gas_limit: header.gas_limit,
        basefee: header.base_fee_per_gas.unwrap_or_default(),
        difficulty: header.difficulty,
        prevrandao: Some(header.mix_hash),
        blob_excess_gas_and_price: header
            .excess_blob_gas
            .map(|g| BlobExcessGasAndPrice::new_with_spec(g, SpecId::OSAKA)),
        ..Default::default()
    };

    let mut evm = eden_evm_factory(header.beneficiary).create_evm(
        &mut state,
        EvmEnv {
            cfg_env: cfg,
            block_env: block,
        },
    );

    let mut gas_used = 0u64;
    for (index, raw) in input.transactions.iter().enumerate() {
        let envelope = TxEnvelope::decode_2718(&mut raw.as_ref()).map_err(|e| {
            ExecError::Transaction {
                index,
                reason: e.to_string(),
            }
        })?;
        let caller = envelope
            .recover_signer()
            .map_err(|_| ExecError::Sender { index })?;
        let tx: EvTxEnv = tx_env(&envelope, caller).into();
        let outcome = evm.transact(tx).map_err(|e| ExecError::Evm {
            index,
            reason: e.to_string(),
        })?;
        gas_used += match &outcome.result {
            ExecutionResult::Success { gas, .. }
            | ExecutionResult::Revert { gas, .. }
            | ExecutionResult::Halt { gas, .. } => gas.tx_gas_used(),
        };
        evm.db_mut().commit(outcome.state);
    }
    drop(evm);

    if gas_used != header.gas_used {
        return Err(ExecError::GasUsed {
            number: header.number,
            got: gas_used,
            want: header.gas_used,
        });
    }

    state.merge_transitions(BundleRetention::PlainState);
    let bundle = state.take_bundle();
    let root = post_state_root(&witness, parent_state_root, &bundle)?;
    if root != header.state_root {
        return Err(ExecError::StateRoot {
            number: header.number,
            got: root.to_string(),
            want: header.state_root.to_string(),
        });
    }
    Ok(header)
}

/// Eden's EVM chain id, which is also its Hyperlane domain.
const CHAIN_ID: u64 = 3_735_928_814;

/// Eden's ev-reth configuration, pinned here rather than taken from the request: whoever
/// chooses the fee sink chooses where value goes.
///
/// The sink is the block's beneficiary, read off the chain. Eden's extra precompiles are left
/// off because it is not known to run them; if it does, the root will not reproduce and the
/// route stops rather than attesting something wrong.
fn eden_evm_factory(beneficiary: Address) -> EvTxEvmFactory {
    EvTxEvmFactory::new(
        Some(BaseFeeRedirectSettings::new(
            BaseFeeRedirect::new(beneficiary),
            0,
        )),
        None,
        None,
        None,
        None,
    )
}

/// Fold every account and storage change back into the trie and take the new root.
fn post_state_root(
    witness: &Witness,
    pre_root: B256,
    bundle: &BundleState,
) -> Result<B256, ExecError> {
    let mut account_changes: Vec<(B256, Option<Vec<u8>>)> = Vec::new();

    for (address, account) in &bundle.state {
        let key = keccak256(address.as_slice());
        let Some(info) = account.info.as_ref() else {
            account_changes.push((key, None));
            continue;
        };

        // A destroyed account's storage goes with it, so what follows builds on an empty
        // trie rather than on whatever the address held before.
        let storage_base = if account.status.was_destroyed() {
            EMPTY_ROOT
        } else {
            account
                .original_info
                .as_ref()
                .map(|_| read_storage_root(witness, pre_root, *address))
                .transpose()?
                .flatten()
                .unwrap_or(EMPTY_ROOT)
        };

        let mut storage_changes: Vec<(B256, Option<Vec<u8>>)> = Vec::new();
        for (slot, value) in &account.storage {
            if !account.status.was_destroyed() && value.present_value == value.previous_or_original_value
            {
                continue;
            }
            storage_changes.push((
                keccak256(B256::from(*slot).as_slice()),
                if value.present_value.is_zero() {
                    None
                } else {
                    Some(rlp_uint(value.present_value))
                },
            ));
        }
        let storage_root = witness.update(storage_base, &storage_changes)?;

        account_changes.push((
            key,
            Some(encode_account(
                info.nonce,
                info.balance,
                storage_root,
                info.code_hash,
            )),
        ));
    }

    Ok(witness.update(pre_root, &account_changes)?)
}

/// The storage root an address had before the block, or `None` if it had no account.
fn read_storage_root(
    witness: &Witness,
    root: B256,
    address: Address,
) -> Result<Option<B256>, ExecError> {
    Ok(read_account(witness, root, address)?.map(|(_, _, storage_root, _)| storage_root))
}

/// `(nonce, balance, storage_root, code_hash)` as the account trie stores it.
fn read_account(
    witness: &Witness,
    root: B256,
    address: Address,
) -> Result<Option<(u64, U256, B256, B256)>, ExecError> {
    let Some(raw) = witness.get(root, keccak256(address.as_slice()))? else {
        return Ok(None);
    };
    let mut slice = raw.as_slice();
    let decoded = alloy_rlp::Header::decode(&mut slice).map_err(|_| ExecError::BadAccount {
        address: address.to_string(),
    })?;
    if !decoded.list {
        return Err(ExecError::BadAccount {
            address: address.to_string(),
        });
    }
    let bad = || ExecError::BadAccount {
        address: address.to_string(),
    };
    let nonce = u64::decode(&mut slice).map_err(|_| bad())?;
    let balance = U256::decode(&mut slice).map_err(|_| bad())?;
    let storage_root = B256::decode(&mut slice).map_err(|_| bad())?;
    let code_hash = B256::decode(&mut slice).map_err(|_| bad())?;
    Ok(Some((nonce, balance, storage_root, code_hash)))
}

fn encode_account(nonce: u64, balance: U256, storage_root: B256, code_hash: B256) -> Vec<u8> {
    use alloy_rlp::Encodable;
    let mut body = Vec::new();
    nonce.encode(&mut body);
    balance.encode(&mut body);
    storage_root.encode(&mut body);
    code_hash.encode(&mut body);
    let mut out = Vec::new();
    alloy_rlp::Header {
        list: true,
        payload_length: body.len(),
    }
    .encode(&mut out);
    out.extend_from_slice(&body);
    out
}

fn rlp_uint(value: StorageValue) -> Vec<u8> {
    use alloy_rlp::Encodable;
    let mut out = Vec::new();
    value.encode(&mut out);
    out
}

fn tx_env(envelope: &TxEnvelope, caller: Address) -> TxEnv {
    let mut tx = TxEnv::default();
    tx.caller = caller;
    tx.gas_limit = envelope.gas_limit();
    tx.gas_price = envelope.max_fee_per_gas();
    tx.gas_priority_fee = envelope.max_priority_fee_per_gas();
    tx.kind = envelope.kind();
    tx.value = envelope.value();
    tx.data = envelope.input().clone();
    tx.nonce = envelope.nonce();
    tx.chain_id = envelope.chain_id();
    tx.tx_type = envelope.tx_type() as u8;
    if let Some(list) = envelope.access_list() {
        tx.access_list = list.clone();
    }
    tx.blob_hashes = envelope.blob_versioned_hashes().unwrap_or_default().to_vec();
    tx.max_fee_per_blob_gas = envelope.max_fee_per_blob_gas().unwrap_or_default();
    if let Some(auth) = envelope.authorization_list() {
        tx.set_signed_authorization(auth.to_vec());
    }
    tx
}

// ---------------------------------------------------------------- database

/// revm's view of the pre-state, served entirely out of the witness.
#[derive(Debug)]
struct WitnessDb<'a> {
    witness: &'a Witness,
    root: B256,
    codes: &'a HashMap<B256, Bytes>,
    ancestors: &'a HashMap<u64, B256>,
}

#[derive(Debug, thiserror::Error)]
pub enum WitnessError {
    #[error(transparent)]
    Trie(#[from] TrieError),
    #[error("the witness is missing the code for hash {0}")]
    MissingCode(B256),
    #[error("the witness is missing the header for block {0}")]
    MissingAncestor(u64),
    #[error("an account entry in the trie is malformed")]
    BadAccount,
}

impl DBErrorMarker for WitnessError {}

impl WitnessDb<'_> {
    fn account(&self, address: Address) -> Result<Option<(u64, U256, B256, B256)>, WitnessError> {
        let Some(raw) = self.witness.get(self.root, keccak256(address.as_slice()))? else {
            return Ok(None);
        };
        let mut slice = raw.as_slice();
        let header = alloy_rlp::Header::decode(&mut slice).map_err(|_| WitnessError::BadAccount)?;
        if !header.list {
            return Err(WitnessError::BadAccount);
        }
        let nonce = u64::decode(&mut slice).map_err(|_| WitnessError::BadAccount)?;
        let balance = U256::decode(&mut slice).map_err(|_| WitnessError::BadAccount)?;
        let storage_root = B256::decode(&mut slice).map_err(|_| WitnessError::BadAccount)?;
        let code_hash = B256::decode(&mut slice).map_err(|_| WitnessError::BadAccount)?;
        Ok(Some((nonce, balance, storage_root, code_hash)))
    }
}

impl Database for WitnessDb<'_> {
    type Error = WitnessError;

    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        let Some((nonce, balance, _, code_hash)) = self.account(address)? else {
            return Ok(None);
        };
        Ok(Some(AccountInfo {
            balance,
            nonce,
            code_hash,
            code: None,
            ..Default::default()
        }))
    }

    fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        if code_hash == alloy_primitives::KECCAK256_EMPTY {
            return Ok(Bytecode::default());
        }
        let raw = self
            .codes
            .get(&code_hash)
            .ok_or(WitnessError::MissingCode(code_hash))?;
        Ok(Bytecode::new_raw(raw.clone()))
    }

    fn storage(
        &mut self,
        address: Address,
        index: StorageKey,
    ) -> Result<StorageValue, Self::Error> {
        let Some((_, _, storage_root, _)) = self.account(address)? else {
            return Ok(StorageValue::ZERO);
        };
        let key = keccak256(B256::from(index).as_slice());
        let Some(raw) = self.witness.get(storage_root, key)? else {
            return Ok(StorageValue::ZERO);
        };
        let mut slice = raw.as_slice();
        StorageValue::decode(&mut slice).map_err(|_| WitnessError::BadAccount)
    }

    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        self.ancestors
            .get(&number)
            .copied()
            .ok_or(WitnessError::MissingAncestor(number))
    }
}
