//! Execute one block against a witness and rebuild the state root it must produce.

use std::collections::HashMap;

use alloy_consensus::transaction::SignerRecoverable;
use alloy_consensus::{Header, Transaction, TxEnvelope};
use alloy_eips::eip2718::Decodable2718;
use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_rlp::Decodable;
use revm::context::result::{EVMError, ExecutionResult};
use revm::context::{BlockEnv, CfgEnv, TxEnv};
use revm::context_interface::block::BlobExcessGasAndPrice;
use revm::database_interface::{DBErrorMarker, Database};
use revm::primitives::hardfork::SpecId;
use revm::primitives::{StorageKey, StorageValue};
use revm::state::{AccountInfo, Bytecode, EvmState};
use revm::{Context, ExecuteEvm, MainBuilder, MainContext};

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

    let db = WitnessDb {
        witness: &witness,
        root: parent_state_root,
        codes: &codes,
        ancestors: &ancestors,
    };

    let mut cfg = CfgEnv::default();
    cfg.chain_id = CHAIN_ID;
    cfg.spec = SpecId::PRAGUE;
    // Left enforced: a block over its own gas limit is one this executor will not accept.
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
            .map(|g| BlobExcessGasAndPrice::new_with_spec(g, SpecId::PRAGUE)),
        ..Default::default()
    };

    let mut evm = Context::mainnet()
        .with_db(db)
        .with_block(block)
        .with_cfg(cfg)
        .build_mainnet();

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
        let tx = tx_env(&envelope, caller);
        let outcome = evm
            .transact_one(tx)
            .map_err(|e: EVMError<WitnessError>| ExecError::Evm {
                index,
                reason: e.to_string(),
            })?;
        gas_used += match &outcome {
            ExecutionResult::Success { gas, .. }
            | ExecutionResult::Revert { gas, .. }
            | ExecutionResult::Halt { gas, .. } => gas.tx_gas_used(),
        };
    }
    let changes = evm.finalize();

    if gas_used != header.gas_used {
        return Err(ExecError::GasUsed {
            number: header.number,
            got: gas_used,
            want: header.gas_used,
        });
    }

    // Eden does not burn the base fee; ev-reth pays it to the beneficiary with the priority
    // fee. Measured: the first run came out exactly `base_fee * gas_used` short there.
    let base_fee = U256::from(header.base_fee_per_gas.unwrap_or_default());
    let root = post_state_root(
        &witness,
        parent_state_root,
        &changes,
        header.beneficiary,
        base_fee * U256::from(gas_used),
    )?;
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

/// Fold every account and storage change back into the trie and take the new root.
///
/// `fee_credit` is the base fee, which this chain pays to the beneficiary. revm burns it, so
/// it is added back here.
fn post_state_root(
    witness: &Witness,
    pre_root: B256,
    changes: &EvmState,
    beneficiary: Address,
    fee_credit: U256,
) -> Result<B256, ExecError> {
    /// What an address looks like after the block, before it is encoded.
    enum After {
        Gone,
        Present {
            nonce: u64,
            balance: U256,
            storage_root: B256,
            code_hash: B256,
        },
    }

    let mut after: Vec<(Address, After)> = Vec::new();

    for (address, account) in changes {
        if !account.is_touched() {
            continue;
        }

        // Gone, either because it destroyed itself or because EIP-161 sweeps an account that
        // was touched and left empty.
        if account.is_selfdestructed() || account.is_empty() {
            after.push((*address, After::Gone));
            continue;
        }

        let pre = read_account(witness, pre_root, *address)?;
        // A created account starts from an empty storage trie whatever the address held
        // before, which is what makes destroy-then-recreate come out right.
        let storage_base = if account.is_created() {
            EMPTY_ROOT
        } else {
            pre.map(|(_, _, root, _)| root).unwrap_or(EMPTY_ROOT)
        };

        let mut storage_changes: Vec<(B256, Option<Vec<u8>>)> = Vec::new();
        for (slot, value) in &account.storage {
            let present = value.present_value();
            if !account.is_created() && present == value.original_value() {
                continue;
            }
            let slot_key = keccak256(B256::from(*slot).as_slice());
            storage_changes.push((
                slot_key,
                if present.is_zero() {
                    None
                } else {
                    Some(rlp_uint(present))
                },
            ));
        }
        let storage_root = witness.update(storage_base, &storage_changes)?;

        after.push((
            *address,
            After::Present {
                nonce: account.info.nonce,
                balance: account.info.balance,
                storage_root,
                code_hash: account.info.code_hash,
            },
        ));
    }

    if !fee_credit.is_zero() {
        // Normally already here, having taken the priority fee; read from the pre-state only
        // when a block paid none.
        let slot = after.iter_mut().find(|(a, _)| *a == beneficiary);
        match slot {
            Some((_, After::Present { balance, .. })) => *balance += fee_credit,
            Some((address, entry @ After::Gone)) => {
                let (nonce, balance, storage_root, code_hash) =
                    read_account(witness, pre_root, *address)?.unwrap_or((
                        0,
                        U256::ZERO,
                        EMPTY_ROOT,
                        alloy_primitives::KECCAK256_EMPTY,
                    ));
                *entry = After::Present {
                    nonce,
                    balance: balance + fee_credit,
                    storage_root,
                    code_hash,
                };
            }
            None => {
                let (nonce, balance, storage_root, code_hash) =
                    read_account(witness, pre_root, beneficiary)?.unwrap_or((
                        0,
                        U256::ZERO,
                        EMPTY_ROOT,
                        alloy_primitives::KECCAK256_EMPTY,
                    ));
                after.push((
                    beneficiary,
                    After::Present {
                        nonce,
                        balance: balance + fee_credit,
                        storage_root,
                        code_hash,
                    },
                ));
            }
        }
    }

    let account_changes: Vec<(B256, Option<Vec<u8>>)> = after
        .into_iter()
        .map(|(address, entry)| {
            let key = keccak256(address.as_slice());
            match entry {
                After::Gone => (key, None),
                After::Present {
                    nonce,
                    balance,
                    storage_root,
                    code_hash,
                } => (
                    key,
                    Some(encode_account(nonce, balance, storage_root, code_hash)),
                ),
            }
        })
        .collect();

    Ok(witness.update(pre_root, &account_changes)?)
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
