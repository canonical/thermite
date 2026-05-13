use std::path::Path;

use crate::error::Result;
use crate::shell::run_command;
use crate::types::versions::ShortRustVersion;

/// Run `debian/rules update-version` with `RUST_BOOTSTRAP_DIR` pointing to
/// the installed Rust toolchain.
pub async fn run_update_version_rule(repo_dir: &Path, rust_bootstrap_dir: &Path) -> Result<()> {
    let bootstrap_str = rust_bootstrap_dir.to_string_lossy().to_string();
    run_command(
        "debian/rules",
        &["update-version"],
        repo_dir,
        &[("RUST_BOOTSTRAP_DIR", &bootstrap_str)],
    )
    .await?;
    Ok(())
}

/// Read `debian/control` and verify that the two bootstrapping compiler
/// `Build-Depends` entries are `rustc-<old>` and `rustc-<new>`.
///
/// Returns `Ok(())` if both entries are present, or an error describing what
/// is missing.
pub async fn verify_bootstrap_deps(
    control_path: &Path,
    old: &ShortRustVersion,
    new: &ShortRustVersion,
) -> Result<()> {
    let content = std::fs::read_to_string(control_path)?;
    let old_dep = format!("rustc-{old}");
    let new_dep = format!("rustc-{new}");

    let has_old = content.contains(&old_dep);
    let has_new = content.contains(&new_dep);

    if !has_old || !has_new {
        let missing: Vec<&str> = [
            (!has_old).then_some(old_dep.as_str()),
            (!has_new).then_some(new_dep.as_str()),
        ]
        .into_iter()
        .flatten()
        .collect();
        return Err(crate::error::ThermiteError::CommandFailed {
            cmd: "verify bootstrap deps".to_owned(),
            code: 0,
            stdout: String::new(),
            stderr: format!(
                "Expected bootstrap Build-Depends not found in debian/control: {}",
                missing.join(", ")
            ),
        });
    }
    Ok(())
}

/// Generate the `XS-Vendored-Sources-Rust` field value by running
/// `dh-cargo-vendored-sources` with `CARGO_VENDOR_DIR=vendor/`.
pub async fn generate_vendored_sources(repo_dir: &Path) -> Result<String> {
    let output = run_command(
        "/usr/share/cargo/bin/dh-cargo-vendored-sources",
        &[],
        repo_dir,
        &[("CARGO_VENDOR_DIR", "vendor/")],
    )
    .await?;
    Ok(output.stdout.trim().to_owned())
}

/// Replace the `XS-Vendored-Sources-Rust` field value in `debian/control`
/// with `new_value`.
pub fn update_xs_vendored_sources(control_path: &Path, new_value: &str) -> Result<()> {
    let content = std::fs::read_to_string(control_path)?;
    let mut lines: Vec<String> = content.lines().map(|l| l.to_owned()).collect();

    // Find the line starting with `XS-Vendored-Sources-Rust:` and replace it
    // along with any continuation lines.
    let mut i = 0;
    let mut found = false;
    while i < lines.len() {
        if lines[i].starts_with("XS-Vendored-Sources-Rust:") {
            // Remove the old field (header + continuation lines).
            lines.remove(i);
            while i < lines.len() && (lines[i].starts_with(' ') || lines[i].starts_with('\t')) {
                lines.remove(i);
            }
            // Insert the new field.
            lines.insert(i, format!("XS-Vendored-Sources-Rust: {new_value}"));
            found = true;
            break;
        }
        i += 1;
    }

    if !found {
        lines.push(format!("XS-Vendored-Sources-Rust: {new_value}"));
    }

    std::fs::write(control_path, lines.join("\n") + "\n")?;
    Ok(())
}

/// Check for Windows-related crate names in the `XS-Vendored-Sources-Rust`
/// field value. Returns a list of crate names that appear to be Windows-only.
pub fn check_no_windows_crates(xs_value: &str) -> Vec<String> {
    xs_value
        .split(',')
        .map(|entry| entry.trim())
        .filter(|entry| {
            let lower = entry.to_lowercase();
            lower.starts_with("windows") || lower.contains("windows-sys")
        })
        .map(|e| e.to_owned())
        .collect()
}

/// Add a `Build-Depends` entry to `debian/control` and `debian/control.in`.
///
/// The `dep` string should be the raw dependency (e.g. `"libonig-dev"`).
pub fn add_build_dependency(control_path: &Path, dep: &str) -> Result<()> {
    let content = std::fs::read_to_string(control_path)?;
    let mut lines: Vec<String> = content.lines().map(|l| l.to_owned()).collect();

    // Find `Build-Depends:` section and append the new dependency just before
    // the first blank line or the next field.
    let mut in_build_depends = false;
    let mut insert_at: Option<usize> = None;

    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("Build-Depends:") {
            in_build_depends = true;
            insert_at = Some(i + 1);
            continue;
        }
        if in_build_depends {
            if line.starts_with(' ') || line.starts_with('\t') {
                insert_at = Some(i + 1);
            } else {
                break;
            }
        }
    }

    if let Some(idx) = insert_at {
        lines.insert(idx, format!(" {dep},"));
    }

    std::fs::write(control_path, lines.join("\n") + "\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_path(suffix: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "thermite-control-{}-{}",
            suffix,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
        ))
    }

    /// Finding 12: add_build_dependency appends the new package to the
    /// Build-Depends section of the control file.
    #[test]
    fn add_build_dependency_appends_to_build_depends() {
        let path = temp_path("control");
        fs::write(
            &path,
            "Source: rustc-1.85\nBuild-Depends: debhelper (>= 13),\n libssl-dev,\nRules-Requires-Root: no\n",
        )
        .unwrap();

        add_build_dependency(&path, "libonig-dev").unwrap();

        let result = fs::read_to_string(&path).unwrap();
        assert!(
            result.contains("libonig-dev"),
            "new dep should appear in file"
        );
        assert!(
            result.contains("Build-Depends:"),
            "section header must be preserved"
        );

        let _ = fs::remove_file(&path);
    }

    /// Finding 12: add_vendor_exclusion (copyright.rs) appends a pattern
    /// under the Files-Excluded-vendor section.
    #[test]
    fn add_vendor_exclusion_adds_pattern() {
        use crate::steps::copyright::add_vendor_exclusion;
        let path = temp_path("copyright");
        fs::write(
            &path,
            "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/\n\
             Files-Excluded-vendor:\n libfoo/\n",
        )
        .unwrap();

        add_vendor_exclusion(&path, "libgit2/").unwrap();

        let result = fs::read_to_string(&path).unwrap();
        assert!(
            result.contains("libgit2/"),
            "new exclusion should appear in file"
        );

        let _ = fs::remove_file(&path);
    }
}
