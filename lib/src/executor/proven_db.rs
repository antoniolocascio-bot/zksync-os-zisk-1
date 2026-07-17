//! Proof-verified state database.
//!
//! Every storage and account read is verified against a merkle proof that
//! recovers the expected state root. Values come FROM the proofs, not from
//! a separate data path.

use std::collections::HashMap;

use revm::database_interface::DBErrorMarker;
use revm::primitives::{Address, B256, Bytes, U256, KECCAK_EMPTY};
use revm::state::{AccountInfo, Bytecode};
use revm::DatabaseRef;

use crate::merkle::{self, StorageProof};
use crate::types::*;

/// Database that verifies every read against a merkle proof.
/// Values are taken FROM the proofs — there is no separate unverified data path.
/// Proof results are cached after first verification to avoid re-hashing.
pub(super) struct ProvenDB {
    /// Pre-verified storage values: flat_key -> value (None = proven non-existing).
    /// All proofs are verified at construction time; reads are pure lookups.
    /// This includes account-property entries at address 0x8003.
    pub(super) verified_storage: HashMap<B256, Option<B256>>,
    /// Merkle-verified account info. Every entry was proven against the tree
    /// root at construction time. None = proven non-existent.
    verified_accounts: HashMap<Address, Option<AccountInfo>>,
    /// Verified bytecodes keyed by hash (keccak256 or blake2s).
    bytecodes: HashMap<B256, Bytecode>,
    /// Block hashes for BLOCKHASH opcode (verified against batch_meta).
    block_hashes: HashMap<u64, B256>,
}

impl ProvenDB {
    /// Assemble a `ProvenDB` from its already-verified component maps. Both the
    /// batch-collecting builder (`build_proven_db`) and the streaming builder
    /// (`super::stream`) funnel through here, so the two paths produce a
    /// byte-identical database.
    pub(super) fn from_parts(
        verified_storage: HashMap<B256, Option<B256>>,
        verified_accounts: HashMap<Address, Option<AccountInfo>>,
        bytecodes: HashMap<B256, Bytecode>,
        block_hashes: HashMap<u64, B256>,
    ) -> Self {
        ProvenDB {
            verified_storage,
            verified_accounts,
            bytecodes,
            block_hashes,
        }
    }
}

#[cfg(test)]
impl ProvenDB {
    /// Borrow all four component maps for A/B equality checks between the
    /// collecting and streaming builders.
    #[allow(clippy::type_complexity)]
    pub(super) fn parts_for_test(
        &self,
    ) -> (
        &HashMap<B256, Option<B256>>,
        &HashMap<Address, Option<AccountInfo>>,
        &HashMap<B256, Bytecode>,
        &HashMap<u64, B256>,
    ) {
        (
            &self.verified_storage,
            &self.verified_accounts,
            &self.bytecodes,
            &self.block_hashes,
        )
    }
}

#[derive(Debug)]
pub(super) struct ProvenDBError(String);

impl core::fmt::Display for ProvenDBError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "ProvenDB error: {}", self.0)
    }
}

impl std::error::Error for ProvenDBError {}
impl DBErrorMarker for ProvenDBError {}

impl DatabaseRef for ProvenDB {
    type Error = ProvenDBError;

    fn basic_ref(&self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        // Fast path: account was pre-verified from account_preimages
        if let Some(proven) = self.verified_accounts.get(&address) {
            return Ok(proven.clone());
        }

        // Slow path: check verified_storage for account-property proof.
        // The server may include a proof without a preimage (for non-existent accounts).
        let addr_bytes: [u8; 20] = address.into_array();
        let flat_key = merkle::derive_account_properties_key(&addr_bytes);
        match self.verified_storage.get(&flat_key) {
            Some(None) => Ok(None), // proven non-existent
            Some(Some(_)) => Err(ProvenDBError(format!(
                "account {address} exists in merkle tree but no preimage in account_preimages"
            ))),
            None => Err(ProvenDBError(format!(
                "no proof for account {address}. The server must provide a merkle proof \
                 (existence or non-existence) for every account REVM accesses."
            ))),
        }
    }

    fn code_by_hash_ref(&self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        if code_hash.is_zero() || code_hash == KECCAK_EMPTY {
            return Ok(Bytecode::default());
        }
        self.bytecodes.get(&code_hash).cloned().ok_or_else(|| {
            ProvenDBError(format!(
                "no bytecode for code_hash {code_hash}. The server must include \
                 all contract bytecodes in the batch."
            ))
        })
    }

