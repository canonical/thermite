use std::path::Path;

use crate::error::Result;
use crate::shell::run_command;

/// Import an upstream orig tarball into the Git repository using
/// `gbp import-orig`.
///
/// `extra_args` can be used to pass additional flags such as
/// `--component=vendor`.
pub async fn gbp_import_orig(
    repo_dir: &Path,
    tarball: &Path,
    upstream_branch: &str,
    debian_branch: &str,
    extra_args: &[&str],
) -> Result<()> {
    let tarball_str = tarball.to_string_lossy().to_string();

    let upstream_arg = format!("--upstream-branch={upstream_branch}");
    let debian_arg = format!("--debian-branch={debian_branch}");
    let mut args = vec![
        "import-orig",
        "--no-symlink-orig",
        "--no-pristine-tar",
        &upstream_arg,
        &debian_arg,
    ];
    args.extend_from_slice(extra_args);
    args.push(&tarball_str);

    run_command("gbp", &args, repo_dir, &[]).await?;
    Ok(())
}
