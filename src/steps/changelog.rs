use std::path::Path;

use crate::error::Result;
use crate::shell::run_command;
use crate::types::versions::RustVersion;

/// Run `dch -v <version_str>` to open a new changelog entry.
pub async fn run_dch(repo_dir: &Path, version_str: &str) -> Result<()> {
    // --no-auto-nmu   avoids NMU version mangling.
    // --force-bad-version (-b) allows backport versions that sort lower than
    //   the current entry, which is always the case when backporting to an
    //   older Ubuntu series.
    run_command(
        "dch",
        &["-v", version_str, "--no-auto-nmu", "--force-bad-version"],
        repo_dir,
        // Use `touch` as the editor: it updates the file's mtime (which dch
        // checks to decide whether the editor modified the file) without
        // altering any content.  Using `true` would leave mtime unchanged,
        // causing dch to treat the file as unmodified and roll back the new
        // entry.
        &[("VISUAL", "touch"), ("EDITOR", "touch")],
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

/// Read the current version string from the first line of `debian/changelog`.
///
/// The first line has the format:
/// `<package> (<version>) <distribution>; urgency=<level>`
///
/// Returns an error if the file cannot be read or the format is unexpected.
pub fn read_current_version(changelog_path: &Path) -> Result<String> {
    let content = std::fs::read_to_string(changelog_path)?;
    let first_line = content.lines().next().unwrap_or("");
    // Extract the version between the first `(` and `)`.
    let version = first_line
        .split_once('(')
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(ver, _)| ver.trim().to_owned())
        .unwrap_or_default();
    if version.is_empty() {
        return Err(crate::error::ThermiteError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("could not parse version from debian/changelog first line: '{first_line}'"),
        )));
    }
    Ok(version)
}

/// Compute the Debian version string for a backport to `target_series`.
///
/// The algorithm:
/// 1. Split the version on the **first** `-` to obtain the upstream part and
///    the Debian revision.
/// 2. In the upstream part, strip any trailing `~XX.YY[.Z…]` suffix and
///    append `~<target_series>`.
/// 3. In the Debian revision, strip any trailing `~XX.YY[.Z…]` suffix and
///    append `~<target_series>.1`.
///
/// Examples (from the backporting docs):
/// - `"1.93.0+dfsg-0ubuntu1"` + `"24.04"`
///   → `"1.93.0+dfsg~24.04-0ubuntu1~24.04.1"`
/// - `"1.89.0+dfsg2~24.04.1-0ubuntu3~24.04.2"` + `"22.04"`
///   → `"1.89.0+dfsg2~22.04-0ubuntu3~22.04.1"`
pub fn compute_backport_version(current_version: &str, target_series: &str) -> String {
    // Split on the first `-`.  Debian version strings guarantee at least one
    // `-` separating the upstream version from the revision.
    let (upstream_raw, debian_rev_raw) = match current_version.split_once('-') {
        Some(pair) => pair,
        None => {
            // Fallback: treat the whole string as the upstream part.
            return format!("{current_version}~{target_series}-0ubuntu1~{target_series}.1");
        }
    };

    let upstream = strip_series_suffix(upstream_raw);
    let debian_rev = strip_series_suffix(debian_rev_raw);

    format!("{upstream}~{target_series}-{debian_rev}~{target_series}.1")
}

/// Strip a trailing `~XX.YY[.Z…]` series suffix from a version component.
///
/// Matches patterns like `~22.04`, `~24.04.1`, `~24.04.2`, etc.
fn strip_series_suffix(s: &str) -> &str {
    // Find the last `~` in the string.
    if let Some(tilde_pos) = s.rfind('~') {
        let suffix = &s[tilde_pos + 1..];
        // The suffix must look like a series: digits, dots, and digits only.
        // Valid: "22.04", "24.04.1", "24.04.2"
        // Invalid: anything else (e.g. a pre-release marker)
        if is_series_like(suffix) {
            return &s[..tilde_pos];
        }
    }
    s
}

/// Return `true` if `s` looks like a numeric series identifier such as
/// `"22.04"` or `"24.04.1"`.
fn is_series_like(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    // Must consist only of ASCII digits and dots, and start/end with a digit.
    let all_digit_dot = s.chars().all(|c| c.is_ascii_digit() || c == '.');
    let starts_digit = s.starts_with(|c: char| c.is_ascii_digit());
    let ends_digit = s.ends_with(|c: char| c.is_ascii_digit());
    all_digit_dot && starts_digit && ends_digit
}