    fn storage_ref(&self, address: Address, index: U256) -> Result<U256, Self::Error> {
        let addr_bytes: [u8; 20] = address.into_array();
        let slot = B256::from(index.to_be_bytes::<32>());
        let flat_key = merkle::derive_flat_storage_key(&addr_bytes, &slot);

        match self.verified_storage.get(&flat_key) {
            Some(value) => Ok(value.map(|v| U256::from_be_bytes(v.0)).unwrap_or_default()),
            None => Err(ProvenDBError(format!(
                "no merkle proof for storage read: address={address}, slot={index}, flat_key={flat_key}"
            ))),
        }
    }

    fn block_hash_ref(&self, number: u64) -> Result<B256, Self::Error> {
        Ok(self
            .block_hashes
            .get(&number)
            .copied()
            .unwrap_or_default())
    }
}

// ---------------------------------------------------------------------------
// Shared construction helpers.
//
// The verified-account, bytecode, block-hash, and per-proof verification logic
// is factored out so the batch-collecting `build_proven_db` and the streaming
// builder (`super::stream`, the streaming path) share EXACTLY the same code. Only *how the
// storage proofs are fed in* differs between the two paths (all-resident vs.
// verify-and-drop); everything downstream is identical, which is what makes the
// two paths produce a byte-identical `ProvenDB`.
// ---------------------------------------------------------------------------

/// The pre-state tree root a block's storage proofs must recover: the
/// per-block `expected_tree_root` when set, else the batch pre-state root.
pub(super) fn expected_root_for_block<'a>(
    block: &'a BlockInput,
    meta: &'a BatchMeta,
) -> &'a B256 {
    if !block.expected_tree_root.is_zero() {
        &block.expected_tree_root
    } else {
        &meta.tree_root_before
    }
}

/// Verify one storage proof, returning the recovered root and the proven value
/// (`None` = proven non-existent). Panics with the original message on a
/// malformed proof — identical to the inline check in the pre-refactor code.
pub(super) fn verify_storage_proof(key: &B256, proof: &StorageProof) -> (B256, Option<B256>) {
    proof
        .verify(key)
        .unwrap_or_else(|e| panic!("merkle proof failed for key {key}: {e}"))
}

/// Load batch-level bytecodes. All are keyed by keccak256(code). The server
/// converts blake2s-keyed force-deploy bytecodes to keccak256 at
/// witness-building time.
pub(super) fn load_bytecodes(bytecodes: &[(B256, Vec<u8>)]) -> HashMap<B256, Bytecode> {
    let mut out: HashMap<B256, Bytecode> = HashMap::new();
    for (hash, code) in bytecodes {
        let computed = crate::hash::keccak256(code);
        assert_eq!(
            computed, *hash,
            "bytecode hash mismatch: key={hash}, keccak256={computed}, len={}",
            code.len()
        );
        out.insert(*hash, Bytecode::new_raw(Bytes::copy_from_slice(code)));
    }
    out
}

/// Build the verified-account map from the blocks' account preimages, resolving
/// each against the already-verified storage values and bytecodes. First block
/// to name an account wins (matching the pre-refactor loop).
pub(super) fn build_verified_accounts(
    blocks: &[BlockInput],
    verified_storage: &HashMap<B256, Option<B256>>,
    bytecodes: &HashMap<B256, Bytecode>,
) -> HashMap<Address, Option<AccountInfo>> {
    let mut verified_accounts: HashMap<Address, Option<AccountInfo>> = HashMap::new();

    for block in blocks {
        for (addr, preimage) in &block.account_preimages {
            if verified_accounts.contains_key(addr) {
                continue;
            }
            let addr_bytes: [u8; 20] = addr.into_array();
            let flat_key = merkle::derive_account_properties_key(&addr_bytes);

            let proven_value = verified_storage.get(&flat_key).unwrap_or_else(|| {
                panic!("account_preimage for {addr} but no storage proof at flat_key={flat_key}")
            });

            match proven_value {
                None => {
                    verified_accounts.insert(*addr, None);
                }
                Some(proven_hash) => {
                    let preimage_hash = merkle::AccountProperties::hash(preimage);
                    assert_eq!(
                        *proven_hash, preimage_hash,
                        "account preimage hash mismatch for {addr}: \
                         proven={proven_hash}, computed={preimage_hash}"
                    );

                    let props = merkle::AccountProperties::decode(preimage);
                    let code_hash = if props.observable_bytecode_hash.is_zero() {
                        if props.nonce == 0 && props.balance == [0u8; 32] {
                            B256::ZERO
                        } else {
                            KECCAK_EMPTY
                        }
                    } else {
                        props.observable_bytecode_hash
                    };
                    let code = bytecodes.get(&code_hash).cloned();

                    verified_accounts.insert(
                        *addr,
                        Some(AccountInfo {
                            nonce: props.nonce,
                            balance: U256::from_be_bytes(props.balance),
                            code_hash,
                            code,
                            account_id: None,
                        }),
                    );
                }
            }
        }
    }

    verified_accounts
}

