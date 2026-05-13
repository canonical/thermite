use std::path::Path;

use crate::error::Result;
use crate::shell::run_command;
use crate::types::versions::RustVersion;

/// Run `dch -v <version_str>` to open a new changelog entry.
pub async fn run_dch(repo_dir: &Path, version_str: &str) -> Result<()> {
    // Pass --no-auto-nmu to avoid NMU version mangling.
    run_command(
        "dch",
        &["-v", version_str, "--no-auto-nmu"],
        repo_dir,
        // Prevent dch from launching an editor; we update the file ourselves.
        &[("VISUAL", "true"), ("EDITOR", "true")],
    )
    .await?;
    Ok(())
}

/// Update the first changelog entry in `debian/changelog` so that:
/// - The source package name is changed from `old_pkg_name` to `new_pkg_name`.
/// - The target distribution is set to `release`.
/// - The first bullet in the changelog body is replaced with the canonical new
///   upstream version entry referencing `lp_bug`.
///
/// This function edits the file in-place using string manipulation.
pub fn update_changelog_entry(
    changelog_path: &Path,
    old_pkg_name: &str,
    new_pkg_name: &str,
    release: &str,
    upstream_version: &str,
    lp_bug: &str,
) -> Result<()> {
    let content = std::fs::read_to_string(changelog_path)?;
    let mut lines: Vec<String> = content.lines().map(|l| l.to_owned()).collect();

    if lines.is_empty() {
        return Ok(());
    }

    // Fix the first line: rename the package and set the distribution.
    //
    // Typical first line format:
    //   rustc-1.84 (1.84.0+dfsg-0ubuntu1) UNRELEASED; urgency=medium
    let first = &lines[0];
    let updated_first = first
        .replace(old_pkg_name, new_pkg_name)
        .replace("UNRELEASED", release);
    lines[0] = updated_first;

    // Replace the first non-empty bullet line in the body with the canonical
    // new upstream version entry.
    let new_bullet = format!("  * New upstream version {upstream_version} (LP: #{lp_bug})");
    for line in lines.iter_mut().skip(1) {
        let trimmed = line.trim();
        if trimmed.starts_with('*') || trimmed.starts_with('-') {
            *line = new_bullet.clone();
            break;
        }
        // Stop if we hit the trailer line (starts with " -- ").
        if line.starts_with(" -- ") {
            break;
        }
    }

    let new_content = lines.join("\n") + "\n";
    std::fs::write(changelog_path, new_content)?;
    Ok(())
}

/// Return the Debian version string for the new upstream Rust release.
///
/// Format: `"<X.Y.Z>+dfsg-0ubuntu1"`
pub fn debian_version_string(version: &RustVersion) -> String {
    format!("{version}+dfsg-0ubuntu1")
}