/// Update the most-recent `debian/changelog` entry for a backport so that:
/// - The distribution is set to `release`.
/// - The first bullet is set to `"* Backport to <release> (LP: #N)"` when a
///   bug number is provided, or `"* Backport to <release>"` otherwise.
///
/// This function edits the file in-place.
pub fn update_backport_changelog_entry(
    changelog_path: &Path,
    release: &str,
    lp_bug: Option<&str>,
) -> Result<()> {
    let content = std::fs::read_to_string(changelog_path)?;
    let mut lines: Vec<String> = content.lines().map(|l| l.to_owned()).collect();

    if lines.is_empty() {
        return Ok(());
    }

    // Fix the first line: replace UNRELEASED (or any distribution) with the
    // target release.  The first line format is:
    //   <pkg> (<version>) <distribution>; urgency=medium
    {
        let first = lines[0].clone();
        // Replace the distribution token, which sits between `) ` and `;`.
        if let Some((before_dist, after_semi)) = first
            .split_once(") ")
            .and_then(|(left, right)| right.split_once(';').map(|(_, tail)| (left, tail)))
        {
            lines[0] = format!("{before_dist}) {release};{after_semi}");
        }
    }

    // Build the backport bullet text.
    let bullet = match lp_bug {
        Some(bug) => format!("  * Backport to {release} (LP: #{bug})"),
        None => format!("  * Backport to {release}"),
    };

    // Replace the first non-empty bullet in the body.
    for line in lines.iter_mut().skip(1) {
        let trimmed = line.trim();
        if trimmed.starts_with('*') || trimmed.starts_with('-') {
            *line = bullet.clone();
            break;
        }
        if line.starts_with(" -- ") {
            break;
        }
    }

    let new_content = lines.join("\n") + "\n";
    std::fs::write(changelog_path, new_content)?;
    Ok(())
}

#[cfg(test)]
mod backport_changelog_tests {
    use super::*;

    #[test]
    fn compute_backport_version_from_devel() {
        // Devel (no series number in orig part) → Noble
        assert_eq!(
            compute_backport_version("1.93.0+dfsg-0ubuntu1", "24.04"),
            "1.93.0+dfsg~24.04-0ubuntu1~24.04.1"
        );
    }

    #[test]
    fn compute_backport_version_from_stable_to_older() {
        // Noble → Jammy
        assert_eq!(
            compute_backport_version("1.89.0+dfsg2~24.04.1-0ubuntu3~24.04.2", "22.04"),
            "1.89.0+dfsg2~22.04-0ubuntu3~22.04.1"
        );
    }

    #[test]
    fn compute_backport_version_strips_revision_series() {
        // When the debian revision already carries a series suffix it is stripped.
        assert_eq!(
            compute_backport_version("1.85.0+dfsg~25.04-0ubuntu1~25.04.1", "24.04"),
            "1.85.0+dfsg~24.04-0ubuntu1~24.04.1"
        );
    }

    #[test]
    fn compute_backport_version_idempotent_for_same_series() {
        // When the input already targets the same series, the computed version
        // must equal the input.  backport.rs Phase 3 relies on this invariant
        // to decide whether to skip `dch -v` and avoid creating a duplicate
        // changelog entry on re-runs.
        let v = "1.94.1+dfsg~24.04-0ubuntu1~24.04.1";
        assert_eq!(compute_backport_version(v, "24.04"), v);

        // Also idempotent across different version shapes targeting the same
        // series (e.g. a multi-segment debian revision).
        let v2 = "1.89.0+dfsg2~22.04-0ubuntu3~22.04.1";
        assert_eq!(compute_backport_version(v2, "22.04"), v2);
    }

    #[test]
    fn strip_series_suffix_with_sub_revision() {
        assert_eq!(strip_series_suffix("0ubuntu3~24.04.2"), "0ubuntu3");
        assert_eq!(strip_series_suffix("1.89.0+dfsg2~24.04.1"), "1.89.0+dfsg2");
    }

    #[test]
    fn strip_series_suffix_without_suffix() {
        assert_eq!(strip_series_suffix("0ubuntu1"), "0ubuntu1");
        assert_eq!(strip_series_suffix("1.93.0+dfsg"), "1.93.0+dfsg");
    }

    #[test]
    fn is_series_like_valid() {
        assert!(is_series_like("22.04"));
        assert!(is_series_like("24.04.1"));
        assert!(is_series_like("25.10"));
    }

    #[test]
    fn is_series_like_invalid() {
        assert!(!is_series_like(""));
        assert!(!is_series_like("alpha"));
        assert!(!is_series_like("24.04-foo"));
    }
}
