//! Compatibility checks for backporting.
//!
//! Each check answers one question: how does this package satisfy the
//! requirement on the target release, and what evidence supports that?
//!
//! - Satisfied from the archive: the required package/version is published
//!   (checked with `rmadison` and `dpkg --compare-versions`).
//! - Satisfied by the packaging: a common backporting fix is already applied
//!   — vendored LLVM or libgit2, disabled dh-cargo, the pkg-config fallback,
//!   a cmake-mozilla alternative.
//! - Needs attention: a demonstrated problem (dependency too old, a fix only
//!   half applied); the remaining edit is named.
//! - Could not infer automatically: the evidence needed to distinguish the
//!   archive path from an already-applied fix is missing or contradictory;
//!   both sides are reported.
//! - Archive check failed: a lookup was needed and attempted but failed.
//!
//! Only the known common fixes (LLVM, libgit2, dh-cargo, pkgconf, cmake,
//! debhelper-compat) are detected — no attempt is made to diagnose arbitrary
//! build failures.

use std::path::Path;

use crate::cache;
use crate::error::ThermiteError;
use crate::shell::{run_command, which};

// ── Public types ──────────────────────────────────────────────────────────────

/// The coarse conclusion of a compatibility check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeKind {
    /// The required package/version is published in the target archive.
    SatisfiedFromArchive,
    /// The packaging itself satisfies the requirement; the archive is not
    /// involved (vendored dependency, disabled tool, verified fallback).
    SatisfiedByPackaging,
    /// A demonstrated problem; the remaining edit is named.
    NeedsAttention,
    /// Evidence missing or contradictory; the check cannot decide.
    Unknown,
    /// An archive lookup was needed and attempted but failed.
    ArchiveCheckFailed,
}

/// The conclusion of one compatibility check, with the evidence behind it.
#[derive(Debug, Clone)]
pub struct Outcome {
    pub kind: OutcomeKind,
    /// One-line conclusion (e.g. "already vendored — build uses the bundled
    /// src/llvm-project tree").
    pub summary: String,
    /// What was observed, one line per item.
    pub evidence: Vec<String>,
}

/// The combined result of a single compatibility check.
#[derive(Debug, Clone)]
pub struct CheckResult {
    /// Short human-readable name of the check (e.g. `"LLVM"`).
    pub name: &'static str,
    pub outcome: Outcome,
    /// One-line summary of the applicable fix, or empty when no fix is needed.
    pub guidance: &'static str,
    /// Launchpad URL for manual verification; empty when the packaging itself
    /// satisfies the check (the evidence lines replace the link).
    pub url: String,
    /// Whether the orig tarball (Phase 5) must be regenerated after the fix —
    /// meaningful only when the check needs attention.
    pub needs_tarball_regen: bool,
}

impl CheckResult {
    /// Returns `true` when the check passed and no action is needed.
    pub fn is_ok(&self) -> bool {
        matches!(
            self.outcome.kind,
            OutcomeKind::SatisfiedFromArchive | OutcomeKind::SatisfiedByPackaging
        )
    }
}

// ── Packaging readers ─────────────────────────────────────────────────────────

/// Read a file relative to `repo_dir`; `None` when missing or unreadable.
fn read_debian_file(repo_dir: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(repo_dir.join(name)).ok()
}

/// Iterate over the lines of `content`, skipping lines whose first non-space
/// character is `#`. Used when scanning make/rules/config content so
/// commented-out directives (common in backport branches) are not mistaken
/// for active ones.
fn uncommented_lines(content: &str) -> impl Iterator<Item = &str> {
    content
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
}

/// Extract a folded control-style field value (e.g. `Build-Depends:` or
/// `Files-Excluded:`) spanning multiple continuation lines. Full-line `#`
/// comments are legal inside a field and are skipped. `None` when the field
/// does not appear.
fn folded_field(content: &str, field: &str) -> Option<String> {
    let prefix = format!("{field}:");
    let mut value: Option<String> = None;
    for line in content.lines() {
        if line.starts_with('#') {
            continue;
        }
        match &mut value {
            Some(v) => {
                if line.starts_with(' ') || line.starts_with('\t') {
                    if !v.is_empty() {
                        v.push(' ');
                    }
                    v.push_str(line.trim());
                } else {
                    break;
                }
            }
            None => {
                if let Some(rest) = line.strip_prefix(prefix.as_str()) {
                    value = Some(rest.trim().to_owned());
                }
            }
        }
    }
    value
}

/// The active Build-Depends value from `debian/control`, with the value from
/// `debian/control.in` appended when that template exists (it is optional and
/// absent on some backport branches). `None` when `debian/control` itself is
/// missing or unreadable.
fn build_depends_field(repo_dir: &Path) -> Option<String> {
    let control = read_debian_file(repo_dir, "debian/control")?;
    let mut field = folded_field(&control, "Build-Depends").unwrap_or_default();
    if repo_dir.join("debian/control.in").exists()
        && let Some(extra) = read_debian_file(repo_dir, "debian/control.in")
            .as_deref()
            .and_then(|c| folded_field(c, "Build-Depends"))
    {
        if !field.is_empty() {
            field.push(' ');
        }
        field.push_str(&extra);
    }
    Some(field)
}

/// Split a dependency field into comma-separated entries, ignoring commas
/// inside version parentheses.
fn split_entries(field: &str) -> Vec<String> {
    let mut entries = Vec::new();
    let mut current = String::new();
    let mut depth = 0usize;
    for c in field.chars() {
        match c {
            '(' => {
                depth += 1;
                current.push(c);
            }
            ')' => {
                depth = depth.saturating_sub(1);
                current.push(c);
            }
            ',' if depth == 0 => {
                entries.push(current.trim().to_owned());
                current.clear();
            }
            _ => current.push(c),
        }
    }
    entries.push(current.trim().to_owned());
    entries.retain(|e| !e.is_empty());
    entries
}

/// Parse one alternative of a dependency entry into its package name and its
/// `(op version)` relations. Architecture (`[...]`) and build-profile
/// (`<...>`) annotations are ignored.
fn parse_alternative(alt: &str) -> Option<(String, Vec<(String, String)>)> {
    let trimmed = alt.trim();
    if trimmed.is_empty() {
        return None;
    }
    let name_end = trimmed.find(['(', '[', '<']).unwrap_or(trimmed.len());
    let name = trimmed[..name_end].trim().to_owned();
    if name.is_empty() {
        return None;
    }
    let mut relations = Vec::new();
    if let Some(open) = trimmed.find('(')
        && let Some(close) = trimmed[open..].find(')')
    {
        let inner = &trimmed[open + 1..open + close];
        for part in inner.split(',') {
            let mut parts = part.split_whitespace();
            if let (Some(op), Some(ver)) = (parts.next(), parts.next()) {
                relations.push((op.to_owned(), ver.to_owned()));
            }
        }
    }
    Some((name, relations))
}

/// The entries of `entries` that declare `package` as a whole word.
fn entries_for(entries: &[String], package: &str) -> Vec<String> {
    entries
        .iter()
        .filter(|e| contains_word(e, package))
        .cloned()
        .collect()
}

/// Collapse a list of `(op, version)` relations into optional
/// `(floor, ceiling)` versions. `=` is treated as a floor (the versioned
/// build-dep semantics relevant here, e.g. `debhelper-compat (= 13)`).
fn bounds_from_relations(relations: &[(String, String)]) -> (Option<String>, Option<String>) {
    let mut floor = None;
    let mut ceiling = None;
    for (op, ver) in relations {
        match op.as_str() {
            ">=" | ">>" | "=" => floor = Some(ver.clone()),
            "<=" | "<<" => ceiling = Some(ver.clone()),
            _ => {}
        }
    }
    (floor, ceiling)
}

/// The first uncommented line of `content` containing `needle`, with its
/// 1-based line number.
fn find_active_rule_line(content: &str, needle: &str) -> Option<(usize, String)> {
    content
        .lines()
        .enumerate()
        .find(|(_, line)| {
            let trimmed = line.trim_start();
            !trimmed.starts_with('#') && trimmed.contains(needle)
        })
        .map(|(idx, line)| (idx + 1, line.trim().to_owned()))
}

/// The `Files-Excluded` value of `debian/copyright`, when the field exists.
fn files_excluded(repo_dir: &Path) -> Option<String> {
    let copyright = read_debian_file(repo_dir, "debian/copyright")?;
    folded_field(&copyright, "Files-Excluded")
}

/// The quilt patches listed as active in `debian/patches/series`.
fn active_patches(repo_dir: &Path) -> Vec<String> {
    let Some(series) = read_debian_file(repo_dir, "debian/patches/series") else {
        return Vec::new();
    };
    series
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with("include"))
        .map(str::to_owned)
        .collect()
}

/// Active quilt patches that add or remove the cargo `vendored-libgit2`
/// feature line, as `(added_by, removed_by)` patch name lists.
fn vendored_libgit2_patch_effects(repo_dir: &Path) -> (Vec<String>, Vec<String>) {
    let mut added = Vec::new();
    let mut removed = Vec::new();
    for patch in active_patches(repo_dir) {
        let Some(content) = read_debian_file(repo_dir, &format!("debian/patches/{patch}")) else {
            continue;
        };
        let mut minus = 0usize;
        let mut plus = 0usize;
        for line in content.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("-vendored-libgit2") {
                minus += 1;
            } else if trimmed.starts_with("+vendored-libgit2") {
                plus += 1;
            }
        }
        if minus > plus {
            removed.push(patch);
        } else if plus > minus {
            added.push(patch);
        }
    }
    (added, removed)
}

