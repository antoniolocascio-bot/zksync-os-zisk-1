//! Stamp the report with what was compared: the guest lib revision this
//! binary embeds, and the native ZKsync OS commit its rig comes from.

use std::path::Path;
use std::process::Command;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    println!("cargo:rerun-if-changed=Cargo.lock");
    // A commit or a staged change moves the revision the stamp reports.
    if let Some(git_dir) = git(&manifest_dir, &["rev-parse", "--absolute-git-dir"]) {
        println!("cargo:rerun-if-changed={git_dir}/HEAD");
        println!("cargo:rerun-if-changed={git_dir}/index");
    }
    println!(
        "cargo:rustc-env=GUEST_LIB_REVISION={}",
        guest_lib_revision(&manifest_dir)
    );
    println!(
        "cargo:rustc-env=NATIVE_PRODUCER_COMMIT={}",
        native_producer_commit(&manifest_dir)
    );
}

/// The repository commit the linked `zksync-os-zisk-lib` sources come from.
/// A working tree with modified tracked files reports the commit plus
/// `-modified`, because the binary then holds sources no commit names.
fn guest_lib_revision(manifest_dir: &str) -> String {
    let Some(commit) = git(manifest_dir, &["rev-parse", "HEAD"]) else {
        return "unknown".to_string();
    };
    let clean = Command::new("git")
        .args(["-C", manifest_dir, "diff", "--quiet", "HEAD"])
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if clean {
        commit
    } else {
        format!("{commit}-modified")
    }
}

/// The zksync-os commit the lockfile resolved the pinned tag to.
fn native_producer_commit(manifest_dir: &str) -> String {
    let lock_path = Path::new(manifest_dir).join("Cargo.lock");
    let Ok(lock) = std::fs::read_to_string(&lock_path) else {
        return "unknown".to_string();
    };
    lock.lines()
        .filter(|line| line.contains("git+https://github.com/matter-labs/zksync-os?"))
        .find_map(|line| {
            line.rsplit_once('#')
                .map(|(_, commit)| commit.trim_matches('"').to_string())
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn git(manifest_dir: &str, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(["-C", manifest_dir])
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
