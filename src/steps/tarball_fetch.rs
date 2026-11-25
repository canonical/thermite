use std::path::{Path, PathBuf};

use crate::error::{Result, ThermiteError};
use crate::shell;
use crate::steps::launchpad;
use crate::types::versions::{RustVersion, ShortRustVersion};

/// File base for the rust-toolchain staging PPA on Launchpad, whose published
/// source files already carry the `~<series>` suffix of a previous backport
/// upload.
const STAGING_PPA_FILES_BASE: &str =
    "https://launchpad.net/~rust-toolchain/+archive/ubuntu/staging/+files";

/// File base for the primary Ubuntu archive on Launchpad, used to reuse the
/// orig (and orig-vendor) tarball published for the same upstream version.
const PRIMARY_ARCHIVE_FILES_BASE: &str = "https://launchpad.net/ubuntu/+archive/primary/+files";

/// Which of the two backport source tarballs to fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TarballKind {
    /// `rustc-<short>_<ver>+dfsg~<series>.orig.tar.xz`
    Orig,
    /// `rustc-<short>_<ver>+dfsg~<series>.orig-vendor.tar.xz`
    OrigVendor,
}

impl TarballKind {
    fn tail(self) -> &'static str {
        match self {
            TarballKind::Orig => ".orig.tar.xz",
            TarballKind::OrigVendor => ".orig-vendor.tar.xz",
        }
    }
}

/// Build the exact tarball filename thermite expects in `dest_dir`.
///
/// `dfsg_suffix` is appended after `+dfsg`: `""` for plain update naming or
/// `"~<series>"` (e.g. `"~26.04"`) for backport naming.
pub fn expected_tarball_name(
    rust_short: &ShortRustVersion,
    rust_ver: &RustVersion,
    dfsg_suffix: &str,
    kind: TarballKind,
) -> String {
    format!(
        "rustc-{rust_short}_{rust_ver}+dfsg{dfsg_suffix}{}",
        kind.tail()
    )
}

/// Percent-encode a filename for use in a Launchpad `+files` URL path.
///
/// Only `+` needs encoding for the tarball names thermite builds; `~` is an
/// RFC 3986 unreserved character and is left as-is.
fn encode_launchpad_path(name: &str) -> String {
    name.replace('+', "%2B")
}

fn launchpad_files_url(base: &str, filename: &str) -> String {
    format!("{base}/{}", encode_launchpad_path(filename))
}

/// Extract the deduplicated upstream version strings (e.g. `1.96.1+dfsg`)
/// from Launchpad publications for every publication whose upstream base
/// matches `rust_ver`.
///
/// The upstream version is everything before the first `-`; only versions
/// starting with `<rust_ver>+dfsg` are considered, so packaging-only versions
/// without the dfsg suffix are skipped. Series order is preserved.
fn upstream_versions_from<'a>(
    publications: impl IntoIterator<Item = &'a [launchpad::Publication]>,
    rust_ver: &str,
) -> Vec<String> {
    let prefix = format!("{rust_ver}+dfsg");
    let mut found: Vec<String> = Vec::new();
    for per_series in publications {
        for publication in per_series {
            let Some(version) = publication.version.as_deref() else {
                continue;
            };
            let upstream = version.split('-').next().unwrap_or(version);
            if upstream.starts_with(&prefix) && !found.contains(&upstream.to_owned()) {
                found.push(upstream.to_owned());
            }
        }
    }
    found
}

/// Attempt to fetch a source tarball automatically.
///
/// Candidate sources, in order:
///
/// 1. the rust-toolchain staging PPA (the file already carries the
///    `~<series>` suffix of a previous backport upload), and
/// 2. the primary Ubuntu archive, reusing the orig tarball published for the
///    same upstream version (publications resolved via the Launchpad API).
///
/// `dfsg_suffix` is appended after `+dfsg` in the expected filename: `""` for
/// plain update naming or `"~<series>"` for backport naming.
///
/// Returns `Ok(Some(path))` when the tarball is available at the expected
/// location (already present or downloaded), and `Ok(None)` when no candidate
/// has it — the caller should fall back to manual placement.
pub async fn fetch_tarball(
    rust_short: &ShortRustVersion,
    rust_ver: &RustVersion,
    dfsg_suffix: &str,
    dest_dir: &Path,
    kind: TarballKind,
) -> Result<Option<PathBuf>> {
    let name = expected_tarball_name(rust_short, rust_ver, dfsg_suffix, kind);
    let dest = dest_dir.join(&name);
    if dest.exists() {
        return Ok(Some(dest));
    }

    let mut candidates = vec![(
        launchpad_files_url(STAGING_PPA_FILES_BASE, &name),
        format!("staging PPA ({name})"),
    )];
    let source = format!("rustc-{rust_short}");
    match primary_archive_upstreams(rust_ver, &source).await {
        Ok(upstreams) => {
            for upstream in upstreams {
                let archive_name = format!("rustc-{rust_short}_{upstream}{}", kind.tail());
                candidates.push((
                    launchpad_files_url(PRIMARY_ARCHIVE_FILES_BASE, &archive_name),
                    format!("Ubuntu archive ({archive_name})"),
                ));
            }
        }
        Err(e) => {
            shell::out_line(
                "  Launchpad series lookup failed — skipping Ubuntu archive candidates",
            );
            tracing::debug!("Launchpad archive enumeration failed: {e}");
        }
    }

    for (url, label) in &candidates {
        shell::out_line(&format!("  Trying {label} …"));
        match launchpad::download_public_file(url, &dest).await {
            Ok(()) if std::fs::metadata(&dest).is_ok_and(|m| m.len() > 0) => {
                return Ok(Some(dest));
            }
            Ok(()) => {
                let _ = std::fs::remove_file(&dest);
            }
            Err(ThermiteError::Cancelled) => return Err(ThermiteError::Cancelled),
            Err(e) => {
                let _ = std::fs::remove_file(&dest);
                tracing::debug!("candidate {label} failed: {e}");
            }
        }
    }
    Ok(None)
}

