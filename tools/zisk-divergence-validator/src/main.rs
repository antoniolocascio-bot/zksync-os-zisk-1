#![allow(incomplete_features)]
#![feature(allocator_api)]

//! Answer one question about a batch: does the ZiSK guest diverge from native
//! ZKsync OS on it?
//!
//! The tool runs the scenario on native ZKsync OS through the test rig, takes
//! the witness and the native reference commitments that run produced, and
//! replays the same batch through the ZiSK guest. It exits 0 when the two
//! agree, 1 when they diverge, and 2 when it could not decide.

mod compiler;
mod harvest;
mod report;
mod runner;
mod scenario;
mod self_check;

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process;

use anyhow::Context;
use zksync_os_zisk_test_utils::StateDumpBundle;

use report::{
    BlockSummary, Divergence, Report, SelfCheckReport, Status, StepResult, Versions, EXIT_ERROR,
};

/// The native ZKsync OS release this tool links, and the producer of the
/// committed EEST corpus; see tools/CORPUS.md.
const NATIVE_PRODUCER: &str = "matter-labs/zksync-os v0.5.0";

const USAGE: &str = "\
Usage: zisk-divergence-validator <scenario.yaml|scenario.json> [--json] [--skip-self-check]
       zisk-divergence-validator --dump <state-dump.json> [--json] [--skip-self-check]

Runs a scenario on native ZKsync OS and on the ZiSK guest, and reports where
they diverge. `--dump` replays a captured state dump instead of running a
scenario.

Exit codes: 0 match, 1 divergence, 2 error.";

/// What the run compares.
enum Source {
    /// A scenario file: contracts, accounts and a block of transactions.
    Scenario(PathBuf),
    /// A captured native state dump.
    Dump(PathBuf),
}

struct Arguments {
    source: Source,
    json: bool,
    skip_self_check: bool,
}

fn parse_arguments() -> anyhow::Result<Arguments> {
    let mut json = false;
    let mut skip_self_check = false;
    let mut dump = false;
    let mut positional: Vec<String> = Vec::new();
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "--json" => json = true,
            "--skip-self-check" => skip_self_check = true,
            "--dump" => dump = true,
            // A help request is neither a verdict nor an error, so the usage
            // text goes to stdout and the process reports success.
            "-h" | "--help" => {
                println!("{USAGE}");
                process::exit(0);
            }
            other if other.starts_with('-') => anyhow::bail!("unknown option '{other}'\n{USAGE}"),
            other => positional.push(other.to_string()),
        }
    }
    let [path]: [String; 1] = positional
        .try_into()
        .map_err(|_| anyhow::anyhow!("expected exactly one input file\n{USAGE}"))?;
    let source = if dump {
        Source::Dump(PathBuf::from(path))
    } else {
        Source::Scenario(PathBuf::from(path))
    };
    Ok(Arguments {
        source,
        json,
        skip_self_check,
    })
}

fn main() {
    let started = std::time::Instant::now();
    let arguments = match parse_arguments() {
        Ok(arguments) => arguments,
        Err(err) => {
            eprintln!("{err:#}");
            process::exit(EXIT_ERROR);
        }
    };

    // A perturbed guest must never produce a verdict, so the self-check runs
    // before anything else and its failure is fatal.
    let self_check = if arguments.skip_self_check {
        SelfCheckReport::Skipped {
            warning: "the corpus self-check was skipped, so this build of the guest lib is \
                      unverified and the verdict below may come from a perturbed guest"
                .to_string(),
        }
    } else {
        match self_check::run() {
            Ok(passed) => passed,
            Err(err) => {
                eprintln!("refusing to report a verdict: {err:#}");
                process::exit(EXIT_ERROR);
            }
        }
    };

    let versions = Versions {
        guest_lib_revision: env!("GUEST_LIB_REVISION").to_string(),
        native_producer: NATIVE_PRODUCER.to_string(),
        native_producer_commit: env!("NATIVE_PRODUCER_COMMIT").to_string(),
        corpus_native_reference_commit: self_check::corpus_native_reference_commit(),
    };

    let report = build_report(&arguments.source, versions, self_check, started);
    if arguments.json {
        match serde_json::to_string_pretty(&report) {
            Ok(json) => println!("{json}"),
            Err(err) => {
                eprintln!("failed to serialize the report: {err}");
                process::exit(EXIT_ERROR);
            }
        }
    } else {
        report.print_human();
    }
    process::exit(report.status.exit_code());
}

fn build_report(
    source: &Source,
    versions: Versions,
    self_check: SelfCheckReport,
    started: std::time::Instant,
) -> Report {
    let mut report = Report {
        status: Status::Match,
        versions,
        self_check,
        block: None,
        witness: None,
        steps: Vec::new(),
        divergence: None,
        error: None,
        axes: Vec::new(),
        skipped_axes: Vec::new(),
        duration_ms: 0,
    };

    let outcome = match source {
        Source::Scenario(path) => run_scenario_file(path),
        Source::Dump(path) => run_dump_file(path),
    };
    match outcome {
        Ok(compared) => {
            report.block = Some(compared.block);
            report.steps = compared.steps;
            report.witness = Some(compared.witness);
            report.skipped_axes = compared
                .check
                .skipped()
                .iter()
                .map(|axis| axis.name().to_string())
                .collect();
            report.axes = compared
                .check
                .events
                .iter()
                .filter_map(|event| match event {
                    zksync_os_zisk_test_utils::CheckEvent::Axis(comparison) => Some(*comparison),
                    _ => None,
                })
                .collect();
            report.divergence = Divergence::from_check(&compared.check);
            if report.divergence.is_some() {
                report.status = Status::Divergence;
            }
        }
        Err(err) => {
            report.status = Status::Error;
            report.error = Some(format!("{err:#}"));
        }
    }
    report.duration_ms = started.elapsed().as_millis();
    report
}

/// One batch compared on both engines.
struct Compared {
    block: BlockSummary,
    steps: Vec<StepResult>,
    check: zksync_os_zisk_test_utils::NativeCheck,
    witness: zksync_os_zisk_test_utils::ConversionStats,
}

fn run_scenario_file(path: &Path) -> anyhow::Result<Compared> {
    let scenario = read_scenario(path)?;
    let scenario_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let artifacts = compiler::compile_contracts(&scenario.contracts, scenario_dir)?;

    // Arm the rig's state-dump hook before the first block runs.
    let harvester = harvest::DumpHarvester::arm()?;
    let outcome = runner::run_scenario(&scenario, &artifacts, &harvester)?;
    Ok(Compared {
        block: outcome.block,
        steps: outcome.steps,
        check: outcome.check,
        witness: outcome.witness,
    })
}

fn run_dump_file(path: &Path) -> anyhow::Result<Compared> {
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let bundle: StateDumpBundle = serde_json::from_str(&raw)
        .with_context(|| format!("failed to parse {} as a state dump", path.display()))?;
    let block = BlockSummary {
        number: bundle.block.number,
        transactions: bundle.txs.len(),
    };
    let (check, witness) = runner::check_bundle(&bundle)?;
    Ok(Compared {
        block,
        steps: Vec::new(),
        check,
        witness,
    })
}

fn read_scenario(path: &Path) -> anyhow::Result<scenario::Scenario> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read scenario file: {}", path.display()))?;
    let is_yaml = path
        .extension()
        .is_some_and(|ext| ext == OsStr::new("yaml") || ext == OsStr::new("yml"));
    if is_yaml {
        serde_yaml::from_str(&content).context("failed to parse scenario YAML")
    } else {
        serde_json::from_str(&content).context("failed to parse scenario JSON")
    }
}
