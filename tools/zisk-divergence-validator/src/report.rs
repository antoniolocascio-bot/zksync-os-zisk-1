//! The verdict, in a form an operator and a script can both read.

use serde::Serialize;
use zksync_os_zisk_test_utils::native_check::{AxisComparison, NativeCheck, Stage};
use zksync_os_zisk_test_utils::ConversionStats;

pub const EXIT_MATCH: i32 = 0;
pub const EXIT_DIVERGENCE: i32 = 1;
pub const EXIT_ERROR: i32 = 2;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Match,
    Divergence,
    Error,
}

impl Status {
    pub fn exit_code(self) -> i32 {
        match self {
            Status::Match => EXIT_MATCH,
            Status::Divergence => EXIT_DIVERGENCE,
            Status::Error => EXIT_ERROR,
        }
    }
}

/// What the run compared. A guest pinned against a different protocol
/// revision than the native producer reports a false divergence, so the
/// operator sees both revisions with every verdict.
#[derive(Clone, Debug, Serialize)]
pub struct Versions {
    pub guest_lib_revision: String,
    pub native_producer: String,
    pub native_producer_commit: String,
    pub corpus_native_reference_commit: String,
}

/// The pinned corpus case the tool replays before it reports a verdict.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SelfCheckReport {
    Passed {
        case: String,
        axes_checked: usize,
        duration_ms: u128,
    },
    Skipped {
        warning: String,
    },
}

#[derive(Clone, Debug, Serialize)]
pub struct BlockSummary {
    pub number: u64,
    pub transactions: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct StepResult {
    pub description: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gas_used: Option<u64>,
}

/// Where the two implementations parted.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DivergenceClass {
    /// The guest could not reproduce the block at all. Its own assertions —
    /// the block header hash, the storage proofs, the account after-images —
    /// reject before any commitment is derived.
    Execution,
    /// The guest reproduced the block but committed to a different value.
    Commitment,
}

#[derive(Clone, Debug, Serialize)]
pub struct Divergence {
    pub class: DivergenceClass,
    /// The first commitment sub-component that differs, in derivation order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub axis: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub computed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native: Option<String>,
    pub detail: String,
}

impl Divergence {
    /// The first divergence the cross-check found, walking the axes in the
    /// order the guest derives them.
    pub fn from_check(check: &NativeCheck) -> Option<Self> {
        if let Some(failure) = check.first_failure() {
            let stage = match failure.stage {
                Stage::TreeUpdate => "applying the tree update",
                Stage::Execution => "executing the transactions",
            };
            return Some(Divergence {
                class: DivergenceClass::Execution,
                axis: None,
                computed: None,
                native: None,
                detail: format!(
                    "the guest rejected the batch while {stage}: {}",
                    failure.message
                ),
            });
        }
        check.first_mismatch().map(|mismatch| Divergence {
            class: DivergenceClass::Commitment,
            axis: Some(mismatch.axis.name().to_string()),
            computed: Some(mismatch.computed.to_string()),
            native: Some(mismatch.native.to_string()),
            detail: format!(
                "the guest and native agree up to {}, and differ there",
                mismatch.axis.name()
            ),
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub status: Status,
    pub versions: Versions,
    pub self_check: SelfCheckReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block: Option<BlockSummary>,
    /// Sizes of the witness the guest authenticated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub witness: Option<ConversionStats>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<StepResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub divergence: Option<Divergence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub axes: Vec<AxisComparison>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skipped_axes: Vec<String>,
    pub duration_ms: u128,
}

impl Report {
    /// The summary an operator reads first: what was compared, then where the
    /// two implementations parted.
    pub fn print_human(&self) {
        println!("ZiSK divergence validator");
        println!("  guest lib        {}", self.versions.guest_lib_revision);
        println!(
            "  native producer  {} ({})",
            self.versions.native_producer, self.versions.native_producer_commit
        );
        match &self.self_check {
            SelfCheckReport::Passed {
                case,
                axes_checked,
                duration_ms,
            } => println!(
                "  self-check       corpus case {case} reproduced, {axes_checked} axes, {duration_ms} ms"
            ),
            SelfCheckReport::Skipped { warning } => println!("  self-check       WARNING: {warning}"),
        }
        if let Some(block) = &self.block {
            println!(
                "  block            {}, {} transactions",
                block.number, block.transactions
            );
        }
        for (index, step) in self.steps.iter().enumerate() {
            let outcome = if step.success { "ok" } else { "failed" };
            match step.gas_used {
                Some(gas) => println!("    [{index}] {} — {outcome}, gas {gas}", step.description),
                None => println!("    [{index}] {} — {outcome}", step.description),
            }
        }
        if let Some(witness) = &self.witness {
            println!(
                "  witness          {} slot proofs, {} accounts, {} bytecodes, {} tree writes",
                witness.slot_reads,
                witness.account_reads,
                witness.bytecodes,
                witness.tree_updates + witness.tree_inserts
            );
        }
        if !self.skipped_axes.is_empty() {
            println!("  not compared     {}", self.skipped_axes.join(", "));
        }
        println!("  elapsed          {} ms", self.duration_ms);
        println!();
        match (&self.divergence, &self.error) {
            (Some(divergence), _) => {
                println!("DIVERGENCE ({:?})", divergence.class);
                if let Some(axis) = &divergence.axis {
                    println!("  first mismatching axis: {axis}");
                    println!(
                        "    guest  {}",
                        divergence.computed.as_deref().unwrap_or("-")
                    );
                    println!("    native {}", divergence.native.as_deref().unwrap_or("-"));
                }
                println!("  {}", divergence.detail);
            }
            (None, Some(error)) => println!("ERROR\n  {error}"),
            (None, None) => println!(
                "MATCH — the guest reproduced every native reference value ({} axes)",
                self.axes.len()
            ),
        }
    }
}
