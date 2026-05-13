use std::fmt;

use crate::error::{Result, ThermiteError};

/// A full Rust version in `X.Y.Z` format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl RustVersion {
    /// Parse a `"X.Y.Z"` string into a [`RustVersion`].
    pub fn parse(s: &str) -> Result<Self> {
        let parts: Vec<&str> = s.split('.').collect();
        if parts.len() != 3 {
            return Err(ThermiteError::InvalidRustVersion(s.to_owned()));
        }
        let parse_component = |p: &str| {
            p.parse::<u32>()
                .map_err(|_| ThermiteError::InvalidRustVersion(s.to_owned()))
        };
        Ok(Self {
            major: parse_component(parts[0])?,
            minor: parse_component(parts[1])?,
            patch: parse_component(parts[2])?,
        })
    }

    /// Return the short (`X.Y`) form of this version.
    pub fn short(&self) -> ShortRustVersion {
        ShortRustVersion {
            major: self.major,
            minor: self.minor,
        }
    }
}

impl fmt::Display for RustVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A short Rust version in `X.Y` format (patch component omitted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortRustVersion {
    pub major: u32,
    pub minor: u32,
}

impl fmt::Display for ShortRustVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_version() {
        let v = RustVersion::parse("1.85.1").unwrap();
        assert_eq!(v.major, 1);
        assert_eq!(v.minor, 85);
        assert_eq!(v.patch, 1);
        assert_eq!(v.to_string(), "1.85.1");
    }

    #[test]
    fn parse_invalid_versions() {
        assert!(RustVersion::parse("1.85").is_err());
        assert!(RustVersion::parse("1.85.x").is_err());
        assert!(RustVersion::parse("").is_err());
        assert!(RustVersion::parse("1.2.3.4").is_err());
    }

    #[test]
    fn short_version() {
        let v = RustVersion::parse("1.85.1").unwrap();
        assert_eq!(v.short().to_string(), "1.85");
    }
}
