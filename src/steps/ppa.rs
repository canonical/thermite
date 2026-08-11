use std::path::Path;

use crate::error::Result;
use crate::shell::run_command;

/// Create a new PPA using `ppa-dev-tools`.
///
/// Returns the URL of the newly created PPA.
pub async fn create_ppa(name: &str) -> Result<String> {
    let output = run_command("ppa", &["create", name], Path::new("."), &[]).await?;
    // The ppa tool prints the PPA URL on stdout.
    let url = output
        .stdout
        .lines()
        .find(|l| l.contains("launchpad.net"))
        .unwrap_or("")
        .trim()
        .to_owned();
    Ok(url)
}

/// Add a temporary PPA changelog entry with the version suffixed by
/// `~ppa<upload_number>`.
pub async fn add_ppa_changelog_entry(
    repo_dir: &Path,
    version_str: &str,
    release: &str,
    upload_number: u32,
) -> Result<()> {
    let ppa_version = format!("{version_str}~ppa{upload_number}");
    run_command(
        "dch",
        &["-bv", &ppa_version, "--distribution", release, "PPA upload"],
        repo_dir,
        &[("VISUAL", "true"), ("EDITOR", "true")],
    )
    .await?;
    Ok(())
}

/// Upload a source package `.changes` file to a PPA using `dput`.
pub async fn dput_to_ppa(ppa_path: &str, changes_file: &Path) -> Result<()> {
    let changes_str = changes_file.to_string_lossy().to_string();
    run_command(
        "dput",
        &[&format!("ppa:{ppa_path}"), &changes_str],
        Path::new("."),
        &[],
    )
    .await?;
    Ok(())
}

/// Run `ppa tests` to get links to PPA autopkgtest results.
pub async fn get_ppa_test_urls(lpuser: &str, ppa_name: &str, release: &str) -> Result<Vec<String>> {
    let ppa_ref = format!("ppa:{lpuser}/{ppa_name}");
    let output = run_command(
        "ppa",
        &["tests", &ppa_ref, "--release", release, "--show-url"],
        Path::new("."),
        &[],
    )
    .await?;
    let urls: Vec<String> = output
        .stdout
        .lines()
        .filter(|l| l.starts_with("http"))
        .map(|l| l.to_owned())
        .collect();
    Ok(urls)
}

/// Run `ppa tests ppa:rust-toolchain/staging` to get autopkgtest trigger URLs
/// for a package in the staging PPA.
///
/// `pkg_name` should be the versioned package name (e.g. `"rustc-1.85"`).
pub async fn get_staging_ppa_test_urls(pkg_name: &str, release: &str) -> Result<Vec<String>> {
    let output = run_command(
        "ppa",
        &[
            "tests",
            "ppa:rust-toolchain/staging",
            "-p",
            pkg_name,
            "--release",
            release,
            "--show-url",
        ],
        Path::new("."),
        &[],
    )
    .await?;
    let urls: Vec<String> = output
        .stdout
        .lines()
        .filter(|l| l.starts_with("http"))
        .map(|l| l.to_owned())
        .collect();
    Ok(urls)
}
