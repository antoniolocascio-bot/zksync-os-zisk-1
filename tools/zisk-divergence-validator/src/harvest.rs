//! The seam between the two engines: the native rig's state-dump hook writes
//! one JSON bundle per executed block, and that bundle is what the guest leg
//! consumes.
//!
//! The hook is the producer the committed EEST corpus is generated with, so
//! calling it here keeps this tool and the corpus lane on one conversion.

use std::path::Path;

use anyhow::{bail, Context};
use zksync_os_zisk_test_utils::StateDumpBundle;

/// The rig enables its state-dump hook when this variable names a directory.
const DUMP_DIR_VAR: &str = "ZKOS_STATE_DUMP_DIR";

/// A private directory the rig writes this run's bundles into.
pub struct DumpHarvester {
    dir: tempfile::TempDir,
}

impl DumpHarvester {
    /// Arm the rig's state-dump hook for this process. Call before the first
    /// block runs: the hook reads the variable at the start of every block.
    pub fn arm() -> anyhow::Result<Self> {
        let dir = tempfile::tempdir().context("failed to create the state-dump directory")?;
        std::env::set_var(DUMP_DIR_VAR, dir.path());
        Ok(Self { dir })
    }

    /// The bundle the rig wrote for `block_number`.
    ///
    /// A missing bundle is a hard error: the hook writes nothing when it is
    /// disabled or when the block failed to execute, and an empty comparison
    /// must never read as agreement.
    pub fn take(&self, block_number: u64) -> anyhow::Result<StateDumpBundle> {
        let written = self.written_bundles()?;
        let suffix = format!("-{block_number}.json");
        let matching: Vec<&std::path::PathBuf> = written
            .iter()
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("dump-") && name.ends_with(&suffix))
            })
            .collect();

        let bundle_path = match matching.as_slice() {
            [only] => *only,
            [] => bail!(
                "the native rig wrote no state dump for block {block_number} ({} bundles in {}); \
                 the block produced no dump, so there is nothing to compare",
                written.len(),
                self.dir.path().display()
            ),
            many => bail!(
                "the native rig wrote {} state dumps for block {block_number}; \
                 the tool cannot tell which one the scenario executed",
                many.len()
            ),
        };

        let raw = std::fs::read_to_string(bundle_path)
            .with_context(|| format!("failed to read {}", bundle_path.display()))?;
        let bundle: StateDumpBundle = serde_json::from_str(&raw)
            .with_context(|| format!("failed to parse {}", bundle_path.display()))?;
        if bundle.block.number != block_number {
            bail!(
                "{} holds block {} but the scenario executed block {block_number}",
                bundle_path.display(),
                bundle.block.number
            );
        }
        Ok(bundle)
    }

    fn written_bundles(&self) -> anyhow::Result<Vec<std::path::PathBuf>> {
        read_json_files(self.dir.path())
            .with_context(|| format!("failed to read {}", self.dir.path().display()))
    }
}

fn read_json_files(dir: &Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            files.push(path);
        }
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A block that wrote no bundle must be a hard error. The hook writes
    /// nothing when it is disabled or when the block failed, and an empty
    /// comparison must never read as agreement.
    #[test]
    fn a_missing_bundle_is_an_error() {
        let harvester = DumpHarvester::arm().expect("arm the state-dump hook");
        let Err(err) = harvester.take(1) else {
            panic!("no block ran, so there is no bundle");
        };
        assert!(
            err.to_string().contains("wrote no state dump for block 1"),
            "unexpected message: {err}"
        );
    }
}
