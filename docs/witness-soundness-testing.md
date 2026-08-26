# Witness-soundness testing

A framework for testing whether the guest **verifies** its witness, as opposed
to whether it **executes** correctly. No mutations ship yet. This document
fixes the property, the seam and the classification, so that a later change adds
cases to a catalogue instead of inventing a harness.

## Why the divergence validator can not answer this

`tools/zisk-divergence-validator` compares the guest against native ZKsync OS on
an honest witness. Every proof it supplies is valid, so a guest that skips proof
verification reads the same values, computes the same transition, and reports a
match. The missing check is invisible however many scenarios run.

Executing correctly and verifying correctly are different properties. The
validator measures the first. This framework measures the second.

## The property

The tempting rule is "the guest must reject a tampered witness". That rule is
wrong, and it reports findings that are not defects. Parts of the witness bind
nothing. `BlockInput::l2_to_l1_logs` is the clearest case: the guest derives its
own log set from the REVM journal and commits that, and it never reads the copy
the server supplies. A correct guest accepts a change there, and a harness built
on the tempting rule calls that a soundness bug.

Unbound is not the same as unimportant. L2→L1 logs carry withdrawals, so what
the guest commits about them is critical. The field is inert **because** the
guest derives the value itself rather than trusting the server — which is the
second-prover rule, not an oversight. An oracle over an inert field therefore
does double duty: it proves the harness can tell inert from bound, and it fails
the day someone binds a critical value to server-supplied data.

The rule that holds:

> For a mutation of the verified part of the witness, with the statement held
> fixed, the guest either **rejects**, or commits the **same** public input as
> the honest run.

A finding is therefore **accepted AND a different commitment**. That is a real
soundness defect: two witnesses for one statement, both accepted, committing to
different state transitions. Unbound data clears itself, because the commitment
decides rather than a judgement about which fields ought to matter.

## Statement and witness

The split is what makes the property meaningful. The **statement** is what the
proof is about; changing it asks for a proof of something else, and a different
commitment is then correct rather than a defect. The **witness** is what the
guest must check against the statement.

| | fields |
|---|---|
| statement — hold fixed | `chain_id`, `spec_id`, `protocol_version_minor`, the pre-state words of `batch_meta`, its chain-config and data-availability words, and per block its number, timestamp, context and `transactions` |
| witness — mutate | `StorageProof` and its `SlotProofEntry` fields, `NeighborProofEntry`, `account_preimages`, `bytecodes`, `block_hashes` |
| unbound — negative control | `l2_to_l1_logs` |

Mutating `tree_root_before` is not a soundness test. It asks the guest to prove
a transition from a different pre-state, and any rule flags it.

A field belongs on the statement side for one of two reasons: it names the
transition, or L1 verifies it outside the proof. The chain configuration and
the data-availability declaration are of the second kind — the guest is not
their verifier, so this harness cannot judge a change to them.
`statement_digest` carries the full set, with the reason for each field.

### Enforce the split, do not document it

The finding condition is "the inputs are the same, an output changed, and the
guest accepted it". The first clause carries as much weight as the rest, so the
harness proves it instead of trusting the oracle to respect a convention.

Project the statement fields to a digest and require the oracle to preserve it:

```rust
/// Hash of every field the commitment is a statement about. An oracle that
/// changes this is asking for a proof of something else.
pub fn statement_digest(input: &BatchInput) -> B256;
```

The gate runs between the oracle and the guest:

| `statement_digest` | meaning |
|---|---|
| unchanged | the comparison is valid; classify the guest's behavior |
| changed | the **oracle** is at fault, not the guest — fail the run as a harness error and name the oracle |

Without the gate, an oracle that touches `transactions` or `tree_root_before`
produces a different commitment for a legitimate reason, and the harness reports
a soundness finding that is not one. That failure grows more likely as the
catalogue grows, and it is the failure that would discredit the whole exercise.

Report the digest alongside every verdict. A reader can then see that the two
runs proved statements about the same thing.

## Classification

```rust
pub enum Outcome {
    /// The guest refused the witness. `assert` records which check fired.
    Rejected { assert: String },
    /// The guest accepted and committed the honest value: the mutated bytes
    /// bind nothing.
    AcceptedIdentical,
    /// The guest accepted and committed a different transition. A finding.
    AcceptedDifferent { honest: B256, mutated: B256 },
}
```

The honest commitment is computed once per case. Each oracle then supplies its
witness, the statement gate above runs, and the guest executes under
`catch_unwind`. A case therefore yields one of four results: the three outcomes
above, or a harness error when the oracle moved the statement.

## The seam already exists

`build_proven_db(input: &BatchInput) -> ProvenDB` is the witness provider. It is
where `StorageProof::verify` runs, and where raw witness becomes a verified
database. The execution core takes the result as a parameter:

```rust
fn run_execution_and_commit(input: &BatchInput, spec_id: ZkSpecId, proven_db: ProvenDB)
```

Two producers already feed it: the collecting path, and
`stream::stream_deserialize_and_build_db`. A witness oracle is a third supplier
at the same boundary, so nothing in the guest needs a new abstraction.

Supply the witness, not a patch:

```rust
pub trait WitnessOracle {
    /// Stable identifier. A finding reproduces from case id and this name.
    fn name(&self) -> &str;
    /// Produce the witness this oracle wants the guest to accept. Returns None
    /// when the case offers no site for it.
    fn witness(&self, honest: &BatchInput) -> Option<BatchInput>;
}
```

The honest oracle is the identity. Each adversarial oracle is one
implementation, and it lives in `tools/test-utils` beside the conversion and the
native check, so one set serves both the validator on a single scenario and a
sweep over the committed corpus.

**Keep the trait host-side.** Adding one to `lib/` would rotate the guest ELF,
and with it the programVK, the verification key hash, the contract pins, the
server configuration and every fixture. The parameterization such a trait would
introduce is already present, so a test-only abstraction does not justify that
cost.

### Why a provider rather than a byte patch

An oracle constructs the witness, so it can tell a **well-formed** lie: recompute
the sibling path so a forged value verifies against a forged root, or splice a
self-consistent subtree. That tests whether the guest binds the proof to the
**pinned** root rather than merely to *a* root — the check most likely to be
written incorrectly.

A byte patch cannot reach that question. It produces garbage, which any
half-present check rejects, and a green result then says nothing about whether
the binding is right.

## Two failure modes of the harness itself

**Rejection for the wrong reason.** A mutation may trip an unrelated assert
before reaching the check under test. The result is green, and the check may not
exist. Recording which assert fired, and reporting the distribution across a
sweep, turns that from an assumption into data. A family whose mutations all
reject at one site has not exercised what its name suggests.

**Aborts are not catchable.** `catch_unwind` handles panics. It does not handle
`SIGABRT`, and this stack has an abort hazard of its own. Run one process per
case rather than per mutation: an abort then costs one case, and the driver
reports the signal as its own outcome class instead of losing the sweep.

## Determinism

Enumerate sites in sorted order at fixed offsets. No random input. A finding
must reproduce from the case identifier and the mutation name alone.

## Scope

This framework covers witness verification. It does not establish that the guest
implements the state transition correctly; the divergence validator covers that,
and only relative to native ZKsync OS. Neither answers whether both
implementations share a misconception. That needs an oracle derived from the
specification rather than from either implementation.
