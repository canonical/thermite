use crate::error::{Result, ThermiteError};
use ubuntu_info_rust::SeriesInfo;

/// The oldest Ubuntu release version covered by the Rust toolchain packaging
/// workflow. Series older than this (Warty through Disco) are excluded from
/// validation and from the backport chain, matching the manually maintained
/// release list this replaces. Bump when the packaging floor moves.
const MIN_SUPPORTED_VERSION: u32 = 20;

/// Return every known Ubuntu series with a major version at or above
/// [`MIN_SUPPORTED_VERSION`], oldest first.
///
/// `SeriesInfo::all()` covers the whole Ubuntu history; the toolchain
/// packaging workflow only cares about modern series, so the floor is
/// applied here once and reused by every lookup below.
fn supported_series() -> impl Iterator<Item = &'static SeriesInfo> {
    SeriesInfo::all().filter(|s| is_supported(s))
}

/// Whether `s` is the series number of a supported Ubuntu release (e.g. `"24.04"`).
pub fn is_known_series(s: &str) -> bool {
    supported_series().any(|info| info.version == s)
}

/// A validated Ubuntu release adjective (e.g. `"noble"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UbuntuRelease(String);

impl UbuntuRelease {
    /// Parse and validate a release adjective string.
    ///
    /// Matching is ASCII case-insensitive (`"Noble"` parses to `"noble"`),
    /// matching [`ubuntu_info_rust::SeriesInfo::from_code_name`].
    pub fn parse(s: &str) -> Result<Self> {
        match SeriesInfo::from_code_name(s) {
            Some(info) if is_supported(info) => Ok(Self(info.code_name_short.to_owned())),
            _ => Err(ThermiteError::UnknownRelease(s.to_owned())),
        }
    }

    /// Return the release adjective as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Return the numeric series identifier for this release (e.g. `"22.04"`).
    ///
    /// Panics if the release was somehow constructed with an unknown adjective,
    /// which cannot happen through the public [`Self::parse`] constructor.
    pub fn series_number(&self) -> &'static str {
        SeriesInfo::from_code_name(&self.0)
            .map(|info| info.version)
            .expect("UbuntuRelease invariant: adjective is always a supported series")
    }

    /// Return the adjective of the current Ubuntu development release.
    ///
    /// Sourced from [`ubuntu_info_rust::SeriesInfo::devel`], which resolves
    /// the unreleased series with the earliest release date — no manual
    /// list appending required when a new cycle opens.
    pub fn devel() -> &'static str {
        SeriesInfo::devel().code_name_short
    }

    /// Construct the [`UbuntuRelease`] for the current devel release.
    ///
    /// Equivalent to `UbuntuRelease::parse(UbuntuRelease::devel()).unwrap()`
    /// but infallible, since [`devel`](Self::devel) always returns a known
    /// adjective.
    pub fn devel_release() -> Self {
        Self(Self::devel().to_owned())
    }

    /// Return `true` when this release is the current Ubuntu development
    /// release.
    ///
    /// This is a **static** heuristic: it is correct only as long as the
    /// `ubuntu-info-rust` data is kept up to date with the release cadence
    /// (the crate's data is generated from `distro-info-data`). Callers
    /// that need to be robust against the devel→stable transition should
    /// treat this as a hint for messaging only, not as the source of truth
    /// for branch-name decisions.
    pub fn is_devel(&self) -> bool {
        self.0 == Self::devel()
    }

    /// Return `true` when this release is an Ubuntu Long-Term Support release.
    ///
    /// Sourced from [`ubuntu_info_rust::SeriesInfo::is_lts`], which is derived
    /// from the upstream `ubuntu.csv` data rather than a YY.04 heuristic.
    pub fn is_lts(&self) -> bool {
        SeriesInfo::from_code_name(&self.0).is_some_and(|info| info.is_lts)
    }

    /// Return the ordered chain of releases that backports traverse, from the
    /// current devel release down to the oldest supported LTS.
    ///
    /// The chain consists of the current devel release followed by every LTS
    /// release at or above the `MIN_SUPPORTED_VERSION` floor, newest-first. Non-LTS,
    /// non-devel releases are excluded — backports normally go one LTS at a
    /// time.
    ///
    /// Example (as of the 27.04 cycle): `["tenacious", "resolute", "noble",
    /// "jammy", "focal"]`.
    pub fn backport_chain() -> Vec<&'static str> {
        let mut chain: Vec<&'static str> = supported_series()
            .filter(|info| info.is_lts)
            .map(|info| info.code_name_short)
            .collect();
        // `all()` yields oldest-to-newest; the chain wants newest-to-oldest.
        chain.reverse();
        // Head is the current devel release.
        chain.insert(0, Self::devel());
        // `devel()` may itself be an LTS release (e.g. a 26.04 devel before
        // release); dedup so it appears only once, at the head.
        chain.dedup();
        chain
    }

    /// Return the position of this release in [`Self::backport_chain()`], or
    /// `None` when the release is not part of the LTS+devel chain (i.e. it is
    /// a non-LTS, non-devel release such as `oracular` or `questing`).
    pub fn chain_position(&self) -> Option<usize> {
        Self::backport_chain()
            .iter()
            .position(|name| *name == self.0.as_str())
    }

    /// Reverse lookup: the release adjective for a numeric series
    /// (`"24.04"` → `"noble"`), or `None` for an unknown series.
    ///
    /// Delegates to [`ubuntu_info_rust::SeriesInfo::from_version`], which also
    /// accepts LTS suffixes and point releases (`"24.04.1 LTS"` → `"noble"`).
    pub fn release_from_series(series: &str) -> Option<&'static str> {
        SeriesInfo::from_version(series)
            .filter(|info| is_supported(info))
            .map(|info| info.code_name_short)
    }
}

