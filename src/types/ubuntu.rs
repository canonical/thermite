use crate::error::{Result, ThermiteError};

/// The set of known Ubuntu release adjectives.
///
/// This list covers all Ubuntu releases from Focal onwards. Add new entries
/// here as new Ubuntu releases are announced.
const KNOWN_RELEASES: &[&str] = &[
    "focal",    // 20.04 LTS
    "groovy",   // 20.10
    "hirsute",  // 21.04
    "impish",   // 21.10
    "jammy",    // 22.04 LTS
    "kinetic",  // 22.10
    "lunar",    // 23.04
    "mantic",   // 23.10
    "noble",    // 24.04 LTS
    "oracular", // 24.10
    "plucky",   // 25.04
    "questing", // 25.10
    "resolute", // 26.04 LTS
];

/// A validated Ubuntu release adjective (e.g. `"noble"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UbuntuRelease(String);

impl UbuntuRelease {
    /// Parse and validate a release adjective string.
    pub fn parse(s: &str) -> Result<Self> {
        if KNOWN_RELEASES.contains(&s) {
            Ok(Self(s.to_owned()))
        } else {
            Err(ThermiteError::UnknownRelease(s.to_owned()))
        }
    }

    /// Return the release adjective as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
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
}
