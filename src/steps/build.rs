use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::shell::run_command;

/// Build a Debian source package with `dpkg-buildpackage -S -I -i -nc -d -sa`.
pub async fn run_dpkg_buildpackage_source(repo_dir: &Path) -> Result<()> {
    run_command(
        "dpkg-buildpackage",
        &["-S", "-I", "-i", "-nc", "-d", "-sa"],
        repo_dir,
        &[],
    )
    .await?;
    Ok(())
}

/// Clean build artifacts from the parent directory and the local `debian/`
/// files.
pub async fn clean_build_artifacts(parent_dir: &Path, repo_dir: &Path) -> Result<()> {
    // Remove generated Debian build output files from the parent directory.
    for suffix in &["debian.tar.xz", "dsc", "buildinfo", "changes", "ppa.upload"] {
        let pattern = format!("*.{suffix}");
        let _ = run_command(
            "bash",
            &["-c", &format!("rm -vf {pattern}")],
            parent_dir,
            &[],
        )
        .await;
    }

    // Remove debian/files and the quilt state directory.
    let debian_files = repo_dir.join("debian/files");
    if debian_files.exists() {
        std::fs::remove_file(&debian_files)?;
    }
    let pc_dir = repo_dir.join(".pc");
    if pc_dir.exists() {
        std::fs::remove_dir_all(&pc_dir)?;
    }
    Ok(())
}

/// The result of an `sbuild` invocation.
#[derive(Debug)]
pub enum SbuildResult {
    Success,
    Failure {
        /// Path to the build log produced by sbuild, or `None` when sbuild
        /// failed before it could create one (e.g. during source packaging).
        log_path: Option<PathBuf>,
        /// Captured sbuild stdout — always present, even on early failure.
        stdout: String,
        /// Captured sbuild stderr — always present, even on early failure.
        stderr: String,
    },
}

/// Run `sbuild -Ad <release>` to build the package in a clean chroot.
///
/// `extra_args` can include flags like `--extra-repository=...` for PPA
/// bootstrapping.
pub async fn run_sbuild(
    repo_dir: &Path,
    release: &str,
    extra_args: &[String],
) -> Result<SbuildResult> {
    let dist_arg = format!("-Ad{release}");
    let mut args = vec![dist_arg.as_str()];
    let extra_refs: Vec<&str> = extra_args.iter().map(|s| s.as_str()).collect();
    args.extend_from_slice(&extra_refs);

    match run_command("sbuild", &args, repo_dir, &[]).await {
        Ok(_) => Ok(SbuildResult::Success),
        Err(crate::error::ThermiteError::CommandFailed { stdout, stderr, .. }) => {
            // Find the most recent .build log sbuild may have produced.
            let parent = repo_dir.parent().unwrap_or(repo_dir);
            let log_path = find_latest_build_log(parent);

            // When sbuild fails before opening its build log (e.g. during
            // `dpkg-source` packaging), there is no `.build` file anywhere.
            // Persist the captured stdout/stderr to a real file so the user
            // always has something to inspect instead of a fictional path.
            let log_path = log_path.or_else(|| {
                let fallback = parent.join("sbuild-failure.log");
                let mut content = String::new();
                if !stdout.is_empty() {
                    content.push_str("=== sbuild stdout ===\n");
                    content.push_str(&stdout);
                    if !content.ends_with('\n') {
                        content.push('\n');
                    }
                }
                if !stderr.is_empty() {
                    content.push_str("=== sbuild stderr ===\n");
                    content.push_str(&stderr);
                    if !content.ends_with('\n') {
                        content.push('\n');
                    }
                }
                if content.is_empty() {
                    content.push_str("(sbuild produced no output)\n");
                }
                // Best-effort write: if it fails, we still return the
                // captured text via the Failure variant so callers can
                // surface it.
                let _ = std::fs::write(&fallback, &content);
                Some(fallback)
            });

            Ok(SbuildResult::Failure {
                log_path,
                stdout,
                stderr,
            })
        }
        Err(e) => Err(e),
    }
}

/// Extract the `stdout ----` sections from an sbuild log, which contain
/// individual test failure output.
pub fn extract_test_failures(sbuild_log: &Path) -> Result<Vec<String>> {
    let content = std::fs::read_to_string(sbuild_log)?;
    let mut sections = Vec::new();
    let mut current: Option<String> = None;

    for line in content.lines() {
        if line.contains("stdout ----") {
            current = Some(line.to_owned() + "\n");
        } else if let Some(ref mut buf) = current {
            buf.push_str(line);
            buf.push('\n');
            // End of section — typically a blank line or dashes.
            if line.trim().is_empty() || line.starts_with("----") {
                sections.push(buf.clone());
                current = None;
            }
        }
    }
    Ok(sections)
}