/// The vendored `libgit2-sys-*` crate directories whose bundled `libgit2` C
/// source is present (non-empty), sorted by crate name.
fn vendored_libgit2_crates(repo_dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(repo_dir.join("vendor")) else {
        return found;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("libgit2-sys-") {
            continue;
        }
        let bundled = entry.path().join("libgit2");
        let non_empty = std::fs::read_dir(&bundled)
            .map(|mut dir| dir.next().is_some())
            .unwrap_or(false);
        if non_empty {
            found.push(name);
        }
    }
    found.sort();
    found
}

/// Whether `src/tools/cargo/Cargo.toml` declares the `vendored-libgit2`
/// cargo feature.
fn cargo_feature_vendored_libgit2_in_tree(repo_dir: &Path) -> bool {
    read_debian_file(repo_dir, "src/tools/cargo/Cargo.toml")
        .map(|content| {
            content
                .lines()
                .any(|line| line.trim_start().starts_with("vendored-libgit2"))
        })
        .unwrap_or(false)
}

/// The `debian/*.install.in` files containing active `${env:VAR}`
/// substitutions (compat-13 style; compat 12 needs `@VARIABLE@`).
fn install_in_env_substitutions(repo_dir: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(repo_dir.join("debian")) else {
        return found;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".install.in") {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(entry.path())
            && content.contains("${env:")
        {
            found.push(format!("debian/{name}"));
        }
    }
    found.sort();
    found
}

/// The LLVM major version from an uncommented `LLVM_VERSION = <N>` assignment
/// in `debian/rules` (accepts `=`, `:=`, and `?=`).
fn rules_llvm_major(rules: &str) -> Option<String> {
    for line in uncommented_lines(rules) {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("LLVM_VERSION") else {
            continue;
        };
        let rest = rest.trim_start();
        let rest = rest
            .strip_prefix("?=")
            .or_else(|| rest.strip_prefix(":="))
            .or_else(|| rest.strip_prefix('='))
            .unwrap_or(rest);
        let rest = rest.trim();
        if !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return Some(rest.to_owned());
        }
    }
    None
}

/// A path (relative to the package root) proving that the bundled LLVM source
/// is present in the tree.
fn llvm_source_marker(repo_dir: &Path) -> Option<String> {
    for marker in [
        "src/llvm-project/llvm/CMakeLists.txt",
        "src/llvm-project/cmake/Modules/LLVMVersion.cmake",
    ] {
        if repo_dir.join(marker).is_file() {
            return Some(marker.to_owned());
        }
    }
    let dir = repo_dir.join("src/llvm-project");
    let non_empty = std::fs::read_dir(&dir)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false);
    non_empty.then(|| "src/llvm-project (non-empty directory)".to_owned())
}

/// An uncommented `llvm-config = "/usr/..."` line in the bootstrap config
/// template that selects the system LLVM, together with the major version it
/// names. Template placeholders (`LLVM_DESTDIR`, `@LLVM_VERSION_MAJOR@`)
/// carry no version digits and are ignored.
fn config_system_llvm_config(content: &str) -> Option<(String, String)> {
    for line in uncommented_lines(content) {
        let trimmed = line.trim();
        if !trimmed.contains("llvm-config") || !trimmed.contains("/usr/") {
            continue;
        }
        if let Some(major) = llvm_major_from_llvm_config(trimmed) {
            return Some((trimmed.to_owned(), major));
        }
    }
    None
}

/// The LLVM major version named by a system `llvm-config` path, either
/// `/usr/lib/llvm-<N>/...` or `/usr/bin/llvm-config-<N>`.
fn llvm_major_from_llvm_config(line: &str) -> Option<String> {
    let candidates = [
        line.find("/usr/lib/llvm-")
            .map(|idx| idx + "/usr/lib/llvm-".len()),
        line.find("llvm-config-")
            .map(|idx| idx + "llvm-config-".len()),
    ];
    for start in candidates.into_iter().flatten() {
        let digits: String = line[start..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if !digits.is_empty() {
            return Some(digits);
        }
    }
    None
}

/// An uncommented `debian/rules` line exporting `PKG_CONFIG` with a value
/// that selects `pkg-config`, e.g. `export PKG_CONFIG=pkg-config`.
fn pkg_config_export_line(rules: &str) -> Option<(usize, String)> {
    for (idx, line) in rules.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || !trimmed.contains("PKG_CONFIG") {
            continue;
        }
        let Some(eq) = trimmed.find('=') else {
            continue;
        };
        let lhs = trimmed[..eq]
            .trim_end()
            .trim_end_matches(['?', ':'])
            .trim_end();
        let rhs = trimmed[eq + 1..].trim();
        if lhs.ends_with("PKG_CONFIG") && rhs.contains("pkg-config") {
            return Some((idx + 1, trimmed.to_owned()));
        }
    }
    None
}

// ── Word-boundary helpers ─────────────────────────────────────────────────────

/// Returns `true` when `word` appears in `line` as a whole word (i.e. not
/// preceded or followed by a non-whitespace, non-comma, non-paren character).
fn contains_word(line: &str, word: &str) -> bool {
    find_word(line, word).is_some()
}

/// Find the byte index of `word` in `line` when it appears as a whole word.
/// Returns `None` when not found.
fn find_word(line: &str, word: &str) -> Option<usize> {
    let mut start = 0;
    while let Some(idx) = line[start..].find(word) {
        let abs = start + idx;
        let before_ok = abs
            .checked_sub(1)
            .map(|i| is_word_boundary(line.as_bytes()[i]))
            .unwrap_or(true);
        let after_idx = abs + word.len();
        let after_ok = line
            .as_bytes()
            .get(after_idx)
            .map(|b| is_word_boundary(*b))
            .unwrap_or(true);
        if before_ok && after_ok {
            return Some(abs);
        }
        start = abs + word.len();
    }
    None
}

/// A byte is a word boundary for our purposes when it is whitespace, a comma,
/// a parenthesis, or end-of-line.
fn is_word_boundary(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b',' | b'(' | b')' | b'|')
}

// ── Archive check ─────────────────────────────────────────────────────────────

/// The status of a package/version in the target release's archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveStatus {
    /// The package is published in the archive; carries its version string.
    Available(String),
    /// The package is not published in the archive at all.
    NotPublished,
    /// The archive check itself failed (e.g. `rmadison` not installed,
    /// network error).
    CheckFailed(String),
}

/// Check whether `package` is published in the `release` suite of the Ubuntu
/// archive using `rmadison -u ubuntu <package>`.
///
/// The raw rmadison output is cached per query under the user cache directory
/// (see [`crate::cache`]); repeated backports against the same query reuse the
/// stored result instead of re-querying the archive. A cache hit needs no
/// local `rmadison` binary, so the availability check only guards the
/// network-fetch path.
///
/// Returns the version string of the first matching entry, or `NotPublished`
/// when no entry matches the release.
pub async fn check_archive(package: &str, release: &str) -> ArchiveStatus {
    if let Some(hit) = cache::lookup_rmadison(package) {
        println!(
            "  rmadison: using cached result for {package} ({})",
            cache::format_age(hit.age_secs)
        );
        return match parse_rmadison_version(&hit.data, release) {
            Some(v) => ArchiveStatus::Available(v),
            None => ArchiveStatus::NotPublished,
        };
    }
    if which("rmadison").is_err() {
        return ArchiveStatus::CheckFailed(
            "rmadison not installed (sudo apt install devscripts)".to_owned(),
        );
    }
    let output =
        match run_command("rmadison", &["-u", "ubuntu", package], Path::new("."), &[]).await {
            Ok(o) => o,
            Err(ThermiteError::CommandFailed { stdout, stderr, .. }) => {
                // rmadison exits non-zero on network failure; surface a hint.
                let detail = if !stderr.trim().is_empty() {
                    stderr.trim().to_owned()
                } else if !stdout.trim().is_empty() {
                    stdout.trim().to_owned()
                } else {
                    "rmadison exited non-zero".to_owned()
                };
                return ArchiveStatus::CheckFailed(detail);
            }
            Err(e) => return ArchiveStatus::CheckFailed(e.to_string()),
        };
    cache::store_rmadison(package, &output.stdout);
    match parse_rmadison_version(&output.stdout, release) {
        Some(v) => ArchiveStatus::Available(v),
        None => ArchiveStatus::NotPublished,
    }
}

/// Parse `rmadison -u ubuntu <package>` output and return the version string
/// of the first entry whose suite matches `release`.
///
/// rmadison output format (one source per line, pipe-separated):
/// ```text
///   rustc | 1.85.0+dfsg1~24.04-0ubuntu1~24.04.1 | noble-security | source
///   rustc | 1.85.0+dfsg1~24.04-0ubuntu1~24.04.1 | noble-updates   | source
///   rustc | 1.85.0+dfsg1-0ubuntu1               | noble           | source
/// ```
///
/// The suite field is emitted as `<suite>` or `<suite>/<component>`
/// (e.g. `noble` or `resolute/universe`), and may carry a pocket suffix
/// (e.g. `noble-security`, `noble-updates`, `resolute-proposed/universe`).
///
/// We accept entries whose suite equals `release`, starts with
/// `<release>-` (e.g. `jammy-security`, `jammy-updates`), or starts with
/// `<release>/` (e.g. `resolute/universe`). Iteration preserves rmadison's
/// output order, so the first matching line wins — callers that need the
/// newest version across pockets should pre-sort the input.
pub fn parse_rmadison_version(stdout: &str, release: &str) -> Option<String> {
    let with_hyphen = format!("{release}-");
    let with_slash = format!("{release}/");
    for line in stdout.lines() {
        // Skip blank lines, warnings, and headers.
        if line.trim().is_empty() || !line.contains('|') {
            continue;
        }
        let parts: Vec<&str> = line.split('|').map(|p| p.trim()).collect();
        if parts.len() < 4 {
            continue;
        }
        // parts[0] = package, parts[1] = version, parts[2] = suite, parts[3+] = archs
        let version = parts[1];
        let suite = parts[2];
        if suite == release || suite.starts_with(&with_hyphen) || suite.starts_with(&with_slash) {
            return Some(version.to_owned());
        }
    }
    None
}

