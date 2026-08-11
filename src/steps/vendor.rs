use std::path::{Path, PathBuf};

use crate::error::{Result, ThermiteError};
use crate::shell::{run_command, which};
use crate::types::versions::RustVersion;

/// Ensure `rustup` is installed (available on PATH).
///
/// Returns an error with installation guidance if it is not found.
pub async fn ensure_rustup_installed() -> Result<String> {
    which("rustup").map_err(|_| {
        ThermiteError::CommandNotFound(
            "rustup (install it with: snap install rustup --classic)".to_owned(),
        )
    })
}

/// Install the Rust toolchain matching `version` via `rustup install`.
///
/// Returns the path to the installed toolchain directory
/// (`~/.rustup/toolchains/<version>-x86_64-unknown-linux-gnu`).
pub async fn rustup_install_toolchain(version: &RustVersion) -> Result<PathBuf> {
    let version_str = version.to_string();
    run_command("rustup", &["install", &version_str], Path::new("."), &[]).await?;

    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_owned());
    let toolchain_dir = PathBuf::from(format!(
        "{home}/.rustup/toolchains/{version}-x86_64-unknown-linux-gnu"
    ));
    Ok(toolchain_dir)
}

/// Install `cargo-vendor-filterer` using the Rust toolchain for `version`.
pub async fn install_cargo_vendor_filterer(version: &RustVersion) -> Result<()> {
    let version_str = version.to_string();
    let toolchain_flag = format!("+{version_str}");
    run_command(
        "cargo",
        &[&toolchain_flag, "install", "cargo-vendor-filterer"],
        Path::new("."),
        &[],
    )
    .await?;
    Ok(())
}

/// Generate the pruned vendor tarball component by running
/// `debian/rules vendor-tarball` with `RUST_BOOTSTRAP_DIR` set.
///
/// `dfsg_suffix` is appended after `+dfsg` in the tarball filename to match
/// the upstream version in `debian/changelog`.  For backports this is the
/// series suffix (e.g. `"~20.04"`); for main-series builds pass `""`.
///
/// Returns the path to the generated vendor tarball component.
///
/// Finding 6: use the known naming convention derived from `version` instead
/// of selecting the first matching file, making the choice deterministic even
/// when stale tarballs from prior builds are present.
pub async fn generate_vendor_tarball(
    repo_dir: &Path,
    rust_bootstrap_dir: &Path,
    version: &RustVersion,
    dfsg_suffix: &str,
) -> Result<PathBuf> {
    let bootstrap_str = rust_bootstrap_dir.to_string_lossy().to_string();
    run_command(
        "debian/rules",
        &["vendor-tarball"],
        repo_dir,
        &[("RUST_BOOTSTRAP_DIR", &bootstrap_str)],
    )
    .await?;

    // Finding 6: construct the expected filename deterministically from the
    // Rust version rather than picking the first .orig-vendor.tar.xz found.
    let parent = repo_dir.parent().unwrap_or(repo_dir);
    let short = version.short();
    let tarball_name = format!("rustc-{short}_{version}+dfsg{dfsg_suffix}.orig-vendor.tar.xz");
    let tarball_path = parent.join(&tarball_name);

    if !tarball_path.exists() {
        return Err(ThermiteError::CommandFailed {
            cmd: "debian/rules vendor-tarball".to_owned(),
            code: 0,
            stdout: String::new(),
            stderr: format!(
                "expected vendor tarball not found: {}",
                tarball_path.display()
            ),
        });
    }

    Ok(tarball_path)
}

#[cfg(test)]
mod tests {
    use crate::types::versions::RustVersion;

    /// Finding 6: the vendor tarball name is derived deterministically from the
    /// Rust version, matching the debian/rules naming convention.
    #[test]
    fn vendor_tarball_expected_name_format() {
        let version = RustVersion::parse("1.85.0").unwrap();
        let short = version.short();
        let name = format!("rustc-{short}_{version}+dfsg.orig-vendor.tar.xz");
        assert_eq!(name, "rustc-1.85_1.85.0+dfsg.orig-vendor.tar.xz");
    }
}
