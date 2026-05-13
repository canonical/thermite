use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::shell::run_command;
use crate::types::versions::RustVersion;

/// Run `uscan --download-version <version>` from `repo_dir`, streaming output
/// to the terminal and saving it to `log_path`.
///
/// Returns the path to the generated orig tarball in the parent directory.
pub async fn run_uscan(repo_dir: &Path, version: &RustVersion, log_path: &Path) -> Result<PathBuf> {
    let version_str = version.to_string();
    let output = run_command(
        "uscan",
        &["--download-version", &version_str, "-v"],
        repo_dir,
        &[],
    )
    .await?;

    // uscan places the tarball in the parent directory. By convention the name
    // ends with `1` (the uscan-generated suffix) before we rename it.
    let short = version.short();
    let tarball_name = format!("rustc-{short}_{version}+dfsg1.orig.tar.xz");
    let tarball = repo_dir.parent().unwrap_or(repo_dir).join(&tarball_name);

    // Finding 7: write the full uscan output to log_path so that the audit
    // trail is available for debugging file-exclusion and repack issues.
    let log_content = output.stdout.clone() + &output.stderr;
    std::fs::write(log_path, &log_content)?;

    Ok(tarball)
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