/// Compare two Debian version strings using `dpkg --compare-versions`.
///
/// `op` is one of `lt`, `le`, `eq`, `ne`, `ge`, `gt`. Returns `true` when the
/// comparison holds. On any error (e.g. dpkg unavailable), returns `false`.
pub async fn dpkg_compare_versions(a: &str, op: &str, b: &str) -> bool {
    let result = run_command(
        "dpkg",
        &["--compare-versions", a, op, b],
        Path::new("."),
        &[],
    )
    .await;
    match result {
        Ok(o) => o.status.success(),
        Err(ThermiteError::CommandFailed { .. }) => false,
        Err(_) => false,
    }
}

/// Normalize a version floor for comparison. Floors written with a doubled
/// tilde (`1.9.0~~`, meaning "any 1.9.0 and newer") must drop the tildes;
/// a single trailing tilde (`28ubuntu1~`) is meaningful to dpkg and is kept
/// as-is.
fn normalize_floor(version: &str) -> String {
    if version.ends_with("~~") {
        version.trim_end_matches('~').to_owned()
    } else {
        version.to_owned()
    }
}

// ── Outcome evaluation ────────────────────────────────────────────────────────

/// What the source packaging says about how a requirement is met, before any
/// archive lookup.
#[derive(Debug)]
enum PackagingState {
    /// The packaging already satisfies the requirement without the archive.
    /// Non-empty `issues` mark a partially applied fix; `tarball_regen` marks
    /// fixes that require regenerating the orig tarball.
    SelfContained {
        summary: String,
        evidence: Vec<String>,
        issues: Vec<String>,
        tarball_regen: bool,
    },
    /// The package requires `requirement` from the target release's archive.
    Archive {
        requirement: Requirement,
        evidence: Vec<String>,
    },
    /// Evidence is missing or contradictory; the check cannot decide.
    Unknown {
        summary: String,
        evidence: Vec<String>,
    },
}

/// A requirement the target release's archive must satisfy.
#[derive(Debug)]
enum Requirement {
    /// One source package with optional version bounds.
    Single {
        package: String,
        floor: Option<String>,
        ceiling: Option<String>,
    },
    /// Declared alternatives, tried in order (e.g.
    /// `cmake (>= 3.0) | cmake3 (>= 3.0) | cmake-mozilla (>= 3.0)`).
    Alternatives(Vec<(String, Option<String>)>),
}

/// Turn a [`PackagingState`] into the final [`CheckResult`], querying the
/// archive when the packaging points at it.
async fn finalize_check(
    name: &'static str,
    state: PackagingState,
    guidance: &'static str,
    url: String,
    release: &str,
    tarball_on_attention: bool,
) -> CheckResult {
    let (outcome, state_regen) = match state {
        PackagingState::SelfContained {
            summary,
            evidence,
            issues,
            tarball_regen,
        } => {
            let outcome = if issues.is_empty() {
                Outcome {
                    kind: OutcomeKind::SatisfiedByPackaging,
                    summary,
                    evidence,
                }
            } else {
                Outcome {
                    kind: OutcomeKind::NeedsAttention,
                    summary: issues.join("; "),
                    evidence,
                }
            };
            (outcome, tarball_regen)
        }
        PackagingState::Unknown { summary, evidence } => (
            Outcome {
                kind: OutcomeKind::Unknown,
                summary,
                evidence,
            },
            false,
        ),
        PackagingState::Archive {
            requirement,
            evidence,
        } => (
            evaluate_requirement(&requirement, release, evidence).await,
            true,
        ),
    };
    // Archive-driven attention implies a tarball regen only when the check is
    // flagged for it; a partially applied vendoring fix implies a regen only
    // when the Files-Excluded exclusion is still in place.
    let needs_tarball_regen =
        tarball_on_attention && outcome.kind == OutcomeKind::NeedsAttention && state_regen;
    CheckResult {
        name,
        outcome,
        guidance,
        url,
        needs_tarball_regen,
    }
}

/// Compare a requirement against the target release's archive.
async fn evaluate_requirement(
    requirement: &Requirement,
    release: &str,
    evidence: Vec<String>,
) -> Outcome {
    match requirement {
        Requirement::Single {
            package,
            floor,
            ceiling,
        } => {
            let status = check_archive(package, release).await;
            match status {
                ArchiveStatus::CheckFailed(detail) => Outcome {
                    kind: OutcomeKind::ArchiveCheckFailed,
                    summary: detail,
                    evidence,
                },
                ArchiveStatus::NotPublished => Outcome {
                    kind: OutcomeKind::NeedsAttention,
                    summary: format!("{package} is not published in {release}"),
                    evidence,
                },
                ArchiveStatus::Available(available) => {
                    if let Some(floor) = floor.as_deref().filter(|f| !f.is_empty() && *f != "*") {
                        let floor = normalize_floor(floor);
                        if !dpkg_compare_versions(&available, "ge", &floor).await {
                            return Outcome {
                                kind: OutcomeKind::NeedsAttention,
                                summary: format!(
                                    "{package} {available} in {release} is older than the required {floor}"
                                ),
                                evidence,
                            };
                        }
                    }
                    if let Some(ceiling) = ceiling.as_deref().filter(|c| !c.is_empty())
                        && !dpkg_compare_versions(&available, "lt", ceiling).await
                    {
                        return Outcome {
                            kind: OutcomeKind::NeedsAttention,
                            summary: format!(
                                "{package} {available} in {release} is not below the required ceiling {ceiling}"
                            ),
                            evidence,
                        };
                    }
                    Outcome {
                        kind: OutcomeKind::SatisfiedFromArchive,
                        summary: format!("{package} available in {release} ({available})"),
                        evidence,
                    }
                }
            }
        }
        Requirement::Alternatives(alternatives) => {
            let mut too_old: Vec<String> = Vec::new();
            let mut last_failure: Option<String> = None;
            for (package, floor) in alternatives {
                match check_archive(package, release).await {
                    ArchiveStatus::Available(available) => {
                        let satisfied =
                            match floor.as_deref().filter(|f| !f.is_empty() && *f != "*") {
                                Some(f) => {
                                    dpkg_compare_versions(&available, "ge", &normalize_floor(f))
                                        .await
                                }
                                None => true,
                            };
                        if satisfied {
                            return Outcome {
                                kind: OutcomeKind::SatisfiedFromArchive,
                                summary: format!(
                                    "{package} ({available}) satisfies the requirement in {release}"
                                ),
                                evidence,
                            };
                        }
                        too_old.push(format!(
                            "{package} {available} does not meet (>= {})",
                            floor.as_deref().unwrap_or("")
                        ));
                    }
                    ArchiveStatus::NotPublished => {
                        last_failure = Some(format!("{package} is not published in {release}"));
                    }
                    ArchiveStatus::CheckFailed(detail) => {
                        last_failure = Some(detail);
                    }
                }
            }
            if !too_old.is_empty() {
                Outcome {
                    kind: OutcomeKind::NeedsAttention,
                    summary: format!(
                        "none of the declared alternatives is sufficient: {}",
                        too_old.join("; ")
                    ),
                    evidence,
                }
            } else if let Some(detail) = last_failure {
                Outcome {
                    kind: OutcomeKind::ArchiveCheckFailed,
                    summary: detail,
                    evidence,
                }
            } else {
                Outcome {
                    kind: OutcomeKind::Unknown,
                    summary: "no declared alternative could be evaluated".to_owned(),
                    evidence,
                }
            }
        }
    }
}

// ── Per-gate checks ───────────────────────────────────────────────────────────

/// Construct a Launchpad source package URL for `package` in `release`.
fn lp_source_url(release: &str, package: &str) -> String {
    format!("https://launchpad.net/ubuntu/{release}/+source/{package}")
}