/// Whether `info` is at or above the [`MIN_SUPPORTED_VERSION`] floor.
fn is_supported(info: &SeriesInfo) -> bool {
    info.version
        .split('.')
        .next()
        .and_then(|year| year.parse::<u32>().ok())
        .is_some_and(|year| year >= MIN_SUPPORTED_VERSION)
}

impl std::fmt::Display for UbuntuRelease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_known_release() {
        assert!(UbuntuRelease::parse("noble").is_ok());
        assert!(UbuntuRelease::parse("jammy").is_ok());
    }

    #[test]
    fn parse_normalizes_case() {
        assert_eq!(UbuntuRelease::parse("Noble").unwrap().as_str(), "noble");
    }

    #[test]
    fn parse_accepts_devel_alias() {
        assert_eq!(
            UbuntuRelease::parse("devel").unwrap(),
            UbuntuRelease::devel_release()
        );
        assert_eq!(
            UbuntuRelease::parse("Devel").unwrap(),
            UbuntuRelease::devel_release()
        );
    }

    #[test]
    fn parse_accepts_full_code_name() {
        assert_eq!(
            UbuntuRelease::parse("Noble Numbat").unwrap().as_str(),
            "noble"
        );
    }

    #[test]
    fn reject_unknown_release() {
        assert!(UbuntuRelease::parse("foobar").is_err());
        assert!(UbuntuRelease::parse("").is_err());
    }

    #[test]
    fn reject_pre_floor_release() {
        // Bionic (18.04) is a real Ubuntu release but below the packaging floor.
        assert!(UbuntuRelease::parse("bionic").is_err());
    }

    #[test]
    fn series_number_returns_correct_value() {
        assert_eq!(
            UbuntuRelease::parse("jammy").unwrap().series_number(),
            "22.04"
        );
        assert_eq!(
            UbuntuRelease::parse("noble").unwrap().series_number(),
            "24.04"
        );
        assert_eq!(
            UbuntuRelease::parse("focal").unwrap().series_number(),
            "20.04"
        );
        assert_eq!(
            UbuntuRelease::parse("questing").unwrap().series_number(),
            "25.10"
        );
    }

    #[test]
    fn devel_matches_series_info_devel() {
        assert_eq!(UbuntuRelease::devel(), SeriesInfo::devel().code_name_short);
    }

    #[test]
    fn is_devel_true_for_series_info_devel() {
        let r = UbuntuRelease::devel_release();
        assert!(r.is_devel(), "{} should be devel", r);
    }

    #[test]
    fn is_devel_false_for_non_devel_release() {
        let r = UbuntuRelease::parse("noble").unwrap();
        // `noble` is only the devel release if it happens to be the current
        // devel; assert against the known devel adjective instead.
        if UbuntuRelease::devel() != "noble" {
            assert!(!r.is_devel());
        }
    }

    #[test]
    fn devel_release_matches_parse_of_devel_adjective() {
        let via_devel_release = UbuntuRelease::devel_release();
        let via_parse = UbuntuRelease::parse(UbuntuRelease::devel()).unwrap();
        assert_eq!(via_devel_release.as_str(), via_parse.as_str());
        assert!(via_devel_release.is_devel());
    }

    #[test]
    fn is_lts_true_for_known_lts_releases() {
        for name in ["focal", "jammy", "noble", "resolute"] {
            let r = UbuntuRelease::parse(name).unwrap();
            assert!(r.is_lts(), "{name} should be LTS");
        }
    }

    #[test]
    fn is_lts_false_for_non_lts_releases() {
        for name in ["groovy", "oracular", "questing", "stonking"] {
            let r = UbuntuRelease::parse(name).unwrap();
            // `stonking` (26.10) is a recent non-LTS release; if a future LTS
            // is appended it stays non-LTS. Guard against the devel being LTS.
            if !r.is_devel() {
                assert!(!r.is_lts(), "{name} should not be LTS");
            }
        }
    }

    #[test]
    fn backport_chain_starts_with_devel_then_lts_newest_first() {
        let chain = UbuntuRelease::backport_chain();
        assert!(!chain.is_empty(), "chain should be non-empty");
        // Head is always the current devel release.
        assert_eq!(chain[0], UbuntuRelease::devel());
        // Every subsequent entry is an LTS release, newest-first.
        for name in &chain[1..] {
            let r = UbuntuRelease::parse(name).unwrap();
            assert!(r.is_lts(), "{name} in chain should be LTS");
        }
        // LTS entries are in descending series-number order.
        let lts_series: Vec<&str> = chain[1..]
            .iter()
            .map(|name| UbuntuRelease::parse(name).unwrap().series_number())
            .collect();
        let mut sorted = lts_series.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(lts_series, sorted, "LTS entries should be newest-first");
    }

    #[test]
    fn chain_position_returns_index_for_chain_members() {
        let chain = UbuntuRelease::backport_chain();
        for (idx, name) in chain.iter().enumerate() {
            let r = UbuntuRelease::parse(name).unwrap();
            assert_eq!(
                r.chain_position(),
                Some(idx),
                "{name} should be at position {idx}"
            );
        }
    }

    #[test]
    fn chain_position_none_for_non_chain_release() {
        // `oracular` is non-LTS and non-devel, so it is not in the chain.
        let r = UbuntuRelease::parse("oracular").unwrap();
        assert_eq!(r.chain_position(), None);
    }

    #[test]
    fn backport_chain_includes_lts_releases_above_floor() {
        let chain = UbuntuRelease::backport_chain();
        for expected in ["resolute", "noble", "jammy", "focal"] {
            assert!(chain.contains(&expected), "chain should contain {expected}");
        }
        for excluded in ["bionic", "xenial", "trusty"] {
            assert!(
                !chain.contains(&excluded),
                "chain should not contain {excluded}"
            );
        }
    }

    #[test]
    fn is_known_series_matches_supported_releases() {
        assert!(is_known_series("24.04"));
        assert!(is_known_series("20.04"));
        assert!(is_known_series("26.10"));
        assert!(!is_known_series("banana"));
        assert!(!is_known_series("20.4"));
        assert!(!is_known_series(""));
        assert!(!is_known_series("18.04"), "bionic is below the floor");
    }

    #[test]
    fn release_from_series_resolves_supported_series() {
        assert_eq!(UbuntuRelease::release_from_series("24.04"), Some("noble"));
        assert_eq!(UbuntuRelease::release_from_series("20.04"), Some("focal"));
        assert_eq!(UbuntuRelease::release_from_series("banana"), None);
        assert_eq!(
            UbuntuRelease::release_from_series("18.04"),
            None,
            "bionic is below the floor"
        );
    }
}
