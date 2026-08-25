use crate::error::{Result, ThermiteError};

/// The set of known Ubuntu release adjectives paired with their series numbers.
///
/// This list covers all Ubuntu releases from Focal onwards. Add new entries
/// here as new Ubuntu releases are announced.
const KNOWN_RELEASES: &[(&str, &str)] = &[
    ("focal", "20.04"),    // 20.04 LTS
    ("groovy", "20.10"),   // 20.10
    ("hirsute", "21.04"),  // 21.04
    ("impish", "21.10"),   // 21.10
    ("jammy", "22.04"),    // 22.04 LTS
    ("kinetic", "22.10"),  // 22.10
    ("lunar", "23.04"),    // 23.04
    ("mantic", "23.10"),   // 23.10
    ("noble", "24.04"),    // 24.04 LTS
    ("oracular", "24.10"), // 24.10
    ("plucky", "25.04"),   // 25.04
    ("questing", "25.10"), // 25.10
    ("resolute", "26.04"), // 26.04 LTS
    ("stonking", "26.10"), // 26.10
];

/// A validated Ubuntu release adjective (e.g. `"noble"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UbuntuRelease(String);

impl UbuntuRelease {
    /// Parse and validate a release adjective string.
    pub fn parse(s: &str) -> Result<Self> {
        if KNOWN_RELEASES.iter().any(|(name, _)| *name == s) {
            Ok(Self(s.to_owned()))
        } else {
            Err(ThermiteError::UnknownRelease(s.to_owned()))
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
        KNOWN_RELEASES
            .iter()
            .find(|(name, _)| *name == self.0.as_str())
            .map(|(_, series)| *series)
            .expect("UbuntuRelease invariant: adjective is always in KNOWN_RELEASES")
    }

    /// Return the adjective of the current Ubuntu development release.
    ///
    /// The devel release is defined as the **last** entry in
    /// [`KNOWN_RELEASES`]. This list is appended to as new Ubuntu releases are
    /// announced, so the value returned here tracks the current devel release
    /// as long as the list is kept up to date.
    pub fn devel() -> &'static str {
        KNOWN_RELEASES
            .last()
            .map(|(name, _)| *name)
            .expect("KNOWN_RELEASES is non-empty by construction")
    }

    /// Return `true` when this release is the current Ubuntu development
    /// release (i.e. the last entry in [`KNOWN_RELEASES`]).
    ///
    /// This is a **static** heuristic: it is correct only as long as
    /// [`KNOWN_RELEASES`] is kept up to date with the release cadence. Callers
    /// that need to be robust against the devel→stable transition should treat
    /// this as a hint for messaging only, not as the source of truth for
    /// branch-name decisions.
    pub fn is_devel(&self) -> bool {
        self.0 == Self::devel()
    }
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
    fn reject_unknown_release() {
        assert!(UbuntuRelease::parse("foobar").is_err());
        assert!(UbuntuRelease::parse("").is_err());
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
    fn devel_returns_last_known_release_adjective() {
        let last = KNOWN_RELEASES.last().expect("KNOWN_RELEASES is non-empty");
        assert_eq!(UbuntuRelease::devel(), last.0);
    }

    #[test]
    fn is_devel_true_for_last_known_release() {
        let last = KNOWN_RELEASES.last().expect("KNOWN_RELEASES is non-empty");
        let r = UbuntuRelease::parse(last.0).unwrap();
        assert!(r.is_devel(), "{} should be devel", last.0);
    }

    #[test]
    fn is_devel_false_for_non_last_release() {
        let r = UbuntuRelease::parse("noble").unwrap();
        // `noble` is only the devel release if it happens to be the last entry
        // in KNOWN_RELEASES; assert against the known devel adjective instead.
        if UbuntuRelease::devel() != "noble" {
            assert!(!r.is_devel());
        }
    }
}