/// How the package satisfies its LLVM requirement.
///
/// Vendoring is recognized from real evidence: the bundled `src/llvm-project`
/// source in the tree, the `Files-Excluded` state in `debian/copyright`, and
/// the active build configuration (either `debian/config.toml.in` or the
/// older `debian/bootstrap.toml.in` template). A system LLVM requirement is
/// inferred from an `LLVM_VERSION = <N>` assignment in `debian/rules` or a
/// system `llvm-config` path in the config template.
fn assess_llvm(repo_dir: &Path) -> PackagingState {
    let rules = read_debian_file(repo_dir, "debian/rules");
    let rules_major = rules.as_deref().and_then(rules_llvm_major);

    let config = read_debian_file(repo_dir, "debian/config.toml.in")
        .map(|content| ("debian/config.toml.in".to_owned(), content))
        .or_else(|| {
            read_debian_file(repo_dir, "debian/bootstrap.toml.in")
                .map(|content| ("debian/bootstrap.toml.in".to_owned(), content))
        });
    let config_system = config.as_ref().and_then(|(name, content)| {
        config_system_llvm_config(content).map(|(line, major)| (name.clone(), line, major))
    });
    let source_marker = llvm_source_marker(repo_dir);
    let excluded = files_excluded(repo_dir)
        .map(|fe| fe.contains("src/llvm-project"))
        .unwrap_or(false);
    let rules_reference = rules
        .as_deref()
        .and_then(|rules| find_active_rule_line(rules, "src/llvm-project"));

    if let Some(marker) = source_marker {
        let mut evidence = vec![format!(
            "{marker} present — the bundled LLVM source is in the tree"
        )];
        if excluded {
            evidence
                .push("debian/copyright Files-Excluded still lists src/llvm-project".to_owned());
        } else {
            evidence.push("debian/copyright does not exclude src/llvm-project".to_owned());
        }
        if let Some((lineno, line)) = rules_reference {
            evidence.push(format!("debian/rules:{lineno}: {line}"));
        }
        match &config {
            Some((name, _)) => {
                if config_system.is_none() {
                    evidence.push(format!(
                        "{name} does not point llvm-config at the system toolchain"
                    ));
                }
            }
            None => evidence
                .push("no debian/config.toml.in or debian/bootstrap.toml.in found".to_owned()),
        }

        let mut issues = Vec::new();
        if excluded {
            issues.push(
                "src/llvm-project is still listed in Files-Excluded in debian/copyright; \
                 remove that exclusion so the vendored LLVM ships in the orig tarball"
                    .to_owned(),
            );
        }
        if let Some((name, line, _major)) = &config_system {
            issues.push(format!(
                "{name} still points llvm-config at the system toolchain ({line}); \
                 remove that line so the build uses the bundled LLVM"
            ));
        }
        return PackagingState::SelfContained {
            summary: "already vendored — build uses the bundled src/llvm-project tree".to_owned(),
            evidence,
            issues,
            tarball_regen: excluded,
        };
    }

    // No bundled source: does the package require a system LLVM?
    if let Some(major) = rules_major {
        return PackagingState::Archive {
            requirement: Requirement::Single {
                package: format!("llvm-toolchain-{major}"),
                floor: Some(major.clone()),
                ceiling: None,
            },
            evidence: vec![format!("debian/rules: LLVM_VERSION = {major}")],
        };
    }
    if let Some((name, line, major)) = config_system {
        return PackagingState::Archive {
            requirement: Requirement::Single {
                package: format!("llvm-toolchain-{major}"),
                floor: Some(major.clone()),
                ceiling: None,
            },
            evidence: vec![format!("{name}: {line}")],
        };
    }

    // Neither side has usable evidence — report what was and was not found.
    let mut evidence = vec![
        "system LLVM: no uncommented 'LLVM_VERSION = <N>' assignment in debian/rules".to_owned(),
        "vendored LLVM: no src/llvm-project/llvm/CMakeLists.txt (or \
         src/llvm-project/cmake/Modules/LLVMVersion.cmake) in the tree"
            .to_owned(),
    ];
    match &config {
        Some((name, _)) => evidence.push(format!(
            "system LLVM: {name} has no llvm-config line pointing at /usr"
        )),
        None => {
            evidence.push("no debian/config.toml.in or debian/bootstrap.toml.in found".to_owned())
        }
    }
    if let Some(fe) = files_excluded(repo_dir)
        && !fe.contains("src/llvm-project")
    {
        evidence.push(
            "note: debian/copyright does not exclude src/llvm-project, but that alone \
             does not establish that the source is present"
                .to_owned(),
        );
    }
    PackagingState::Unknown {
        summary: "cannot tell whether LLVM is used from the archive or vendored".to_owned(),
        evidence,
    }
}

/// Check whether the LLVM version required by the package is available in the
/// target release's archive, or whether LLVM is already vendored.
pub async fn check_llvm(repo_dir: &Path, release: &str) -> CheckResult {
    let state = assess_llvm(repo_dir);
    let url = match &state {
        PackagingState::Archive {
            requirement: Requirement::Single { package, .. },
            ..
        } => lp_source_url(release, package),
        _ => String::new(),
    };
    let guidance = "Vendor LLVM: remove src/llvm-project from Files-Excluded \
                    in debian/copyright, regenerate the orig tarball, update \
                    debian/control and the bootstrap config template per the \
                    backporting guide.";
    finalize_check("LLVM", state, guidance, url, release, true).await
}

/// How the package satisfies its libgit2 requirement.
///
/// Vendoring is recognized from the bundled `vendor/libgit2-sys-*/libgit2` C
/// source, the `Files-Excluded` state, removal of `libgit2-dev` and
/// `libhttp-parser-dev` from Build-Depends, and whether the cargo
/// `vendored-libgit2` feature survives the active quilt patches.
fn assess_libgit2(repo_dir: &Path) -> PackagingState {
    let Some(field) = build_depends_field(repo_dir) else {
        return PackagingState::Unknown {
            summary: "debian/control could not be read".to_owned(),
            evidence: vec!["debian/control is missing or unreadable".to_owned()],
        };
    };
    let entries = split_entries(&field);
    let dev_entries = entries_for(&entries, "libgit2-dev");
    let http_parser = !entries_for(&entries, "libhttp-parser-dev").is_empty();

    if !dev_entries.is_empty() {
        let mut evidence: Vec<String> = dev_entries
            .iter()
            .map(|entry| format!("debian/control: {entry}"))
            .collect();
        if http_parser {
            evidence.push("debian/control: libhttp-parser-dev in Build-Depends".to_owned());
        }
        let vendored = vendored_libgit2_crates(repo_dir);
        if !vendored.is_empty() {
            evidence.push(format!(
                "note: bundled libgit2 source also present ({}) while libgit2-dev is still declared",
                vendored.join(", ")
            ));
        }
        let mut floor = None;
        let mut ceiling = None;
        for entry in &dev_entries {
            for alt in entry.split('|') {
                if let Some((name, relations)) = parse_alternative(alt) {
                    if name != "libgit2-dev" {
                        continue;
                    }
                    let (entry_floor, entry_ceiling) = bounds_from_relations(&relations);
                    floor = entry_floor.or(floor);
                    ceiling = entry_ceiling.or(ceiling);
                }
            }
        }
        return PackagingState::Archive {
            requirement: Requirement::Single {
                package: "libgit2".to_owned(),
                floor,
                ceiling,
            },
            evidence,
        };
    }

    // libgit2-dev is not declared — is the bundled source used instead?
    let vendored = vendored_libgit2_crates(repo_dir);
    if vendored.is_empty() {
        let mut evidence = vec![
            "system libgit2: no libgit2-dev entry in Build-Depends".to_owned(),
            "vendored libgit2: no vendor/libgit2-sys-*/libgit2 source in the tree".to_owned(),
        ];
        if http_parser {
            evidence.push("debian/control: libhttp-parser-dev still in Build-Depends".to_owned());
        }
        return PackagingState::Unknown {
            summary: "cannot tell how libgit2 is provided".to_owned(),
            evidence,
        };
    }

    let excluded = files_excluded(repo_dir)
        .map(|fe| fe.contains("libgit2-sys"))
        .unwrap_or(false);
    let (added, removed) = vendored_libgit2_patch_effects(repo_dir);
    let tree_feature = cargo_feature_vendored_libgit2_in_tree(repo_dir);
    let feature_effective = !added.is_empty() || (tree_feature && removed.is_empty());

    let mut evidence = vec![
        format!("bundled libgit2 source present for {}", vendored.join(", ")),
        if excluded {
            "debian/copyright Files-Excluded still lists vendor/libgit2-sys-*/libgit2".to_owned()
        } else {
            "debian/copyright does not exclude vendor/libgit2-sys-*/libgit2".to_owned()
        },
        "libgit2-dev is not in Build-Depends".to_owned(),
    ];
    let mut issues = Vec::new();
    if excluded {
        issues.push(
            "vendor/libgit2-sys-*/libgit2 is still listed in Files-Excluded in \
             debian/copyright; remove that exclusion so the vendored libgit2 \
             ships in the orig tarball"
                .to_owned(),
        );
    }
    if !feature_effective {
        issues.push(
            "the cargo vendored-libgit2 feature is not enabled for the build; keep it in \
             src/tools/cargo/Cargo.toml and make sure no active quilt patch removes it"
                .to_owned(),
        );
    }
    if http_parser {
        issues.push(
            "libhttp-parser-dev is still in Build-Depends; remove it (it is bundled \
             within the vendored libgit2 source)"
                .to_owned(),
        );
    } else {
        evidence.push("libhttp-parser-dev is not in Build-Depends".to_owned());
    }
    if !removed.is_empty() {
        evidence.push(format!(
            "active patch(es) remove the vendored-libgit2 feature: {}",
            removed.join(", ")
        ));
    } else if !added.is_empty() {
        evidence.push(format!(
            "active patch(es) re-add the vendored-libgit2 feature: {}",
            added.join(", ")
        ));
    } else if tree_feature {
        evidence.push("vendored-libgit2 feature present in src/tools/cargo/Cargo.toml".to_owned());
    }

    PackagingState::SelfContained {
        summary: "already vendored — libgit2 is built from the bundled vendor/libgit2-sys-* source"
            .to_owned(),
        evidence,
        issues,
        tarball_regen: excluded,
    }
}

/// Check whether the libgit2-dev version required by the package is available
/// in the target release's archive, or whether libgit2 is already vendored.
pub async fn check_libgit2(repo_dir: &Path, release: &str) -> CheckResult {
    let state = assess_libgit2(repo_dir);
    let url = match &state {
        PackagingState::Archive { .. } => lp_source_url(release, "libgit2"),
        _ => String::new(),
    };
    let guidance = "Either downgrade the libgit2-dev version constraint in \
                    debian/control and debian/control.in (when the archive \
                    version is API-compatible), or vendor libgit2 (comment \
                    out the libgit2 exclusion in debian/copyright, regenerate \
                    the orig tarball, remove libgit2-dev from Build-Depends).";
    finalize_check("libgit2", state, guidance, url, release, true).await
}

