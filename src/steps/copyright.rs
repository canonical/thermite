use std::path::Path;

use crate::error::Result;

/// In `debian/copyright`, comment out the bare `vendor` line under
/// `Files-Excluded` so that `uscan` will include the full `vendor/` directory
/// in the generated orig tarball.
///
/// The transformation applied is:
/// ```text
/// -  vendor
/// +# vendor
/// ```
///
/// This is a temporary, non-committed edit that must be reverted (via
/// `git restore debian/copyright`) after the first `uscan` run.
pub fn comment_out_vendor_exclusion(copyright_path: &Path) -> Result<()> {
    let content = std::fs::read_to_string(copyright_path)?;
    // Match the line that is exactly (optional whitespace) "vendor" with no
    // leading comment character.
    let updated = content
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            // Only modify the bare `vendor` exclusion line — not lines like
            // `Files-Excluded-vendor` or already-commented lines.
            if trimmed == "vendor" && !line.trim_start().starts_with('#') {
                format!("#{line}")
            } else {
                line.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";

    std::fs::write(copyright_path, updated)?;
    Ok(())
}

/// Add a new pattern to the `Files-Excluded-vendor` section in
/// `debian/copyright`.
///
/// The pattern is appended as a new indented line immediately after the
/// `Files-Excluded-vendor:` header (or after the last existing entry in that
/// section).
pub fn add_vendor_exclusion(copyright_path: &Path, pattern: &str) -> Result<()> {
    let content = std::fs::read_to_string(copyright_path)?;
    let mut lines: Vec<String> = content.lines().map(|l| l.to_owned()).collect();

    // Find the `Files-Excluded-vendor:` section and insert at its end.
    let mut in_section = false;
    let mut insert_at: Option<usize> = None;

    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("Files-Excluded-vendor:") {
            in_section = true;
            insert_at = Some(i + 1);
            continue;
        }
        if in_section {
            // Continuation lines start with whitespace.
            if line.starts_with(' ') || line.starts_with('\t') {
                insert_at = Some(i + 1);
            } else {
                // End of section.
                break;
            }
        }
    }

    if let Some(idx) = insert_at {
        lines.insert(idx, format!(" {pattern}"));
    } else {
        // Section does not exist; append it.
        lines.push(String::new());
        lines.push("Files-Excluded-vendor:".to_owned());
        lines.push(format!(" {pattern}"));
    }

    std::fs::write(copyright_path, lines.join("\n") + "\n")?;
    Ok(())
}
