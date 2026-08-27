# Worked example: expose a soundness bug with a witness oracle

This example writes one `WitnessOracle`, deletes one assertion from the guest,
and reads the finding the tool reports. Every output below is a real run against
`examples/predeployed_bytecode.yaml`.

## The bug

`lib/src/executor/mod.rs` ties the batch's `upgrade_tx_hash` to the transactions
the batch actually carries:

```rust
assert_eq!(
    num_upgrade_txs == 1,
    !meta.upgrade_tx_hash.is_zero(),
    "upgrade_tx_hash must be nonzero iff an Upgrade tx is present \
     (upgrade txs: {num_upgrade_txs}, upgrade_tx_hash: {})",
    meta.upgrade_tx_hash,
);
```

Delete it. That is the whole bug.

`upgrade_tx_hash` is a good place to look for one. It reaches
`batch_output_hash` directly and influences nothing else — not execution, not the
receipts the block header seals, not the tree update, not the interop root. No
other check stands behind it, so this assertion is its entire defence.

## The oracle

The adversary claims the batch carried a protocol-upgrade transaction that it did
not. `tools/test-utils/src/witness_oracle.rs` already carries it in the test
module, where it pins the `Rejected` path:

```rust
struct ForgedUpgradeTxHash;

impl WitnessOracle for ForgedUpgradeTxHash {
    fn name(&self) -> &str {
        "forged_upgrade_tx_hash"
    }

    fn witness(&self, honest: &BatchInput) -> Option<BatchInput> {
        let mut mutated = honest.clone();
        mutated.batch_meta.upgrade_tx_hash = B256::repeat_byte(0xee);
        Some(mutated)
    }
}
```

Move it out of `mod tests`, make it `pub`, and add it to `oracles()`. Guard it
with `if !honest.batch_meta.upgrade_tx_hash.is_zero() { return None; }` once it
runs against arbitrary cases, so a batch that really carries an upgrade reports
no site rather than a forgery of a different shape.

The statement digest does not move: the chain, the tier, the pre-state root, the
blocks and their transactions are untouched. Only witness bytes change, so the
harness judges the guest rather than reporting an error against the oracle.

## Against the guest as shipped

```
    forged_upgrade_tx_hash   rejected: assertion `left == right` failed:
    upgrade_tx_hash must be nonzero iff an Upgrade tx is present
    (upgrade txs: 0, upgrade_tx_hash: 0xeeee…eeee)
```

Exit 0. A rejection is the correct answer and is not a finding.

## Against the guest with the assertion deleted

```
    forged_upgrade_tx_hash   FINDING: accepted, committed 0x6daddd4c…
                             where the honest witness committed 0x28c28e4d…

WITNESS-SOUNDNESS FINDING
  oracle forged_upgrade_tx_hash
```

Exit 1. Two witnesses for one statement, both accepted, committing different
transitions. A prover holding the second one proves a batch that applied a
protocol upgrade the chain never ordered.

## Reproduce it

```bash
cd tools/zisk-divergence-validator
ZKSYNC_USE_CUDA_STUBS=1 cargo build --release
./target/release/zisk-divergence-validator \
    examples/predeployed_bytecode.yaml --witness-soundness
```

Add the oracle, delete the assertion, rebuild, and read the verdict. Restore both
afterwards: `lib/` is byte-frozen, and a change there rotates the guest ELF and
every pin derived from it.

## Choosing the next adversary

An oracle finds a missing check only when it reaches a value the commitment
depends on and no other binding covers. Aiming one at the storage-proof root
binding shows what that means: deleting the root assertion in `build_proven_db`
and forging an account balance produced no finding, because four other bindings
still stood, and each rejected in turn.

| the forgery reached | the guest answered |
|---|---|
| a forged slot value | `account preimage hash mismatch` |
| preimage and proven value, consistent | `after-preimage balance mismatch` |
| an absent header pin, to dodge that | `AtlasV4 block 1 must carry the sealed block_header_hash` |
| after images, with a second assertion deleted | `accepted, identical commitment` — the tree update still came from honest data |
| after images and their tree-update entries | `interop multichain height proof recovers root …` |

Two lessons for a catalogue author. **Rejection for the wrong reason looks
green**: the first two rows never exercised the deleted assertion, and only the
recorded assertion text says so. And **each binding needs its own adversary** —
one forgery hoping to find every missing check will instead be stopped by the
checks that remain.