fn find_latest_build_log(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".build"))
        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
        .map(|e| e.path())
}

/// Run `quilt pop -a` to unapply all quilt patches from the working tree.
///
/// This must be called before `clean_build_artifacts` and before
/// `dpkg-buildpackage -S` so that the source package captures patches as
/// quilt series rather than as inline diffs against modified files.
///
/// Errors are silently ignored: the command exits non-zero when no patches
/// are applied, which is the common case.
pub async fn quilt_pop_all(repo_dir: &Path) -> Result<()> {
    // Ignore errors — `quilt pop -a` exits 1 when no patches are applied.
    let _ = run_command("quilt", &["pop", "-a"], repo_dir, &[]).await;
    Ok(())
}

/// Remove the autopkgtest self-build test block from `debian/tests/control`.
///
/// The block to remove is exactly:
/// ```text
/// Test-Command: ./debian/rules build RUST_TEST_SELFBUILD=1
/// Depends: @, @builddeps@
/// Restrictions: rw-build-tree, allow-stderr
/// ```
///
/// If the block is not present (e.g. already removed), the function succeeds
/// without modifying the file.
pub fn disable_self_build_test(repo_dir: &Path) -> Result<()> {
    let tests_control = repo_dir.join("debian/tests/control");
    if !tests_control.exists() {
        return Ok(());
    }

    let content = std::fs::read_to_string(&tests_control)?;

    // The self-build block starts with this prefix (may have varying spacing).
    const SELFBUILD_MARKER: &str = "Test-Command: ./debian/rules build RUST_TEST_SELFBUILD=1";

    if !content.contains(SELFBUILD_MARKER) {
        // Already absent — nothing to do.
        return Ok(());
    }

    // Remove the three-line block.  We iterate over lines and skip the
    // SELFBUILD_MARKER line plus the two lines immediately following it
    // (Depends and Restrictions).
    let mut out_lines: Vec<&str> = Vec::new();
    let mut skip_remaining: u32 = 0;
    for line in content.lines() {
        if skip_remaining > 0 {
            skip_remaining -= 1;
            continue;
        }
        if line
            .trim_start()
            .starts_with("Test-Command: ./debian/rules build RUST_TEST_SELFBUILD=1")
        {
            // Skip this line and the next two.
            skip_remaining = 2;
            continue;
        }
        out_lines.push(line);
    }

    // Remove any leading blank lines that were left at the top of the file
    // after the block was removed.
    while out_lines
        .first()
        .map(|l| l.trim().is_empty())
        .unwrap_or(false)
    {
        out_lines.remove(0);
    }

    let new_content = out_lines.join("\n") + "\n";
    std::fs::write(&tests_control, new_content)?;
    Ok(())
}

#[cfg(test)]
mod disable_self_build_tests {
    use super::*;
    use std::fs;

    fn write_temp_tests_control_manual(content: &str) -> PathBuf {
        let base = std::env::temp_dir().join(format!(
            "thermite-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
        ));
        let tests_dir = base.join("debian/tests");
        fs::create_dir_all(&tests_dir).expect("create dirs");
        let control = tests_dir.join("control");
        fs::write(&control, content).expect("write");
        base
    }

    #[test]
    fn removes_self_build_block() {
        let control_content = "\
Test-Command: ./debian/rules build RUST_TEST_SELFBUILD=1
Depends: @, @builddeps@
Restrictions: rw-build-tree, allow-stderr

Test-Command: cargo test
Depends: @
Restrictions: allow-stderr
";
        let repo_dir = write_temp_tests_control_manual(control_content);
        disable_self_build_test(&repo_dir).expect("disable_self_build_test");
        let result =
            fs::read_to_string(repo_dir.join("debian/tests/control")).expect("read result");
        assert!(!result.contains("RUST_TEST_SELFBUILD"));
        assert!(result.contains("cargo test"));
        let _ = fs::remove_dir_all(&repo_dir);
    }

    #[test]
    fn no_op_when_block_absent() {
        let control_content = "\
Test-Command: cargo test
Depends: @
Restrictions: allow-stderr
";
        let repo_dir = write_temp_tests_control_manual(control_content);
        disable_self_build_test(&repo_dir).expect("no_op");
        let result =
            fs::read_to_string(repo_dir.join("debian/tests/control")).expect("read result");
        assert_eq!(result.trim(), control_content.trim());
        let _ = fs::remove_dir_all(&repo_dir);
    }
}