/// How the package satisfies its dh-cargo requirement.
///
/// Disabling dh-cargo (the documented fix for older releases) is recognized
/// only when both the Build-Depends entry and the
/// `dh-cargo-vendored-sources` invocation in `debian/rules` are gone; a
/// bundled copy of the helper script does not cancel a declared
/// Build-Depends.
fn assess_dh_cargo(repo_dir: &Path) -> PackagingState {
    let Some(field) = build_depends_field(repo_dir) else {
        return PackagingState::Unknown {
            summary: "debian/control could not be read".to_owned(),
            evidence: vec!["debian/control is missing or unreadable".to_owned()],
        };
    };
    let entries = split_entries(&field);
    let invocation = read_debian_file(repo_dir, "debian/rules")
        .as_deref()
        .and_then(|rules| find_active_rule_line(rules, "dh-cargo-vendored-sources"));

    match entries_for(&entries, "dh-cargo").first() {
        Some(entry) => {
            let mut evidence = vec![format!("debian/control: {entry}")];
            if let Some((lineno, line)) = &invocation {
                evidence.push(format!("debian/rules:{lineno}: {line}"));
            }
            let floor = parse_alternative(entry)
                .and_then(|(_, relations)| bounds_from_relations(&relations).0);
            PackagingState::Archive {
                requirement: Requirement::Single {
                    package: "dh-cargo".to_owned(),
                    floor,
                    ceiling: None,
                },
                evidence,
            }
        }
        None => {
            let mut evidence =
                vec!["debian/control: no dh-cargo entry in Build-Depends".to_owned()];
            let mut issues = Vec::new();
            match &invocation {
                Some((lineno, line)) => issues.push(format!(
                    "dh-cargo was removed from Build-Depends but debian/rules:{lineno} still \
                     invokes the vendored-sources check ({line}); remove that invocation"
                )),
                None => evidence.push(
                    "debian/rules: no active dh-cargo-vendored-sources invocation".to_owned(),
                ),
            }
            PackagingState::SelfContained {
                summary: "already disabled — the build does not require dh-cargo".to_owned(),
                evidence,
                issues,
                tarball_regen: false,
            }
        }
    }
}

/// Check whether dh-cargo is available (new enough) in the target release, or
/// whether it has already been disabled.
pub async fn check_dh_cargo(repo_dir: &Path, release: &str) -> CheckResult {
    let state = assess_dh_cargo(repo_dir);
    let url = match &state {
        PackagingState::Archive { .. } => lp_source_url(release, "dh-cargo"),
        _ => String::new(),
    };
    let guidance = "Comment out dh-cargo from Build-Depends in debian/control \
                    and debian/control.in; remove the dh-cargo-vendored-sources \
                    check from debian/rules.";
    finalize_check("dh-cargo", state, guidance, url, release, false).await
}

/// How the package satisfies its pkg-config requirement.
///
/// The documented fallback (replacing `pkgconf` with `pkg-config`) is
/// recognized when `pkg-config` is declared in Build-Depends and
/// `debian/rules` exports `PKG_CONFIG=pkg-config`; a missing export is a
/// half-applied fix.
fn assess_pkgconf(repo_dir: &Path) -> PackagingState {
    let Some(field) = build_depends_field(repo_dir) else {
        return PackagingState::Unknown {
            summary: "debian/control could not be read".to_owned(),
            evidence: vec!["debian/control is missing or unreadable".to_owned()],
        };
    };
    let entries = split_entries(&field);
    let pkgconf_entries = entries_for(&entries, "pkgconf");
    let pkg_config_entries = entries_for(&entries, "pkg-config");

    match (pkgconf_entries.is_empty(), pkg_config_entries.is_empty()) {
        (false, false) => PackagingState::Unknown {
            summary: "both pkgconf and pkg-config appear in Build-Depends; remove pkgconf if the \
                 pkg-config fallback was intended"
                .to_owned(),
            evidence: pkgconf_entries
                .iter()
                .chain(pkg_config_entries.iter())
                .map(|entry| format!("debian/control: {entry}"))
                .collect(),
        },
        (false, true) => PackagingState::Archive {
            requirement: Requirement::Single {
                package: "pkgconf".to_owned(),
                floor: None,
                ceiling: None,
            },
            evidence: pkgconf_entries
                .iter()
                .map(|entry| format!("debian/control: {entry}"))
                .collect(),
        },
        (true, false) => {
            let export = read_debian_file(repo_dir, "debian/rules")
                .as_deref()
                .and_then(pkg_config_export_line);
            let evidence: Vec<String> = pkg_config_entries
                .iter()
                .map(|entry| format!("debian/control: {entry}"))
                .collect();
            match export {
                Some((lineno, line)) => {
                    let mut evidence = evidence;
                    evidence.push(format!("debian/rules:{lineno}: {line}"));
                    PackagingState::SelfContained {
                        summary: "already switched to pkg-config".to_owned(),
                        evidence,
                        issues: Vec::new(),
                        tarball_regen: false,
                    }
                }
                None => PackagingState::SelfContained {
                    summary: "pkg-config is declared but debian/rules does not export \
                         PKG_CONFIG=pkg-config"
                        .to_owned(),
                    evidence,
                    issues: vec![
                        "add 'export PKG_CONFIG=pkg-config' to debian/rules so Cargo uses \
                         pkg-config instead of pkgconf"
                            .to_owned(),
                    ],
                    tarball_regen: false,
                },
            }
        }
        (true, true) => PackagingState::Unknown {
            summary: "neither pkgconf nor pkg-config found in debian/control Build-Depends"
                .to_owned(),
            evidence: Vec::new(),
        },
    }
}

/// Check whether pkgconf is available in the target release's archive, or
/// whether the pkg-config fallback has already been applied.
pub async fn check_pkgconf(repo_dir: &Path, release: &str) -> CheckResult {
    let state = assess_pkgconf(repo_dir);
    let url = match &state {
        PackagingState::Archive { .. } => lp_source_url(release, "pkgconf"),
        _ => String::new(),
    };
    let guidance = "Replace pkgconf with pkg-config in debian/control and \
                    debian/control.in; add 'export PKG_CONFIG=pkg-config' to \
                    debian/rules.";
    finalize_check("pkgconf", state, guidance, url, release, false).await
}

/// How the package satisfies its cmake requirement: the declared alternatives
/// (`cmake`, `cmake3`, `cmake-mozilla`) are evaluated in order, each with its
/// own version constraint.
fn assess_cmake(repo_dir: &Path) -> PackagingState {
    let Some(field) = build_depends_field(repo_dir) else {
        return PackagingState::Unknown {
            summary: "debian/control could not be read".to_owned(),
            evidence: vec!["debian/control is missing or unreadable".to_owned()],
        };
    };
    let entries = split_entries(&field);
    let is_cmake_provider = |name: &str| matches!(name, "cmake" | "cmake3" | "cmake-mozilla");
    let entry = entries.iter().find(|entry| {
        entry.split('|').any(
            |alt| matches!(parse_alternative(alt), Some((ref name, _)) if is_cmake_provider(name)),
        )
    });
    let Some(entry) = entry else {
        return PackagingState::Unknown {
            summary: "no cmake entry found in debian/control Build-Depends".to_owned(),
            evidence: Vec::new(),
        };
    };
    let mut alternatives = Vec::new();
    for alt in entry.split('|') {
        if let Some((name, relations)) = parse_alternative(alt)
            && is_cmake_provider(&name)
        {
            let (floor, _) = bounds_from_relations(&relations);
            alternatives.push((name, floor));
        }
    }
    PackagingState::Archive {
        requirement: Requirement::Alternatives(alternatives),
        evidence: vec![format!("debian/control: {entry}")],
    }
}

/// Check whether one of the declared cmake alternatives is available in the
/// target release's archive.
pub async fn check_cmake(repo_dir: &Path, release: &str) -> CheckResult {
    let state = assess_cmake(repo_dir);
    let url = match &state {
        PackagingState::Archive {
            requirement: Requirement::Alternatives(alternatives),
            ..
        } => alternatives
            .first()
            .map(|(package, _)| lp_source_url(release, package))
            .unwrap_or_default(),
        _ => String::new(),
    };
    let guidance = "Add cmake-mozilla (>= 3.0) as a fallback cmake provider in \
                    debian/control and debian/control.in.";
    finalize_check("cmake", state, guidance, url, release, false).await
}

/// How the package satisfies its debhelper-compat requirement.
fn assess_debhelper_compat(repo_dir: &Path) -> PackagingState {
    let Some(field) = build_depends_field(repo_dir) else {
        return PackagingState::Unknown {
            summary: "debian/control could not be read".to_owned(),
            evidence: vec!["debian/control is missing or unreadable".to_owned()],
        };
    };
    let entries = split_entries(&field);
    let Some(entry) = entries_for(&entries, "debhelper-compat").into_iter().next() else {
        return PackagingState::Unknown {
            summary: "no debhelper-compat entry found in debian/control Build-Depends".to_owned(),
            evidence: Vec::new(),
        };
    };
    let floor =
        parse_alternative(&entry).and_then(|(_, relations)| bounds_from_relations(&relations).0);
    PackagingState::Archive {
        requirement: Requirement::Single {
            package: "debhelper".to_owned(),
            floor,
            ceiling: None,
        },
        evidence: vec![format!("debian/control: {entry}")],
    }
}

/// Check whether the required debhelper-compat level is supported by the
/// debhelper in the target release's archive. When attention is needed, also
/// report `.install.in` files still using `${env:...}` substitution, which
/// must be converted when downgrading the compat level.
pub async fn check_debhelper_compat(repo_dir: &Path, release: &str) -> CheckResult {
    let state = assess_debhelper_compat(repo_dir);
    let guidance = "Downgrade the debhelper-compat level in debian/control and \
                    debian/control.in, and update .install.in substitution \
                    variables to match the lower compat level.";
    let mut result = finalize_check(
        "debhelper-compat",
        state,
        guidance,
        lp_source_url(release, "debhelper"),
        release,
        false,
    )
    .await;
    if result.outcome.kind == OutcomeKind::NeedsAttention {
        let leftovers = install_in_env_substitutions(repo_dir);
        if leftovers.is_empty() {
            result
                .outcome
                .evidence
                .push("no debian/*.install.in file uses ${env:...} substitution".to_owned());
        } else {
            for file in leftovers {
                result.outcome.evidence.push(format!(
                    "{file} still uses ${{env:...}} substitution; switch it to \
                     @VARIABLE@ when downgrading the compat level"
                ));
            }
        }
    }
    result
}

