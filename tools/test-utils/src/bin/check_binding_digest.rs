//! Recompute the aggregated-range binding digest from the per-batch
//! commitments, following guest-aggregator/BINDING_VECTOR.md literally
//! (one keccak over the concatenated untruncated public inputs, one shift,
//! final digest) rather than the aggregator guest's code path — the
//! fixture-session workflow compares this independent value against the
//! aggregated proof's publics[32..64].
//!
//! Usage: check_binding_digest <inner_program_vk> <root_c_vadcop_final>
//!                             <commitment_1> [commitment_2 ...]

use alloy_primitives::B256;
use zksync_os_zisk_lib::hash::keccak256;

fn parse32(arg: &str) -> anyhow::Result<B256> {
    let hex = arg.strip_prefix("0x").unwrap_or(arg);
    anyhow::ensure!(hex.len() == 64, "{arg}: expected 32 hex bytes");
    Ok(hex.parse()?)
}

/// `uint256(word) >> 32`, carried as a 32-byte big-endian word.
fn shr32(word: &B256) -> B256 {
    let mut out = [0u8; 32];
    out[4..].copy_from_slice(&word.as_slice()[..28]);
    B256::from(out)
}

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    anyhow::ensure!(
        args.len() >= 3,
        "usage: check_binding_digest <inner_program_vk> <root_c> <c1> [c2 ...]"
    );
    let inner_vk = parse32(&args[0])?;
    let root_c = parse32(&args[1])?;

    let mut public_inputs = Vec::new();
    for (i, arg) in args[2..].iter().enumerate() {
        let pi = parse32(arg)?;
        println!("public_input[{i}] = {pi}");
        public_inputs.push(pi);
    }

    // A one-batch range is the settlement layer's identity fold: it takes
    // publicInputs[0] verbatim and hashes nothing. Two or more batches are
    // hashed once, over the concatenation of the untruncated inputs.
    let folded = if public_inputs.len() == 1 {
        public_inputs[0]
    } else {
        let mut buf = Vec::with_capacity(public_inputs.len() * 32);
        for pi in &public_inputs {
            buf.extend_from_slice(pi.as_slice());
        }
        keccak256(&buf)
    };
    let range_public_input = shr32(&folded);
    println!("range_public_input = {range_public_input}");

    let mut buf = [0u8; 96];
    buf[..32].copy_from_slice(inner_vk.as_slice());
    buf[32..64].copy_from_slice(root_c.as_slice());
    buf[64..].copy_from_slice(range_public_input.as_slice());
    println!("binding_digest = {}", keccak256(&buf));
    Ok(())
}
