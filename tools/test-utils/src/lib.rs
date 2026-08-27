//! Shared core of the ZiSK test lane: it turns a prover-neutral zksync-os
//! state dump into a current-wire `BatchInput` and checks the guest's
//! commitments against the native reference values the dump carries.
//!
//! Both the corpus reader (`dump_to_batchinput`) and the divergence validator
//! call this code, so the two report on the same comparison.
//!
//! It also holds the witness oracles, which ask the other question about the
//! same batch: does the guest verify that witness, or only execute it?

pub mod dump;
pub mod native_check;
pub mod witness_oracle;

pub use dump::{build_batch_input, Conversion, ConversionStats, HeaderHashCheck, StateDumpBundle};
pub use native_check::{check_against_native, Axis, AxisComparison, CheckEvent, NativeCheck};
pub use witness_oracle::{
    evaluate_all, oracles, statement_digest, Outcome, WitnessOracle, WitnessSoundness,
};

/// The message a caught panic carried. Guest asserts are its rejection
/// mechanism, so the message is the verdict's detail.
pub fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "panicked".to_string()
}
