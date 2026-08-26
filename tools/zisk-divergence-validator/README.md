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
read whether the guest reproduces native ZKsync OS.

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
```

The crate is its own workspace, so run cargo from this directory.

Scenario files are YAML (`.yaml`/`.yml`) or JSON (`.json`).

### Exit codes

| Code | Meaning |
|------|---------|
| 0 | Match — the guest reproduced every native reference value |
| 1 | Divergence found |
| 2 | Error (bad input, conversion failure, self-check failure) |

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
