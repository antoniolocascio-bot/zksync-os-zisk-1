//! Build a current-wire `BatchInput` from a prover-neutral zksync-os test-rig
//! state dump (JSON), so EVM test-corpus batches can be executed by the
//! ZiSK REVM guest.
//!
//! The conversion and the native cross-check live in the crate library, which
//! the divergence validator calls as well.
//!
//! Usage:
//!   cargo run --bin dump_to_batchinput -- <dump.json> <out_dir> [--no-validate]
//!
//! Outputs:
//!   <out_dir>/batch_input.bin — `BatchInput` in the `lib::wire` encoding
//!   <out_dir>/input.bin       — ziskemu framing: [len u64 LE][wire bytes][zero pad to 8]
//!
//! `--no-validate` skips the native-reference comparison — for corpus entries
//! where the guest is expected to panic (the artifacts are always written).

use std::path::Path;

use zksync_os_zisk_lib::wire;
use zksync_os_zisk_test_utils::native_check::{CheckEvent, Stage};
use zksync_os_zisk_test_utils::{
    build_batch_input, check_against_native, HeaderHashCheck, NativeCheck, StateDumpBundle,
};

fn frame_for_zisk(wire_bytes: &[u8]) -> Vec<u8> {
    let mut framed = Vec::with_capacity(8 + wire_bytes.len() + 8);
    framed.extend_from_slice(&(wire_bytes.len() as u64).to_le_bytes());
    framed.extend_from_slice(wire_bytes);
    let pad = (8 - (framed.len() % 8)) % 8;
    framed.extend(std::iter::repeat_n(0u8, pad));
    framed
}

/// Report the cross-check, one line per derived value. A leaf count that
/// agrees stays quiet: the tree root printed above it carries the same fact.
fn print_native_check(check: &NativeCheck) {
    for event in &check.events {
        match event {
            CheckEvent::Axis(comparison) => {
                let name = comparison.axis.name();
                if !comparison.agrees() {
                    println!(
                        "FAIL {name}: computed {} != native {}",
                        comparison.computed, comparison.native
                    );
                } else if let zksync_os_zisk_test_utils::native_check::AxisValue::Hash(hash) =
                    comparison.computed
                {
                    println!("PASS {name}: {hash}");
                }
            }
            CheckEvent::Skipped { axes } => {
                let names: Vec<&str> = axes.iter().map(|axis| axis.name()).collect();
                println!("SKIP {}: not in bundle", names.join("/"));
            }
            CheckEvent::Failed(failure) => match failure.stage {
                Stage::TreeUpdate => println!("FAIL tree_update.apply panicked"),
                Stage::Execution => println!("FAIL executor panicked (see message above)"),
            },
        }
    }
}

fn main() {
    let mut no_validate = false;
    let mut header_hash_check = HeaderHashCheck::Armed;
    let mut pos: Vec<String> = Vec::new();
    for a in std::env::args().skip(1) {
        if a == "--no-validate" {
            no_validate = true;
        } else if a == "--no-header-check" {
            header_hash_check = HeaderHashCheck::Skipped;
        } else {
            pos.push(a);
        }
    }
    let [dump_path, out_dir]: [String; 2] = pos.try_into().unwrap_or_else(|_| {
        panic!("usage: dump_to_batchinput <dump.json> <out_dir> [--no-validate]")
    });

    let raw =
        std::fs::read_to_string(&dump_path).unwrap_or_else(|e| panic!("read {dump_path}: {e}"));
    let d: StateDumpBundle = serde_json::from_str(&raw).expect("parse dump json");
    println!(
        "dump: chain_id={} spec_id={} protocol_minor={} block={} txs={} pre_leaves={} post_leaves={}",
        d.chain_id,
        d.spec_id,
        d.protocol_version_minor,
        d.block.number,
        d.txs.len(),
        d.pre.leaves.len(),
        d.post.leaves.len(),
    );

    let conversion = build_batch_input(&d, header_hash_check);
    let bi = conversion.batch_input;
    println!(
        "tracking: {} slot reads, {} account reads, {} bytecodes",
        conversion.stats.slot_reads, conversion.stats.account_reads, conversion.stats.bytecodes
    );
    println!(
        "tree_update: {} updates, {} inserts",
        conversion.stats.tree_updates, conversion.stats.tree_inserts
    );

    let out = Path::new(&out_dir);
    std::fs::create_dir_all(out).expect("create out_dir");
    let data = wire::encode(&bi).expect("wire encode");
    let bin_path = out.join("batch_input.bin");
    std::fs::write(&bin_path, &data).expect("write batch_input.bin");
    let framed = frame_for_zisk(&data);
    let input_path = out.join("input.bin");
    std::fs::write(&input_path, &framed).expect("write input.bin");
    println!(
        "wrote {} ({} bytes) and {} ({} bytes)",
        bin_path.display(),
        data.len(),
        input_path.display(),
        framed.len(),
    );

    if no_validate {
        println!("validation skipped (--no-validate)");
        return;
    }
    let check = check_against_native(&d, &bi);
    print_native_check(&check);
    if !check.agrees() {
        std::process::exit(1);
    }
    println!("ALL CHECKS PASSED");
}
