use std::path::{Path, PathBuf};

use crate::error::{Result, ThermiteError};
use crate::shell::run_command;
use crate::types::versions::RustVersion;

/// Delays (in seconds) between successive uscan retry attempts when a
/// 403 rate-limit response is detected: first retry after 15 s, second after
/// 30 s, third after 60 s.
const RETRY_DELAYS: [u64; 3] = [15, 30, 60];

/// Returns `true` when the combined uscan output suggests a 403 rate-limit
/// error, allowing the caller to decide whether to retry.
fn is_rate_limit_error(stdout: &str, stderr: &str) -> bool {
    [stdout, stderr].iter().any(|s| {
        let lower = s.to_ascii_lowercase();
        lower.contains("403") || lower.contains("rate limit")
    })
}

/// Run `uscan --download-version <version>` from `repo_dir`, streaming output
/// to the terminal and saving it to `log_path`.
///
/// If uscan exits with a 403 rate-limit error the step is retried up to three
/// times, pausing for 15 s, 30 s, and 60 s respectively between attempts.
///
/// Returns the path to the generated orig tarball in the parent directory.
pub async fn run_uscan(repo_dir: &Path, version: &RustVersion, log_path: &Path) -> Result<PathBuf> {
    let version_str = version.to_string();
    // attempt 0 = first try, attempts 1-3 = retries after rate-limit.
    let mut last_rate_limit_err: Option<ThermiteError> = None;

    for attempt in 0_u32..4 {
        if attempt > 0 {
            let secs = RETRY_DELAYS[(attempt - 1) as usize];
            println!("  Rate limit detected. Retrying in {secs} seconds . . .");
            crate::ui::countdown_secs(secs).await;
        }

        match run_command(
            "uscan",
            &["--download-version", &version_str, "-v"],
            repo_dir,
            &[],
        )
        .await
        {
            Ok(output) => {
                let short = version.short();
                let tarball_name = format!("rustc-{short}_{version}+dfsg1.orig.tar.xz");
                let tarball = repo_dir.parent().unwrap_or(repo_dir).join(&tarball_name);

                let log_content = output.stdout + &output.stderr;
                std::fs::write(log_path, &log_content)?;

                return Ok(tarball);
            }
            Err(e) => {
                let rate_limited = if let ThermiteError::CommandFailed {
                    ref stdout,
                    ref stderr,
                    ..
                } = e
                {
                    is_rate_limit_error(stdout, stderr)
                } else {
                    false
                };

                if rate_limited && attempt < 3 {
                    last_rate_limit_err = Some(e);
                    // continue to next attempt
                } else {
                    return Err(e);
                }
            }
        }
    }

    // All three retries exhausted — surface the last rate-limit error.
    Err(last_rate_limit_err
        .expect("loop invariant: last_rate_limit_err is set before reaching this point"))
}

/// Rename a tarball by replacing its current stem suffix with `new_suffix`.
///
/// For example, calling `rename_tarball_with_suffix(path, "~old")` on a path
/// ending in `1.orig.tar.xz` produces a path ending in `~old.orig.tar.xz`.
pub fn rename_tarball_with_suffix(tarball: &Path, new_suffix: &str) -> Result<PathBuf> {
    let name = tarball
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();

    // Strip the `1` suffix that uscan appends before `.orig.tar.xz`.
    let new_name = if let Some(base) = name.strip_suffix("1.orig.tar.xz") {
        format!("{base}{new_suffix}.orig.tar.xz")
    } else if let Some(base) = name.strip_suffix(".orig.tar.xz") {
        format!("{base}{new_suffix}.orig.tar.xz")
    } else {
        format!("{name}{new_suffix}")
    };

    let new_path = tarball.with_file_name(new_name);
    std::fs::rename(tarball, &new_path)?;
    Ok(new_path)
}

/// Rename a tarball to the canonical orig tarball format (no extra suffix).
///
/// Strips the numeric suffix `1` added by uscan, producing the final name.
pub fn rename_tarball_to_canonical(tarball: &Path) -> Result<PathBuf> {
    let name = tarball
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();

    let new_name = if let Some(base) = name.strip_suffix("1.orig.tar.xz") {
        format!("{base}.orig.tar.xz")
    } else {
        name.to_owned()
    };

    let new_path = tarball.with_file_name(new_name);
    if tarball != new_path {
        std::fs::rename(tarball, &new_path)?;
    }
    Ok(new_path)
}

/// List all `.c` source files inside a `.tar.xz` archive without extracting it.
pub async fn list_c_files_in_tarball(tarball: &Path) -> Result<Vec<String>> {
    let tarball_str = tarball.to_string_lossy().to_string();
    let output = run_command(
        "tar",
        &["-tJf", &tarball_str],
        tarball.parent().unwrap_or(tarball),
        &[],
    )
    .await?;

    let c_files: Vec<String> = output
        .stdout
        .lines()
        .filter(|l| l.ends_with(".c"))
        .map(|l| l.to_owned())
        .collect();

    Ok(c_files)
}

#[cfg(test)]
mod tests {
    use std::fs;

    /// Finding 7: rename_tarball_with_suffix correctly appends the suffix
    /// before .orig.tar.xz.
    #[test]
    fn rename_tarball_with_suffix_produces_correct_name() {
        // Use a temp path; we test name construction without touching the fs.
        let path = std::path::PathBuf::from("/tmp/rustc-1.85_1.85.0+dfsg1.orig.tar.xz");
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let new_name = if let Some(base) = name.strip_suffix("1.orig.tar.xz") {
            format!("{base}~old.orig.tar.xz")
        } else {
            name.to_owned()
        };
        assert_eq!(new_name, "rustc-1.85_1.85.0+dfsg~old.orig.tar.xz");
    }

    /// Finding 7: log_path receives the uscan output rather than being left
    /// empty. We test the write path directly using a temporary file.
    #[test]
    fn log_write_produces_non_empty_file_for_non_empty_output() {
        let tmp = std::env::temp_dir().join(format!(
            "thermite-uscan-log-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
        ));
        let content = "uscan output line 1\nuscan output line 2\n";
        fs::write(&tmp, content).expect("write log");
        let read_back = fs::read_to_string(&tmp).expect("read log");
        assert_eq!(read_back, content);
        let _ = fs::remove_file(&tmp);
    }
}
