use crate::error::{Result, ThermiteError};
use crate::types::{ubuntu::UbuntuRelease, versions::RustVersion};

/// Parameters for the `thermite update` command.
///
/// All values are validated on construction. The short version fields are
/// derived automatically from their full-version counterparts.
#[derive(Debug, Clone)]
pub struct UpdateParams {
    /// Full Rust version being packaged, e.g. `"1.85.1"`.
    pub rust_update_version: RustVersion,
    /// Full Rust version being replaced, e.g. `"1.84.0"`.
    pub rust_old_version: RustVersion,
    /// Target Ubuntu release adjective, e.g. `"noble"`.
    pub release: UbuntuRelease,
    /// Launchpad username; also used as the personal Git remote name.
    pub lpuser: String,
    /// Local Git remote name for the Foundations rustc repository.
    /// Defaults to `"foundations"`.
    pub git_remote: String,
    /// Launchpad bug ID number for this work (digits only).
    pub lp_bug_number: String,
}

impl UpdateParams {
    /// Construct and validate a new [`UpdateParams`].
    pub fn new(
        rust_update_version: &str,
        rust_old_version: &str,
        release: &str,
        lpuser: &str,
        git_remote: &str,
        lp_bug_number: &str,
    ) -> Result<Self> {
        if lpuser.is_empty() {
            // Finding 10: use the dedicated variant instead of InvalidRustVersion.
            return Err(ThermiteError::InvalidLpUser(
                "lpuser must not be empty".to_owned(),
            ));
        }
        if lp_bug_number.is_empty() || !lp_bug_number.chars().all(|c| c.is_ascii_digit()) {
            // Finding 10: dedicated variant for bug number validation failure.
            return Err(ThermiteError::InvalidLpBugNumber(format!(
                "'{lp_bug_number}' must be a non-empty string of digits"
            )));
        }
        Ok(Self {
            rust_update_version: RustVersion::parse(rust_update_version)?,
            rust_old_version: RustVersion::parse(rust_old_version)?,
            release: UbuntuRelease::parse(release)?,
            lpuser: lpuser.to_owned(),
            git_remote: git_remote.to_owned(),
            lp_bug_number: lp_bug_number.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ThermiteError;

    fn valid_params() -> UpdateParams {
        UpdateParams::new("1.85.1", "1.84.0", "noble", "jdoe", "foundations", "12345").unwrap()
    }

    #[test]
    fn valid_params_constructs_successfully() {
        let p = valid_params();
        assert_eq!(p.lpuser, "jdoe");
        assert_eq!(p.lp_bug_number, "12345");
    }

    /// Finding 10: empty lpuser must return InvalidLpUser, not InvalidRustVersion.
    #[test]
    fn empty_lpuser_returns_invalid_lp_user_error() {
        let result = UpdateParams::new("1.85.1", "1.84.0", "noble", "", "foundations", "12345");
        assert!(
            matches!(result, Err(ThermiteError::InvalidLpUser(_))),
            "expected InvalidLpUser, got: {:?}",
            result
        );
    }

    /// Finding 10: non-digit lp_bug_number must return InvalidLpBugNumber.
    #[test]
    fn non_digit_lp_bug_number_returns_invalid_lp_bug_number_error() {
        let result = UpdateParams::new("1.85.1", "1.84.0", "noble", "jdoe", "foundations", "abc");
        assert!(
            matches!(result, Err(ThermiteError::InvalidLpBugNumber(_))),
            "expected InvalidLpBugNumber, got: {:?}",
            result
        );
    }

    /// Finding 10: empty lp_bug_number must also return InvalidLpBugNumber.
    #[test]
    fn empty_lp_bug_number_returns_invalid_lp_bug_number_error() {
        let result = UpdateParams::new("1.85.1", "1.84.0", "noble", "jdoe", "foundations", "");
        assert!(
            matches!(result, Err(ThermiteError::InvalidLpBugNumber(_))),
            "expected InvalidLpBugNumber, got: {:?}",
            result
        );
    }
}