/// Build the BLOCKHASH ring from the blocks' witnessed hashes, cross-checking
/// against `batch_meta.previous_block_hashes` exactly as the pre-refactor loop.
pub(super) fn build_block_hashes(blocks: &[BlockInput], meta: &BatchMeta) -> HashMap<u64, B256> {
    let mut block_hashes: HashMap<u64, B256> = HashMap::new();

    for block in blocks {
        // Verify block hashes against batch_meta.previous_block_hashes.
        //
        // previous_block_hashes is the 255-entry ring preceding the LAST block
        // of the batch (it feeds block_hashes_blake_after). Index j = hash of
        // block (last_block - 255 + j). We use the last block's number, not
        // `block_number_before` (which is first_block - 1), so multi-block
        // batches index into the ring correctly.
        if let Some(last_block) = blocks.last() {
            let last_num = last_block.number;
            if last_num >= 255 {
                let oldest_available = last_num - 255;
                for &(num, hash) in &block.block_hashes {
                    if num >= oldest_available && num < last_num {
                        let idx = (num - oldest_available) as usize;
                        if idx < meta.previous_block_hashes.len() {
                            let verified_hash = meta.previous_block_hashes[idx];
                            if !verified_hash.is_zero() {
                                assert_eq!(
                                    hash, verified_hash,
                                    "block hash mismatch for block {num}: input={hash}, verified={verified_hash}"
                                );
                            }
                        }
                    }
                    block_hashes.insert(num, hash);
                }
            } else {
                for &(num, hash) in &block.block_hashes {
                    block_hashes.insert(num, hash);
                }
            }
        } else {
            for &(num, hash) in &block.block_hashes {
                block_hashes.insert(num, hash);
            }
        }
    }

    block_hashes
}

/// Build a ProvenDB for the entire batch (batch-collecting path).
///
/// All merkle proofs from all blocks are verified at construction time and
/// their values are stored in flat maps. Each block's proofs are verified
/// against that block's expected tree root. This path holds every proof (and
/// therefore every merkle sibling) resident at once; the streaming path in
/// `super::stream` (the streaming path) verifies and drops each proof instead, but reuses
/// the same helpers below so the resulting `ProvenDB` is byte-identical.
pub(super) fn build_proven_db(input: &BatchInput) -> ProvenDB {
    let meta = &input.batch_meta;

    let bytecodes = load_bytecodes(&input.bytecodes);

    let mut verified_storage: HashMap<B256, Option<B256>> = HashMap::new();
    for block in &input.blocks {
        let expected_root = expected_root_for_block(block, meta);

        // Verify all merkle proofs and extract values FROM the proofs.
        for (key, proof) in &block.storage_proofs {
            let (root, value) = verify_storage_proof(key, proof);
            assert_eq!(
                root, *expected_root,
                "proof for {key} recovers root {root}, expected {expected_root}"
            );

            // First block's proof wins — later blocks may have the same key
            // against a different root (after writes), but the pre-state value
            // is what matters for the ProvenDB. Intra-batch updates go through CacheDB.
            verified_storage.entry(*key).or_insert(value);
        }
    }

    let verified_accounts = build_verified_accounts(&input.blocks, &verified_storage, &bytecodes);
    let block_hashes = build_block_hashes(&input.blocks, meta);

    ProvenDB::from_parts(verified_storage, verified_accounts, bytecodes, block_hashes)
}