/// Run all six compatibility checks and return the results in display order.
pub async fn run_all_checks(repo_dir: &Path, release: &str) -> Vec<CheckResult> {
    vec![
        check_llvm(repo_dir, release).await,
        check_libgit2(repo_dir, release).await,
        check_dh_cargo(repo_dir, release).await,
        check_pkgconf(repo_dir, release).await,
        check_cmake(repo_dir, release).await,
        check_debhelper_compat(repo_dir, release).await,
    ]
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_repo() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "thermite-compat-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos()
        ));
        fs::create_dir_all(dir.join("debian")).unwrap();
        dir
    }

    fn write(repo: &std::path::Path, name: &str, content: &str) {
        fs::create_dir_all(repo.join(name).parent().unwrap()).unwrap();
        fs::write(repo.join(name), content).unwrap();
    }

    // ── parse_rmadison_version ─────────────────────────────────────────────

    #[test]
    fn parse_rmadison_version_picks_matching_suite() {
        let out = "\
rustc | 1.85.0+dfsg1~24.04-0ubuntu1~24.04.1 | noble-security | source
rustc | 1.85.0+dfsg1~24.04-0ubuntu1~24.04.1 | noble-updates   | source
rustc | 1.85.0+dfsg1-0ubuntu1               | noble           | source
";
        let v = parse_rmadison_version(out, "noble");
        assert_eq!(v.as_deref(), Some("1.85.0+dfsg1~24.04-0ubuntu1~24.04.1"));
    }

    #[test]
    fn parse_rmadison_version_returns_none_when_not_published() {
        let out = "\
rustc | 1.85.0+dfsg1-0ubuntu1 | oracular | source
";
        assert!(parse_rmadison_version(out, "jammy").is_none());
    }

    #[test]
    fn parse_rmadison_version_skips_blank_and_warning_lines() {
        let out = "\
warning: some network hiccup
\n\
rustc | 1.85.0+dfsg1-0ubuntu1 | jammy | source
";
        let v = parse_rmadison_version(out, "jammy");
        assert_eq!(v.as_deref(), Some("1.85.0+dfsg1-0ubuntu1"));
    }

    #[test]
    fn parse_rmadison_version_handles_empty_output() {
        assert!(parse_rmadison_version("", "jammy").is_none());
    }

    #[test]
    fn parse_rmadison_version_handles_component_suffix() {
        // rmadison emits the suite as `<suite>/<component>` rather than a bare
        // suite name (e.g. `resolute/universe`, `stonking-proposed/universe`).
        // Regression test for LLVM on Resolute, where the previous parser only
        // matched `<release>` and `<release>-*` and so reported NotPublished.
        let out = "\
llvm-toolchain-22 | 1:22.1.2-1ubuntu1 | resolute/universe          | source
llvm-toolchain-22 | 1:22.1.6-1ubuntu1 | stonking/universe          | source
llvm-toolchain-22 | 1:22.1.6-1ubuntu2 | stonking-proposed/universe | source
";
        let v = parse_rmadison_version(out, "resolute");
        assert_eq!(v.as_deref(), Some("1:22.1.2-1ubuntu1"));
    }

    #[test]
    fn parse_rmadison_version_component_suffix_for_pocket() {
        // A pocket with a component (e.g. `resolute-proposed/universe`)
        // should still match `release = "resolute"` via the `<release>-`
        // rule, regardless of the trailing `/component`.
        let out = "\
llvm-toolchain-22 | 1:22.1.6-1ubuntu2 | resolute-proposed/universe | source
llvm-toolchain-22 | 1:22.1.2-1ubuntu1 | resolute/universe          | source
";
        let v = parse_rmadison_version(out, "resolute");
        assert_eq!(v.as_deref(), Some("1:22.1.6-1ubuntu2"));
    }

    #[test]
    fn parse_rmadison_version_component_suffix_does_not_cross_release() {
        // `stonking/universe` must not match `release = "resolute"`,
        // nor vice versa, even though both carry `/universe`.
        let out = "\
llvm-toolchain-22 | 1:22.1.2-1ubuntu1 | resolute/universe | source
llvm-toolchain-22 | 1:22.1.6-1ubuntu1 | stonking/universe | source
";
        assert_eq!(
            parse_rmadison_version(out, "resolute").as_deref(),
            Some("1:22.1.2-1ubuntu1")
        );
        assert_eq!(
            parse_rmadison_version(out, "stonking").as_deref(),
            Some("1:22.1.6-1ubuntu1")
        );
    }

    // ── packaging readers ──────────────────────────────────────────────────

    #[test]
    fn folded_field_joins_continuation_lines() {
        let content =
            "Source: rustc-1.85\nBuild-Depends:\n a,\n b (>= 1),\nStandards-Version: 4.6\n";
        assert_eq!(
            folded_field(content, "Build-Depends").as_deref(),
            Some("a, b (>= 1),")
        );
    }

    #[test]
    fn folded_field_handles_same_line_value() {
        let content = "Build-Depends: debhelper (>= 9), pkgconf\nArchitecture: any\n";
        assert_eq!(
            folded_field(content, "Build-Depends").as_deref(),
            Some("debhelper (>= 9), pkgconf")
        );
    }

    #[test]
    fn folded_field_finds_files_excluded() {
        let content = "Format: ...Files-Excluded-Compressed: xz\nFiles-Excluded:\n src/llvm-project\n vendor/openssl-src-*\nComment: x\n";
        assert_eq!(
            folded_field(content, "Files-Excluded").as_deref(),
            Some("src/llvm-project vendor/openssl-src-*")
        );
    }

    #[test]
    fn folded_field_skips_comments_inside_the_field() {
        // debian/control routinely carries full-line comments inside a folded
        // field; they must not terminate it.
        let content =
            "Build-Depends:\n debhelper (>= 9),\n# needed by some vendor crates\n pkgconf,\n";
        assert_eq!(
            folded_field(content, "Build-Depends").as_deref(),
            Some("debhelper (>= 9), pkgconf,")
        );
    }

    #[test]
    fn folded_field_returns_none_when_absent() {
        assert!(folded_field("Source: x\n", "Build-Depends").is_none());
    }

    #[test]
    fn split_entries_ignores_commas_in_parens() {
        let entries = split_entries("cmake (>= 3.0, << 4), pkgconf, python3:native");
        assert_eq!(
            entries,
            ["cmake (>= 3.0, << 4)", "pkgconf", "python3:native"]
        );
    }

    #[test]
    fn parse_alternative_reads_relations() {
        let (name, relations) = parse_alternative("libgit2-dev (>= 1.9.0~~)").expect("parsed");
        assert_eq!(name, "libgit2-dev");
        assert_eq!(relations, [(">=".to_owned(), "1.9.0~~".to_owned())]);
    }

    #[test]
    fn parse_alternative_ignores_annotations() {
        let (name, relations) = parse_alternative("cargo-1.96 <!nocheck>").expect("parsed");
        assert_eq!(name, "cargo-1.96");
        assert!(relations.is_empty());
    }

    #[test]
    fn normalize_floor_keeps_single_tilde_and_strips_double() {
        assert_eq!(normalize_floor("1.9.0~~"), "1.9.0");
        assert_eq!(normalize_floor("28ubuntu1~"), "28ubuntu1~");
        assert_eq!(normalize_floor("13"), "13");
        assert_eq!(normalize_floor("*"), "*");
    }

    #[test]
    fn rules_llvm_major_accepts_make_assignments() {
        assert_eq!(
            rules_llvm_major("#!/usr/bin/make -f\nLLVM_VERSION = 19\nOLD_LLVM_VERSION = 18\n"),
            Some("19".to_owned())
        );
        assert_eq!(
            rules_llvm_major("LLVM_VERSION := 19\n"),
            Some("19".to_owned())
        );
        assert_eq!(
            rules_llvm_major("LLVM_VERSION ?= 19\n"),
            Some("19".to_owned())
        );
        assert_eq!(rules_llvm_major("# LLVM_VERSION = 19\n"), None);
        assert_eq!(rules_llvm_major("LLVM_VERSION_MAJOR := 19\n"), None);
        assert_eq!(rules_llvm_major("LLVM_VERSION = $(shell echo 19)\n"), None);
    }

    #[test]
    fn llvm_major_from_llvm_config_reads_both_path_shapes() {
        assert_eq!(
            llvm_major_from_llvm_config(r#"llvm-config = "/usr/lib/llvm-18/bin/llvm-config""#),
            Some("18".to_owned())
        );
        assert_eq!(
            llvm_major_from_llvm_config(r#"llvm-config = "/usr/bin/llvm-config-19""#),
            Some("19".to_owned())
        );
        // Template placeholders carry no version digits.
        assert_eq!(
            llvm_major_from_llvm_config(
                r#"llvm-config = "LLVM_DESTDIR/usr/lib/llvm-LLVM_VERSION/bin/llvm-config""#
            ),
            None
        );
    }

    #[test]
    fn pkg_config_export_line_matches_export_and_ignores_other_vars() {
        let rules = "export LIBSSH2_SYS_USE_PKG_CONFIG=1\nexport PKG_CONFIG=pkg-config\n";
        let (lineno, line) = pkg_config_export_line(rules).expect("found");
        assert_eq!(lineno, 2);
        assert!(line.contains("PKG_CONFIG=pkg-config"));
        assert!(pkg_config_export_line("export LIBSSH2_SYS_USE_PKG_CONFIG=1\n").is_none());
        assert!(pkg_config_export_line("# export PKG_CONFIG=pkg-config\n").is_none());
    }

    // ── word-boundary helpers ──────────────────────────────────────────────

    #[test]
    fn contains_word_matches_bare_word() {
        assert!(contains_word(" pkgconf,", "pkgconf"));
        assert!(contains_word("pkgconf", "pkgconf"));
        assert!(contains_word(" pkgconf ", "pkgconf"));
    }

    #[test]
    fn contains_word_does_not_match_substring() {
        // `pkg-config` should not match `pkgconf` because `-` is not a word
        // boundary.
        assert!(!contains_word("pkg-config", "pkgconf"));
        assert!(!contains_word("cmake-mozilla", "cmake"));
        assert!(!contains_word("cmake3", "cmake"));
        assert!(!contains_word("libgit2-dev", "libgit2"));
    }

    #[test]
    fn find_word_returns_index() {
        assert_eq!(find_word(" pkgconf,", "pkgconf"), Some(1));
        assert_eq!(find_word("pkg-config", "pkgconf"), None);
    }

    // ── LLVM ───────────────────────────────────────────────────────────────

    #[test]
    fn assess_llvm_vendored_is_satisfied() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/rules",
            "LLVM_MAJOR := $(shell sed -n 's/x/x/' \\\n  $(CURDIR)/src/llvm-project/cmake/Modules/LLVMVersion.cmake)\n",
        );
        write(
            &repo,
            "debian/copyright",
            "Files-Excluded:\n vendor/openssl-src-*\n",
        );
        write(
            &repo,
            "debian/bootstrap.toml.in",
            "download-ci-llvm = false\n",
        );
        write(&repo, "src/llvm-project/llvm/CMakeLists.txt", "cmake\n");
        match assess_llvm(&repo) {
            PackagingState::SelfContained {
                summary,
                evidence,
                issues,
                tarball_regen,
            } => {
                assert!(issues.is_empty());
                assert!(!tarball_regen);
                assert!(summary.contains("already vendored"));
                assert!(evidence.iter().any(|e| e.contains("CMakeLists.txt")));
                assert!(evidence.iter().any(|e| e.contains("bootstrap.toml.in")));
            }
            other => panic!("expected SelfContained, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn assess_llvm_vendored_but_excluded_needs_attention() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/copyright",
            "Files-Excluded:\n src/llvm-project\n",
        );
        write(&repo, "src/llvm-project/llvm/CMakeLists.txt", "cmake\n");
        match assess_llvm(&repo) {
            PackagingState::SelfContained {
                issues,
                tarball_regen,
                ..
            } => {
                assert_eq!(issues.len(), 1);
                assert!(issues[0].contains("Files-Excluded"));
                assert!(tarball_regen);
            }
            other => panic!("expected SelfContained, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn assess_llvm_unknown_reports_both_sides() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/copyright",
            "Files-Excluded:\n vendor/openssl-src-*\n",
        );
        match assess_llvm(&repo) {
            PackagingState::Unknown { evidence, .. } => {
                let joined = evidence.join("\n");
                assert!(joined.contains("LLVM_VERSION"));
                assert!(joined.contains("src/llvm-project"));
                assert!(joined.contains("does not establish"));
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn assess_llvm_system_from_rules() {
        let repo = temp_repo();
        write(&repo, "debian/rules", "LLVM_VERSION = 19\n");
        match assess_llvm(&repo) {
            PackagingState::Archive {
                requirement,
                evidence,
            } => {
                match requirement {
                    Requirement::Single {
                        package,
                        floor,
                        ceiling,
                    } => {
                        assert_eq!(package, "llvm-toolchain-19");
                        assert_eq!(floor.as_deref(), Some("19"));
                        assert!(ceiling.is_none());
                    }
                    other => panic!("expected Single, got {other:?}"),
                }
                assert!(evidence.iter().any(|e| e.contains("LLVM_VERSION = 19")));
            }
            other => panic!("expected Archive, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn assess_llvm_system_from_config_template() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/config.toml.in",
            "llvm-config = \"/usr/lib/llvm-18/bin/llvm-config\"\n",
        );
        match assess_llvm(&repo) {
            PackagingState::Archive {
                requirement,
                evidence,
            } => {
                match requirement {
                    Requirement::Single { package, .. } => {
                        assert_eq!(package, "llvm-toolchain-18");
                    }
                    other => panic!("expected Single, got {other:?}"),
                }
                assert!(evidence.iter().any(|e| e.contains("config.toml.in")));
            }
            other => panic!("expected Archive, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&repo);
    }

    // ── libgit2 ────────────────────────────────────────────────────────────

    #[test]
    fn assess_libgit2_vendored_is_satisfied() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/control",
            "Build-Depends:\n debhelper (>= 9),\n pkgconf,\n",
        );
        write(
            &repo,
            "debian/copyright",
            "Files-Excluded:\n vendor/libssh2-sys-*/libssh2\n",
        );
        write(
            &repo,
            "src/tools/cargo/Cargo.toml",
            "[features]\nvendored-libgit2 = [\"libgit2-sys/vendored\"]\n",
        );
        write(
            &repo,
            "vendor/libgit2-sys-0.18.2+1.9.1/libgit2/CMakeLists.txt",
            "cmake\n",
        );
        write(
            &repo,
            "vendor/libgit2-sys-0.18.3+1.9.2/libgit2/CMakeLists.txt",
            "cmake\n",
        );
        match assess_libgit2(&repo) {
            PackagingState::SelfContained {
                summary,
                evidence,
                issues,
                tarball_regen,
            } => {
                assert!(issues.is_empty());
                assert!(!tarball_regen);
                assert!(summary.contains("already vendored"));
                let joined = evidence.join("\n");
                assert!(joined.contains("0.18.2+1.9.1"));
                assert!(joined.contains("0.18.3+1.9.2"));
                assert!(joined.contains("does not exclude"));
                assert!(joined.contains("libhttp-parser-dev is not in Build-Depends"));
            }
            other => panic!("expected SelfContained, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn assess_libgit2_vendored_but_excluded_needs_attention() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/control",
            "Build-Depends:\n debhelper (>= 9),\n",
        );
        write(
            &repo,
            "debian/copyright",
            "Files-Excluded:\n vendor/libgit2-sys-*/libgit2\n",
        );
        write(
            &repo,
            "vendor/libgit2-sys-0.18.2+1.9.1/libgit2/CMakeLists.txt",
            "cmake\n",
        );
        match assess_libgit2(&repo) {
            PackagingState::SelfContained {
                issues,
                tarball_regen,
                ..
            } => {
                assert!(issues.iter().any(|i| i.contains("Files-Excluded")));
                assert!(tarball_regen);
            }
            other => panic!("expected SelfContained, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn assess_libgit2_feature_stripped_by_patch_needs_attention() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/control",
            "Build-Depends:\n debhelper (>= 9),\n",
        );
        write(
            &repo,
            "src/tools/cargo/Cargo.toml",
            "[features]\nvendored-libgit2 = [\"libgit2-sys/vendored\"]\n",
        );
        write(&repo, "debian/patches/series", "ubuntu/x.patch\n");
        write(
            &repo,
            "debian/patches/ubuntu/x.patch",
            "--- a/src/tools/cargo/Cargo.toml\n+++ b/src/tools/cargo/Cargo.toml\n-vendored-libgit2 = [\"libgit2-sys/vendored\"]\n",
        );
        write(
            &repo,
            "vendor/libgit2-sys-0.18.2+1.9.1/libgit2/CMakeLists.txt",
            "cmake\n",
        );
        match assess_libgit2(&repo) {
            PackagingState::SelfContained {
                evidence, issues, ..
            } => {
                assert!(issues.iter().any(|i| i.contains("vendored-libgit2")));
                assert!(evidence.iter().any(|e| e.contains("ubuntu/x.patch")));
            }
            other => panic!("expected SelfContained, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn assess_libgit2_unknown_reports_both_sides() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/control",
            "Build-Depends:\n debhelper (>= 9),\n",
        );
        match assess_libgit2(&repo) {
            PackagingState::Unknown { evidence, .. } => {
                let joined = evidence.join("\n");
                assert!(joined.contains("libgit2-dev"));
                assert!(joined.contains("vendor/libgit2-sys"));
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn assess_libgit2_system_reads_both_bounds() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/control",
            "Build-Depends:\n libgit2-dev (>= 1.9.0~~),\n libgit2-dev (<< 1.10~~),\n",
        );
        match assess_libgit2(&repo) {
            PackagingState::Archive { requirement, .. } => match requirement {
                Requirement::Single {
                    package,
                    floor,
                    ceiling,
                } => {
                    assert_eq!(package, "libgit2");
                    assert_eq!(floor.as_deref(), Some("1.9.0~~"));
                    assert_eq!(ceiling.as_deref(), Some("1.10~~"));
                }
                other => panic!("expected Single, got {other:?}"),
            },
            other => panic!("expected Archive, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&repo);
    }

    // ── dh-cargo ───────────────────────────────────────────────────────────

    #[test]
    fn assess_dh_cargo_disabled_is_satisfied() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/control",
            "Build-Depends:\n debhelper (>= 9),\n",
        );
        write(
            &repo,
            "debian/rules",
            "# CARGO_BIN=... CARGO_VENDOR_DIR=... debian/dh-cargo-vendored-sources\n",
        );
        match assess_dh_cargo(&repo) {
            PackagingState::SelfContained {
                summary,
                evidence,
                issues,
                ..
            } => {
                assert!(issues.is_empty());
                assert!(summary.contains("already disabled"));
                let joined = evidence.join("\n");
                assert!(joined.contains("no dh-cargo entry"));
                assert!(joined.contains("no active dh-cargo-vendored-sources"));
            }
            other => panic!("expected SelfContained, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn assess_dh_cargo_half_disabled_needs_attention() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/control",
            "Build-Depends:\n debhelper (>= 9),\n",
        );
        write(
            &repo,
            "debian/rules",
            "CARGO_BIN=\"x\" CARGO_VENDOR_DIR=y debian/dh-cargo-vendored-sources\n",
        );
        match assess_dh_cargo(&repo) {
            PackagingState::SelfContained { issues, .. } => {
                assert_eq!(issues.len(), 1);
                assert!(issues[0].contains("debian/rules:1"));
                assert!(issues[0].contains("dh-cargo-vendored-sources"));
            }
            other => panic!("expected SelfContained, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn assess_dh_cargo_active_goes_to_archive() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/control",
            "Build-Depends:\n dh-cargo (>= 28ubuntu1~),\n",
        );
        write(
            &repo,
            "debian/rules",
            "CARGO_BIN=\"x\" CARGO_VENDOR_DIR=y debian/dh-cargo-vendored-sources\n",
        );
        match assess_dh_cargo(&repo) {
            PackagingState::Archive {
                requirement,
                evidence,
            } => {
                match requirement {
                    Requirement::Single { package, floor, .. } => {
                        assert_eq!(package, "dh-cargo");
                        assert_eq!(floor.as_deref(), Some("28ubuntu1~"));
                    }
                    other => panic!("expected Single, got {other:?}"),
                }
                assert_eq!(evidence.len(), 2);
            }
            other => panic!("expected Archive, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&repo);
    }

    // ── pkgconf ────────────────────────────────────────────────────────────

    #[test]
    fn assess_pkgconf_pkg_config_without_export_needs_attention() {
        let repo = temp_repo();
        write(&repo, "debian/control", "Build-Depends:\n pkg-config,\n");
        write(
            &repo,
            "debian/rules",
            "export LIBSSH2_SYS_USE_PKG_CONFIG=1\n",
        );
        match assess_pkgconf(&repo) {
            PackagingState::SelfContained { issues, .. } => {
                assert_eq!(issues.len(), 1);
                assert!(issues[0].contains("PKG_CONFIG=pkg-config"));
            }
            other => panic!("expected SelfContained, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn assess_pkgconf_pkg_config_with_export_is_satisfied() {
        let repo = temp_repo();
        write(&repo, "debian/control", "Build-Depends:\n pkg-config,\n");
        write(
            &repo,
            "debian/rules",
            "export LIBSSH2_SYS_USE_PKG_CONFIG=1\nexport PKG_CONFIG=pkg-config\n",
        );
        match assess_pkgconf(&repo) {
            PackagingState::SelfContained {
                summary, issues, ..
            } => {
                assert!(issues.is_empty());
                assert!(summary.contains("already switched to pkg-config"));
            }
            other => panic!("expected SelfContained, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn assess_pkgconf_both_present_is_unknown() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/control",
            "Build-Depends:\n pkgconf,\n pkg-config,\n",
        );
        assert!(matches!(
            assess_pkgconf(&repo),
            PackagingState::Unknown { .. }
        ));
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn assess_pkgconf_present_goes_to_archive() {
        let repo = temp_repo();
        write(&repo, "debian/control", "Build-Depends:\n pkgconf,\n");
        match assess_pkgconf(&repo) {
            PackagingState::Archive { requirement, .. } => match requirement {
                Requirement::Single { package, floor, .. } => {
                    assert_eq!(package, "pkgconf");
                    assert!(floor.is_none());
                }
                other => panic!("expected Single, got {other:?}"),
            },
            other => panic!("expected Archive, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&repo);
    }

    // ── cmake ──────────────────────────────────────────────────────────────

    #[test]
    fn assess_cmake_parses_alternatives_with_constraints() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/control",
            "Build-Depends:\n cmake (>= 3.0) | cmake3 (>= 3.0) | cmake-mozilla (>= 3.0),\n",
        );
        match assess_cmake(&repo) {
            PackagingState::Archive {
                requirement,
                evidence,
            } => {
                match requirement {
                    Requirement::Alternatives(alternatives) => {
                        assert_eq!(
                            alternatives,
                            vec![
                                ("cmake".to_owned(), Some("3.0".to_owned())),
                                ("cmake3".to_owned(), Some("3.0".to_owned())),
                                ("cmake-mozilla".to_owned(), Some("3.0".to_owned())),
                            ]
                        );
                    }
                    other => panic!("expected Alternatives, got {other:?}"),
                }
                assert!(evidence.iter().any(|e| e.contains("cmake-mozilla")));
            }
            other => panic!("expected Archive, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn assess_cmake_mozilla_only_entry_is_recognized() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/control",
            "Build-Depends:\n cmake-mozilla (>= 3.0),\n",
        );
        match assess_cmake(&repo) {
            PackagingState::Archive { requirement, .. } => match requirement {
                Requirement::Alternatives(alternatives) => {
                    assert_eq!(alternatives.len(), 1);
                    assert_eq!(alternatives[0].0, "cmake-mozilla");
                }
                other => panic!("expected Alternatives, got {other:?}"),
            },
            other => panic!("expected Archive, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn assess_cmake_star_constraint_is_kept() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/control",
            "Build-Depends:\n cmake (>= *) | cmake3 (>= *),\n",
        );
        match assess_cmake(&repo) {
            PackagingState::Archive { requirement, .. } => match requirement {
                Requirement::Alternatives(alternatives) => {
                    assert_eq!(alternatives[0].1.as_deref(), Some("*"));
                }
                other => panic!("expected Alternatives, got {other:?}"),
            },
            other => panic!("expected Archive, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn assess_cmake_missing_is_unknown() {
        let repo = temp_repo();
        write(&repo, "debian/control", "Build-Depends:\n pkgconf,\n");
        assert!(matches!(
            assess_cmake(&repo),
            PackagingState::Unknown { .. }
        ));
        let _ = fs::remove_dir_all(&repo);
    }

    // ── debhelper-compat ───────────────────────────────────────────────────

    #[test]
    fn assess_debhelper_compat_reads_level() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/control",
            "Build-Depends:\n debhelper-compat (= 13),\n",
        );
        match assess_debhelper_compat(&repo) {
            PackagingState::Archive { requirement, .. } => match requirement {
                Requirement::Single { package, floor, .. } => {
                    assert_eq!(package, "debhelper");
                    assert_eq!(floor.as_deref(), Some("13"));
                }
                other => panic!("expected Single, got {other:?}"),
            },
            other => panic!("expected Archive, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn assess_debhelper_compat_missing_is_unknown() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/control",
            "Build-Depends:\n debhelper (>= 9),\n",
        );
        assert!(matches!(
            assess_debhelper_compat(&repo),
            PackagingState::Unknown { .. }
        ));
        let _ = fs::remove_dir_all(&repo);
    }

    #[test]
    fn install_in_env_substitutions_lists_offending_files() {
        let repo = temp_repo();
        write(
            &repo,
            "debian/libstd-rust-1.97-dev.install.in",
            "usr/lib/rust-${env:RUST_VERSION}/lib/\n",
        );
        write(&repo, "debian/rustc-1.97.install.in", "usr/bin/rustc\n");
        assert_eq!(
            install_in_env_substitutions(&repo),
            vec!["debian/libstd-rust-1.97-dev.install.in".to_owned()]
        );
        let _ = fs::remove_dir_all(&repo);
    }

    // ── outcome plumbing ───────────────────────────────────────────────────

    #[tokio::test]
    async fn dpkg_version_comparison_sanity() {
        // `1.9.0~~` sorts below every `1.9.0*`; a single `~` is a real dpkg
        // revision marker and must be kept.
        assert!(dpkg_compare_versions("1.9.0", "ge", "1.9.0~~").await);
        assert!(!dpkg_compare_versions("28", "ge", "28ubuntu1~").await);
        assert!(dpkg_compare_versions("1.9.1", "lt", "1.10~~").await);
        assert!(dpkg_compare_versions("3.22.1-1ubuntu1", "ge", "3.0").await);
    }

    #[test]
    fn check_result_is_ok_only_for_satisfied_outcomes() {
        let ok = CheckResult {
            name: "test",
            outcome: Outcome {
                kind: OutcomeKind::SatisfiedByPackaging,
                summary: "already vendored".to_owned(),
                evidence: vec![],
            },
            guidance: "",
            url: String::new(),
            needs_tarball_regen: false,
        };
        assert!(ok.is_ok());

        let attention = CheckResult {
            name: "test",
            outcome: Outcome {
                kind: OutcomeKind::NeedsAttention,
                summary: "too old".to_owned(),
                evidence: vec![],
            },
            guidance: "fix it",
            url: "u".to_owned(),
            needs_tarball_regen: true,
        };
        assert!(!attention.is_ok());
        assert!(attention.needs_tarball_regen);
    }

    // ── manual probe (not run by default) ──────────────────────────────────

    /// Print the raw assessments for a real packaging tree, e.g.:
    /// `THERMITE_COMPAT_PROBE_DIR=../rustc/rustc cargo test --lib \
    ///  probe_real_tree -- --nocapture`
    #[test]
    fn probe_real_tree() {
        let Some(dir) = std::env::var_os("THERMITE_COMPAT_PROBE_DIR") else {
            return;
        };
        let repo = PathBuf::from(dir);
        println!("== {repo:?}");
        for (name, state) in [
            ("LLVM", assess_llvm(&repo)),
            ("libgit2", assess_libgit2(&repo)),
            ("dh-cargo", assess_dh_cargo(&repo)),
            ("pkgconf", assess_pkgconf(&repo)),
            ("cmake", assess_cmake(&repo)),
            ("debhelper-compat", assess_debhelper_compat(&repo)),
        ] {
            println!("{name}: {state:#?}");
        }
    }
}