/// Upstream versions (e.g. `1.96.1+dfsg`) published for `source` in the
/// primary archive of any Ubuntu series thermite cares about: the Launchpad
/// active series (ESM releases like focal are absent there) unioned with the
/// backport chain.
async fn primary_archive_upstreams(rust_ver: &RustVersion, source: &str) -> Result<Vec<String>> {
    let mut series_list = launchpad::cached_active_series().await?;
    for series in crate::types::ubuntu::UbuntuRelease::backport_chain() {
        if !series_list.iter().any(|s| s == series) {
            series_list.push(series.to_owned());
        }
    }
    let mut publications = Vec::new();
    for series in &series_list {
        publications.push(launchpad::cached_publications(series, source).await?);
    }
    Ok(upstream_versions_from(
        publications.iter().map(Vec::as_slice),
        &rust_ver.to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ver(patch: u32) -> RustVersion {
        RustVersion::parse(&format!("1.96.{patch}")).unwrap()
    }

    #[test]
    fn expected_names_match_thermite_contract() {
        let rust_ver = ver(0);
        let rust_short = rust_ver.short();
        assert_eq!(
            expected_tarball_name(&rust_short, &rust_ver, "~26.04", TarballKind::Orig),
            "rustc-1.96_1.96.0+dfsg~26.04.orig.tar.xz"
        );
        assert_eq!(
            expected_tarball_name(&rust_short, &rust_ver, "~26.04", TarballKind::OrigVendor),
            "rustc-1.96_1.96.0+dfsg~26.04.orig-vendor.tar.xz"
        );
        assert_eq!(
            expected_tarball_name(&rust_short, &rust_ver, "", TarballKind::Orig),
            "rustc-1.96_1.96.0+dfsg.orig.tar.xz"
        );
        assert_eq!(
            expected_tarball_name(&rust_short, &rust_ver, "", TarballKind::OrigVendor),
            "rustc-1.96_1.96.0+dfsg.orig-vendor.tar.xz"
        );
    }

    #[test]
    fn launchpad_urls_encode_plus_but_not_tilde() {
        assert_eq!(
            launchpad_files_url(
                STAGING_PPA_FILES_BASE,
                "rustc-1.96_1.96.1+dfsg~26.04.orig.tar.xz"
            ),
            "https://launchpad.net/~rust-toolchain/+archive/ubuntu/staging/+files/rustc-1.96_1.96.1%2Bdfsg~26.04.orig.tar.xz"
        );
        assert_eq!(
            launchpad_files_url(
                PRIMARY_ARCHIVE_FILES_BASE,
                "rustc-1.96_1.96.1+dfsg.orig.tar.xz"
            ),
            "https://launchpad.net/ubuntu/+archive/primary/+files/rustc-1.96_1.96.1%2Bdfsg.orig.tar.xz"
        );
    }

    fn publication(version: Option<&str>) -> launchpad::Publication {
        launchpad::Publication {
            pocket: None,
            version: version.map(str::to_owned),
            self_link: None,
        }
    }

    #[test]
    fn upstream_versions_finds_matching_upstream() {
        let publications = [vec![
            publication(Some("1.96.1+dfsg-0ubuntu1")),
            publication(Some("1.96.1+dfsg-0ubuntu1")),
        ]];
        assert_eq!(
            upstream_versions_from(publications.iter().map(Vec::as_slice), "1.96.1"),
            vec!["1.96.1+dfsg".to_owned()]
        );
    }

    #[test]
    fn upstream_versions_handles_series_suffixed_versions() {
        let publications = [
            vec![publication(Some(
                "1.85.1+dfsg0ubuntu2~bpo0-0ubuntu0.24.04.2",
            ))],
            vec![publication(Some("1.85.0+dfsg1~24.04-0ubuntu1~24.04.1"))],
        ];
        assert_eq!(
            upstream_versions_from(publications.iter().map(Vec::as_slice), "1.85.1"),
            vec!["1.85.1+dfsg0ubuntu2~bpo0".to_owned()]
        );
        assert_eq!(
            upstream_versions_from(publications.iter().map(Vec::as_slice), "1.85.0"),
            vec!["1.85.0+dfsg1~24.04".to_owned()]
        );
    }

    #[test]
    fn upstream_versions_skips_non_matching_and_malformed_versions() {
        let publications = [
            vec![publication(Some("1.96.1-0ubuntu1"))],
            vec![publication(Some("1.96.0+dfsg-0ubuntu1"))],
            vec![publication(None)],
        ];
        assert_eq!(
            upstream_versions_from(publications.iter().map(Vec::as_slice), "1.96.0"),
            vec!["1.96.0+dfsg".to_owned()]
        );
    }

    #[test]
    fn upstream_versions_returns_empty_for_no_matches() {
        let publications: Vec<Vec<launchpad::Publication>> = vec![Vec::new()];
        assert!(
            upstream_versions_from(publications.iter().map(Vec::as_slice), "1.96.0").is_empty()
        );
    }
}
