# ZiSK divergence validator

A command-line tool that answers one question about a batch: does the ZiSK
guest diverge from native ZKsync OS on it?

The tool takes a scenario that describes contracts, accounts and a block of
transactions. It runs that block on native ZKsync OS through the test rig,
takes the witness and the native reference commitments the run produced, and
replays the same batch through the ZiSK guest. It compares the two and names
the first value they disagree on. One case takes milliseconds. The tool
generates no proof.

Use it to triage an incident: write the reported scenario, run the tool, and
read whether the guest reproduces native ZKsync OS. Its exit codes and JSON
report also make it the oracle for an automated search that generates scenarios
and iterates; see "Driving it from an automated search".

## Prerequisites

- [Foundry](https://getfoundry.sh/) (`forge`) on PATH, for scenarios that hold
  Solidity sources. Scenarios that use `bytecode` or `send_raw` alone need it
  only if they also declare a Solidity contract.
- The `rust-toolchain.toml` in this directory selects the toolchain. The tool
  links native ZKsync OS, which pins that nightly.
- `ZKSYNC_USE_CUDA_STUBS=1` in the environment. Native ZKsync OS pulls
  `era_cudart_sys`, whose build script stops with "Failed to determine the CUDA
  Toolkit version" on a machine that has no CUDA toolkit.

## Usage

```bash
# Build and run from the crate directory
cd tools/zisk-divergence-validator
cargo build --release
./target/release/zisk-divergence-validator examples/simple_storage.yaml

# Machine-readable report
./target/release/zisk-divergence-validator examples/simple_storage.yaml --json

# Replay a state dump captured from a native run
./target/release/zisk-divergence-validator --dump path/to/dump.json

# Also ask whether the guest verifies the witness of this case
./target/release/zisk-divergence-validator examples/simple_storage.yaml --witness-soundness
```

The crate is its own workspace, so run cargo from this directory.

Scenario files are YAML (`.yaml`/`.yml`) or JSON (`.json`).

### Exit codes

| Code | Meaning |
|------|---------|
| 0 | Match — the guest reproduced every native reference value |
| 1 | Divergence found, or a witness-soundness finding |
| 2 | Error (bad input, conversion failure, self-check failure, harness error) |

## Scenario format

```yaml
contracts:
  MyContract:
    source: |
      // SPDX-License-Identifier: MIT
      pragma solidity ^0.8.20;

      contract MyContract {
          uint256 public x;
          function set(uint256 v) public { x = v; }
      }

accounts:
  alice:
    balance: "1000000000000000000"

block:
  basefee: 1000
  gas_limit: 30000000
  timestamp: 1700000000

steps:
  - type: deploy
    contract: MyContract
    from: alice
    gas: 5000000

  - type: call
    to: "$deployed:0"
    from: alice
    function: "set(uint256)"
    args: [42]
    gas: 1000000
```

### Contracts

Each entry maps a contract name to its definition.

| Fields | Description |
|--------|-------------|
| `source` | Inline Solidity source code (compiled through `forge build`) |
| `file` | Path to a `.sol` file, relative to the scenario file |
| `bytecode` + `address` | Raw runtime bytecode predeployed at the given address |

For a Solidity contract, the name must equal the Solidity `contract` name.

### Accounts

Named accounts with a pre-funded `balance` in wei, decimal or `0x`-prefixed
hex. A `from` name that the steps reference gets a default balance that covers
gas.

### Block

| Field | Default |
|-------|---------|
| `basefee` | 1000 |
| `gas_limit` | 30000000 |
| `timestamp` | 1700000000 |

### Steps

All steps run as transactions in one block.

**deploy** — deploys a compiled contract through CREATE. Fields: `contract`,
`from`, optional `args`, `gas`, `value`.

**call** — calls a deployed contract. Fields: `to`, `from`, `function`,
optional `args`, `gas`, `value`.

**send_raw** — sends raw bytecode or calldata. Fields: `from`, optional `to`,
`data`, `gas`, `value`. Omit `to` for CREATE.

The `to` field accepts `"$deployed:N"` (the address of the Nth deploy or
CREATE step, counted from 0), a contract name, a named account, or a hex
address.

## What the tool compares

The guest re-derives the batch from the witness alone. The tool walks the
values it commits to in the order the guest derives them, and stops the report
at the first difference:

1. `tree_root_after` and `leaf_count_after` — the flat-storage tree update
2. `state_before` and `state_after` — the chain state commitments
3. `batch_output_hash`, `chain_config_hash` and `batch_public_input` — the
   batch commitments

The guest holds its own assertions ahead of those values: the canonical block
header hash, the storage proofs, and the per-account after-images. An
assertion that rejects is reported as an execution divergence, with the
guest's message. A value that differs is reported as a commitment divergence,
with the axis, the guest's value and the native value. The distinction tells
the operator whether the two implementations executed the block differently or
encoded the same execution differently.

## Witness soundness

`--witness-soundness` asks the other question about the same case: does the
guest verify its witness, or only execute it? Every proof an honest run
supplies is valid, so a guest that skips a check still reports a match.

The tool runs each registered witness oracle over the batch it just executed.
An oracle supplies the witness it wants the guest to accept. The rule a correct
guest obeys:

> For a mutation of the verified part of the witness, with the statement held
> fixed, the guest either rejects the witness, or commits the same public input
> as the honest run.

A finding is therefore an accepted witness AND a different commitment: two
witnesses for one statement.

The statement is what the proof is about — the chain, the state transition
function tier, the pre-state, the chain configuration, and the blocks with
their transactions. The harness projects those fields to a digest and requires
every oracle to preserve it. An oracle that moves the digest asks for a proof
of something else, so the run reports a harness error, names that oracle, and
exits 2 rather than judging the guest. Each report prints the digest above the
verdicts it belongs to.

Two oracles ship. `honest` is the identity, and its verdict pins the harness
itself. `unbound_l2_to_l1_logs` appends a fabricated L2→L1 log record to the
witness.

That second one earns its place twice over. It shows that the harness separates
unbound data from bound data, rather than reporting every change as a finding.
It also guards a soundness property of the guest. L2→L1 logs carry withdrawals,
so the value they commit to is critical — and the guest binds the set it derives
itself from the REVM journal, never the copy the server supplies. A change that
made the guest read the server's copy would move a critical value onto
server-supplied data, and this oracle turns that verdict from
`accepted, identical commitment` into a finding.

```
  witness oracles  statement 0x8c7b6f93…, honest commitment 0x1179334c…
    honest                   accepted, identical commitment
    unbound_l2_to_l1_logs    accepted, identical commitment
```

`examples/witness-soundness/README.md` walks the framework end to end: it writes
an adversary, deletes a real binding from the guest, and records what the tool
answered at each step. It doubles as a map of the guest's witness bindings.

The oracles live in `tools/test-utils`, beside the conversion and the native
cross-check, so one set serves this tool and a corpus sweep. A new case is one
implementation of `WitnessOracle` and one line in `witness_oracle::oracles()`.
`docs/witness-soundness-testing.md` holds the design.

## Driving it from an automated search

The tool is built to be the oracle in an automated loop: a scenario goes in, an
exit code and a JSON report come out, and a bytecode scenario takes about 6 ms,
so one core answers on the order of 160 cases a second. The first mismatching
axis gives a generator something to steer on, rather than a yes or no.

The self-check earns its keep here more than anywhere. A campaign that collected
findings from a subtly mis-built guest would poison every result it produced, and
the operator would only learn that after triaging them. Refusing to report beats
reporting wrongly.

Four things shape a loop around it.

**Compilation dominates, not execution.** A Solidity scenario costs about 410 ms,
almost all of it `forge build`, against 6 ms for one that carries bytecode. A
search should emit bytecode directly and keep `forge` for the minimized
reproducer at the end.

**Gas divergence is invisible as such.** The guest receives the native
per-transaction gas through `gas_used_override`, which the corpus lane
established. A pure gas-accounting difference is therefore not an axis of its
own; it surfaces only when it perturbs the block header hash. A clean sweep says
nothing about that class.

**Any exit outside 0, 1 and 2 is a finding to keep.** `catch_unwind` turns a
guest panic into a verdict, but it does not catch `SIGABRT`, and this stack has
an abort hazard of its own. A harness that retries on an unexpected exit throws
away the most interesting cases.

**The generator is the part that is missing.** This tool checks; it does not
propose. The committed EEST corpus already covers 10,605 cases with 16 recorded
non-matches, so a generator that wanders into that space spends its budget
rediscovering waivers.

Two limits to fix before a campaign rather than during one. A scenario run builds
its signers with `PrivateKeySigner::random()`, so the transactions, the statement
digest and the commitment differ on every run — a finding from the scenario path
cannot be reproduced from a case identifier, while the `--dump` path can. And the
self-check runs per invocation, which is 1 ms against a 6 ms case; a loop driving
thousands wants a batch mode that checks once per process and then streams
scenarios, which also removes the process-spawn cost.

## Self-check

This tool is the one place where native ZKsync OS and the ZiSK guest lib
resolve into one cargo graph, so cargo unifies the features of the crates they
share, and the toolchain differs from the one the shipping crates use. Before
the tool reports any verdict, it replays one case from the committed EEST
corpus (`tools/eest-corpus/`) and confirms that this build of the guest
reproduces the committed native reference values. That makes the build's
equivalence to the shipping configuration an observed fact on every run. A
self-check that fails makes the tool exit 2 and report no verdict.

`--skip-self-check` runs without that guard and prints a warning into both the
human and the JSON report.

## Versions

Every report names the guest lib revision it embeds and the native ZKsync OS
commit its rig comes from. A guest compared against a native producer from a
different protocol revision reports a false divergence, so read those two
lines first when a verdict surprises you.

The tool also reports the native producer commit the committed corpus records,
which lets the operator see whether the tool and the corpus baseline compare
against the same native release.

## Design notes

- All steps run in one block.
- The scenario runs with unlimited native resources (`native_price == 0`), so
  ZKsync OS gas accounting follows standard EVM rules.
- The guest leg reads the witness from the rig's own state-dump hook, which is
  the producer that generates the committed EEST corpus. The tool arms that
  hook for a private directory it owns, and it treats a missing bundle as an
  error, so an empty comparison never reads as agreement.
- The conversion and the native cross-check live in
  `tools/test-utils`, which the corpus reader `dump_to_batchinput` also calls.
  One comparison serves both lanes.
