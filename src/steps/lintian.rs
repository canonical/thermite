use std::path::Path;

use crate::error::Result;
use crate::shell::run_command;

/// A single Lintian diagnostic entry.
#[derive(Debug, Clone)]
pub struct LintianEntry {
    pub severity: String,
    pub tag: String,
    pub detail: String,
    pub raw: String,
}

/// Parsed output from a Lintian run.
#[derive(Debug)]
pub struct LintianOutput {
    pub errors: Vec<LintianEntry>,
    pub warnings: Vec<LintianEntry>,
    pub raw: String,
}

/// Run Lintian with the provided `flags` and save output to `log_path`.
///
/// Common flag sets:
/// - Standard: `&["-i", "--tag-display-limit", "0"]`
/// - Pedantic: `&["-i", "-I", "-E", "--pedantic"]`
pub async fn run_lintian(
    repo_dir: &Path,
    flags: &[&str],
    log_path: &Path,
) -> Result<LintianOutput> {
    let output = run_command("lintian", flags, repo_dir, &[]).await;
    let raw = match output {
        Ok(ref o) => o.stdout.clone() + &o.stderr,
        // Finding 2: lintian exits non-zero when it finds issues; its diagnostics
        // are written to stdout. Capture both streams so the parser sees everything.
        Err(crate::error::ThermiteError::CommandFailed {
            ref stdout,
            ref stderr,
            ..
        }) => stdout.clone() + stderr,
        Err(e) => return Err(e),
    };

    // Save the raw log.
    std::fs::write(log_path, &raw)?;

    let parsed = parse_lintian_output(&raw);
    Ok(parsed)
}

/// Run the `debian/lintian-to-copyright.sh` script, piping `lintian_log` into
/// it. Returns the generated copyright stanzas as a string.
pub async fn run_lintian_to_copyright(repo_dir: &Path, lintian_log: &Path) -> Result<String> {
    let log_str = lintian_log.to_string_lossy().to_string();
    let output = run_command(
        "bash",
        &[
            "-c",
            &format!("cat '{log_str}' | debian/lintian-to-copyright.sh"),
        ],
        repo_dir,
        &[],
    )
    .await?;
    Ok(output.stdout)
}

/// Remove Lintian override lines that match any of the entries in `entries`
/// from `overrides_path`.
pub fn remove_mismatched_overrides(overrides_path: &Path, entries: &[String]) -> Result<()> {
    let content = std::fs::read_to_string(overrides_path)?;
    let filtered: Vec<&str> = content
        .lines()
        .filter(|line| !entries.iter().any(|e| line.contains(e.as_str())))
        .collect();
    std::fs::write(overrides_path, filtered.join("\n") + "\n")?;
    Ok(())
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn parse_lintian_output(raw: &str) -> LintianOutput {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    for line in raw.lines() {
        // Lintian lines start with a severity character: E, W, I, P, X.
        let mut chars = line.chars();
        let first = chars.next().unwrap_or(' ');
        if matches!(first, 'E' | 'W' | 'I' | 'P') && line.len() > 2 && &line[1..2] == ":" {
            // Lintian format: "<severity>: <package>: <tag> [details]"
            // Skip the package name field (second colon-delimited token) to
            // reach the tag.
            let rest = &line[3..]; // after "E: "
            let tag = if let Some(after_pkg) = rest.find(": ") {
                rest[after_pkg + 2..]
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
            } else {
                rest.split_whitespace().next().unwrap_or("")
            };
            let entry = LintianEntry {
                severity: first.to_string(),
                tag: tag.to_owned(),
                detail: rest.to_owned(),
                raw: line.to_owned(),
            };
            match first {
                'E' => errors.push(entry),
                'W' => warnings.push(entry),
                _ => {}
            }
        }
    }

    LintianOutput {
        errors,
        warnings,
        raw: raw.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Finding 2: parse_lintian_output correctly classifies E:/W: lines
    /// regardless of which stream they arrived on.
    #[test]
    fn parse_lintian_output_classifies_errors_and_warnings() {
        let raw = "E: rustc-1.85: some-error-tag description here\n\
                   W: rustc-1.85: some-warning-tag description here\n\
                   I: rustc-1.85: info-tag informational\n";
        let out = parse_lintian_output(raw);
        assert_eq!(out.errors.len(), 1, "expected 1 error");
        assert_eq!(out.warnings.len(), 1, "expected 1 warning");
        assert_eq!(out.errors[0].tag, "some-error-tag");
        assert_eq!(out.warnings[0].tag, "some-warning-tag");
    }

    /// Finding 2: when the combined raw text contains E:/W: lines (as would
    /// happen after combining stdout+stderr from a CommandFailed result), the
    /// parser still extracts them correctly.
    #[test]
    fn parse_lintian_output_from_combined_stdout_stderr() {
        // Simulate lintian writing errors to stdout and a summary to stderr.
        let stdout_part = "E: pkg: missing-copyright-file vendor/foo\n";
        let stderr_part = "N: 1 tag overridden\n";
        let combined = stdout_part.to_owned() + stderr_part;
        let out = parse_lintian_output(&combined);
        assert_eq!(out.errors.len(), 1);
        assert_eq!(out.errors[0].tag, "missing-copyright-file");
    }
}
