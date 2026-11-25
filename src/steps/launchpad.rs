//! Launchpad archive access via the [`lpcli`] library crate.
//!
//! Replaces the former `rmadison` subprocess with typed queries against the
//! Launchpad REST API. All reads are anonymous (public archive data only — no
//! credentials are loaded), and in-flight requests honour the TUI
//! cancellation token.

use std::future::Future;
use std::path::Path;
use std::sync::OnceLock;

use lpcli::client::LaunchpadClient;
use lpcli::error::LpError;
use lpcli::packages::{self, SourceSearchParams};
use serde::{Deserialize, Serialize};

use crate::error::{Result, ThermiteError};
use crate::shell;

/// A published source package record reduced to the fields thermite uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Publication {
    /// Pocket (e.g. `"Release"`, `"Security"`, `"Updates"`).
    pub pocket: Option<String>,
    /// Debian version string.
    pub version: Option<String>,
    /// API self-link, needed to resolve per-file download URLs.
    pub self_link: Option<String>,
}

impl From<packages::SourcePackagePublishingHistory> for Publication {
    fn from(p: packages::SourcePackagePublishingHistory) -> Self {
        Publication {
            pocket: p.pocket,
            version: p.source_package_version,
            self_link: p.self_link,
        }
    }
}

static CLIENT: OnceLock<LaunchpadClient> = OnceLock::new();

fn client() -> &'static LaunchpadClient {
    CLIENT.get_or_init(|| LaunchpadClient::new(None))
}

/// Await a Launchpad API call, mapping errors into [`ThermiteError`] and
/// aborting the in-flight request when the TUI cancellation token fires.
async fn launchpad<T>(fut: impl Future<Output = std::result::Result<T, LpError>>) -> Result<T> {
    let mut cancel_rx = shell::cancellation_receiver();
    if cancel_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
        return Err(ThermiteError::Cancelled);
    }
    tokio::select! {
        res = fut => res.map_err(ThermiteError::from),
        _ = wait_for_cancellation(&mut cancel_rx) => Err(ThermiteError::Cancelled),
    }
}

/// Resolves when the cancellation token flips to cancelled; pending forever
/// outside TUI mode (no token installed).
async fn wait_for_cancellation(rx: &mut Option<tokio::sync::watch::Receiver<bool>>) {
    match rx.as_mut() {
        Some(rx) => while !shell::wait_for_cancel(rx).await {},
        None => std::future::pending::<()>().await,
    }
}

/// Search parameters shared by every archive publication query: exact source
/// name (no fuzzy matches such as `libgit2-glib` for `libgit2`), published
/// only, and a small page size.
fn search_params(source_name: &str) -> SourceSearchParams<'_> {
    SourceSearchParams {
        source_name: Some(source_name),
        status: Some("Published"),
        exact_match: true,
        limit: Some(20),
        ..Default::default()
    }
}

/// Query publications of `source_name` in `series` of the Ubuntu primary
/// archive.
pub async fn published_sources(series: &str, source_name: &str) -> Result<Vec<Publication>> {
    let params = search_params(source_name);
    let pubs = launchpad(packages::search_published_sources(
        client(),
        "ubuntu",
        series,
        &params,
    ))
    .await?;
    Ok(pubs.into_iter().map(Into::into).collect())
}

/// Publications of `source_name` in `series`, served from the on-disk cache
/// when enabled (cache key `<series>_<source_name>`, see [`crate::cache`]).
pub async fn cached_publications(series: &str, source_name: &str) -> Result<Vec<Publication>> {
    let cache_key = format!("{series}_{source_name}");
    if let Some(hit) = crate::cache::lookup(&cache_key) {
        shell::out_line(&format!(
            "  Launchpad: using cached result for {source_name} in {series} ({})",
            crate::cache::format_age(hit.age_secs)
        ));
        return Ok(serde_json::from_str(&hit.data).unwrap_or_default());
    }
    let publications = published_sources(series, source_name).await?;
    crate::cache::store(
        &cache_key,
        &serde_json::to_string(&publications).unwrap_or_default(),
    );
    Ok(publications)
}

/// List the active Ubuntu series known to Launchpad (short names, e.g.
/// `"noble"`), served from the on-disk cache when enabled.
pub async fn cached_active_series() -> Result<Vec<String>> {
    const KEY: &str = "active-series";
    if let Some(hit) = crate::cache::lookup(KEY) {
        return Ok(serde_json::from_str(&hit.data).unwrap_or_default());
    }
    let series = launchpad(packages::list_distro_series(client(), "ubuntu")).await?;
    let names: Vec<String> = series
        .into_iter()
        .filter(|s| s.active == Some(true))
        .map(|s| s.name)
        .collect();
    crate::cache::store(KEY, &serde_json::to_string(&names).unwrap_or_default());
    Ok(names)
}

/// Download a public Launchpad file (URL trust-validated, no credentials
/// sent) to `dest`, printing a progress line every 64 MiB.
pub async fn download_public_file(url: &str, dest: &Path) -> Result<()> {
    const STEP: u64 = 64 * 1024 * 1024;
    let mut last = 0u64;
    let client = client();
    launchpad(async move {
        client
            .download_public_to_file_with_progress(url, dest, |received, _| {
                if received >= last + STEP {
                    last = received;
                    shell::out_line(&format!(
                        "  ... {} MiB downloaded",
                        received / (1024 * 1024)
                    ));
                }
            })
            .await?;
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publication_from_history_maps_fields() {
        let history = packages::SourcePackagePublishingHistory {
            self_link: Some(
                "https://api.launchpad.net/devel/ubuntu/+archive/primary/+sourcepub/1".to_owned(),
            ),
            source_package_name: Some("libgit2".to_owned()),
            source_package_version: Some("1.7.2+ds-1ubuntu3".to_owned()),
            component_name: None,
            section_name: None,
            status: Some("Published".to_owned()),
            date_published: None,
            date_superseded: None,
            pocket: Some("Release".to_owned()),
            archive_link: None,
            distro_series_link: None,
            package_maintainer_link: None,
        };
        let publication = Publication::from(history);
        assert_eq!(publication.pocket.as_deref(), Some("Release"));
        assert_eq!(publication.version.as_deref(), Some("1.7.2+ds-1ubuntu3"));
        assert!(publication.self_link.is_some());
    }

    #[test]
    fn publication_serializes_round_trip() {
        let publication = Publication {
            pocket: Some("Security".to_owned()),
            version: Some("1.7.2+ds-1ubuntu3.3".to_owned()),
            self_link: None,
        };
        let json = serde_json::to_string(&publication).unwrap();
        let parsed: Publication = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, publication);
    }

    #[test]
    fn search_params_are_exact_and_published_only() {
        let params = search_params("libgit2");
        assert_eq!(params.source_name, Some("libgit2"));
        assert_eq!(params.status, Some("Published"));
        assert!(params.exact_match);
    }
}
