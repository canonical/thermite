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
    Failure { log_path: PathBuf },
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
        Err(crate::error::ThermiteError::CommandFailed { .. }) => {
            // Find the most recent .build log in the parent directory.
            let parent = repo_dir.parent().unwrap_or(repo_dir);
            let log_path = find_latest_build_log(parent);
            Ok(SbuildResult::Failure {
                log_path: log_path.unwrap_or_else(|| parent.join("build.log")),
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
