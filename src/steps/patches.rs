use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::shell::run_command;

/// The result of a `quilt push` invocation.
#[derive(Debug)]
pub enum QuiltResult {
    /// All patches applied cleanly.
    AllApplied,
    /// A patch failed to apply.
    PatchFailed {
        patch_name: String,
        conflicted_files: Vec<PathBuf>,
    },
}

/// Try to apply all patches with `quilt push -a`.
///
/// Returns [`QuiltResult::AllApplied`] on success, or
/// [`QuiltResult::PatchFailed`] with details on the first failure.
pub async fn quilt_push_all(repo_dir: &Path) -> Result<QuiltResult> {
    match run_command("quilt", &["push", "-a"], repo_dir, &[]).await {
        Ok(_) => Ok(QuiltResult::AllApplied),
        Err(crate::error::ThermiteError::CommandFailed { stdout, stderr, .. }) => {
            // Finding 3: combine both streams; quilt may write the patch name
            // to either stdout or stderr depending on the version.
            let combined = stdout + &stderr;
            let patch_name = parse_failing_patch_name(&combined);
            Ok(QuiltResult::PatchFailed {
                patch_name,
                conflicted_files: Vec::new(),
            })
        }
        Err(e) => Err(e),
    }
}

/// Force-apply the next failing patch with `quilt push -f --merge`, producing
/// merge-conflict markers in the source tree. Returns the patch name and list
/// of conflicted files parsed from the command output.
pub async fn quilt_push_force_merge(repo_dir: &Path) -> Result<QuiltResult> {
    // quilt exits non-zero when conflicts remain, so we capture both outcomes.
    let combined = match run_command("quilt", &["push", "-f", "--merge"], repo_dir, &[]).await {
        Ok(o) => o.stdout + &o.stderr,
        // Finding 3: capture both streams; conflict details may appear on either.
        Err(crate::error::ThermiteError::CommandFailed { stdout, stderr, .. }) => stdout + &stderr,
        Err(e) => return Err(e),
    };
    let patch_name = parse_failing_patch_name(&combined);
    let conflicted_files = parse_conflicted_files(&combined);
    Ok(QuiltResult::PatchFailed {
        patch_name,
        conflicted_files,
    })
}

/// Refresh the currently applied patch with `quilt refresh`.
pub async fn quilt_refresh(repo_dir: &Path) -> Result<()> {
    run_command("quilt", &["refresh"], repo_dir, &[]).await?;
    Ok(())
}

/// Pop all currently applied patches with `quilt pop -a`.
pub async fn quilt_pop_all(repo_dir: &Path) -> Result<()> {
    // Ignore errors — the quilt series may already be empty.
    let _ = run_command("quilt", &["pop", "-a"], repo_dir, &[]).await;
    Ok(())
}

/// Push all patches and keep the stack fully applied.
pub async fn quilt_push_all_unconditional(repo_dir: &Path) -> Result<()> {
    run_command("quilt", &["push", "-a"], repo_dir, &[]).await?;
    Ok(())
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn parse_failing_patch_name(output: &str) -> String {
    // quilt emits lines like: "Applying patch debian/patches/d-0050-foo.patch"
    // followed by a failure line. Capture the last patch name seen.
    let mut name = String::new();
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("Applying patch ") {
            name = rest.trim().to_owned();
        }
    }
    name
}

fn parse_conflicted_files(output: &str) -> Vec<PathBuf> {
    // quilt --merge emits lines like: "Hunk #1 FAILED at 42 in foo/bar.rs"
    // or "patching file foo/bar.rs"
    output
        .lines()
        .filter_map(|line| {
            if line.contains("FAILED") {
                // Try to extract a file path from the line.
                line.split_whitespace()
                    .find(|w| w.contains('/') || w.ends_with(".rs") || w.ends_with(".c"))
                    .map(|w| PathBuf::from(w.trim_end_matches(':')))
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Finding 3: parse_failing_patch_name extracts the last "Applying patch"
    /// line from quilt output.
    #[test]
    fn parse_failing_patch_name_extracts_last_patch() {
        let output = "Applying patch debian/patches/d-0001-foo.patch\n\
                      patching file src/foo.rs\n\
                      Applying patch debian/patches/d-0002-bar.patch\n\
                      Hunk #1 FAILED at 42.\n";
        let name = parse_failing_patch_name(output);
        assert_eq!(name, "debian/patches/d-0002-bar.patch");
    }

    /// Finding 3: parse_conflicted_files extracts file paths from FAILED lines.
    #[test]
    fn parse_conflicted_files_extracts_paths() {
        let output = "Applying patch debian/patches/d-0010-net.patch\n\
                      patching file src/tools/cargo/net.rs\n\
                      Hunk #1 FAILED at 10 in src/tools/cargo/net.rs.\n\
                      1 out of 1 hunk FAILED -- rejects in src/tools/cargo/net.rs\n";
        let files = parse_conflicted_files(output);
        assert!(!files.is_empty(), "expected at least one conflicted file");
    }

    /// Finding 3: parse_failing_patch_name returns empty string when there are
    /// no "Applying patch" lines (e.g. quilt series is empty).
    #[test]
    fn parse_failing_patch_name_empty_for_no_match() {
        assert_eq!(parse_failing_patch_name("no patch lines here"), "");
    }
}
