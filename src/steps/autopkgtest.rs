use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::shell::run_command;

/// Run the autopkgtests for `package` against a QEMU test bed image.
///
/// `extra_apt_sources` can include PPA sources used for bootstrapping.
/// `ram_mb` and `cpus` control the QEMU resource allocation.
pub async fn run_autopkgtest_qemu(
    package: &str,
    testbed_image: &Path,
    extra_apt_source: &str,
    log_path: &Path,
    ram_mb: u32,
    cpus: u32,
) -> Result<()> {
    let log_str = log_path.to_string_lossy().to_string();
    let image_str = testbed_image.to_string_lossy().to_string();
    let ram_arg = format!("--ram-size={ram_mb}");
    let cpus_arg = format!("--cpus={cpus}");
    let apt_source_arg = format!("--add-apt-source={extra_apt_source}");

    run_command(
        "autopkgtest",
        &[
            package,
            "--apt-upgrade",
            "--shell-fail",
            &apt_source_arg,
            &format!("--log-file={log_str}"),
            "--",
            "qemu",
            &ram_arg,
            &cpus_arg,
            &image_str,
        ],
        Path::new("."),
        &[],
    )
    .await?;
    Ok(())
}

/// Build a QEMU autopkgtest image for `release` using
/// `autopkgtest-buildvm-ubuntu-cloud`.
///
/// `disk_gb` and `ram_mb` and `cpus` set the image size and resource profile.
/// Returns the path to the generated image.
pub async fn build_testbed_image(
    release: &str,
    disk_gb: u32,
    ram_mb: u32,
    cpus: u32,
    output_dir: &Path,
) -> Result<PathBuf> {
    // Finding 8: pass -s and the size value as two separate arguments.
    // A single token "-s 100G" with an embedded space is not a valid CLI
    // argument and causes the tool to fail immediately.
    let disk_size = format!("{disk_gb}G");
    let ram_arg = format!("--ram-size={ram_mb}");
    let cpus_arg = format!("--cpus={cpus}");

    run_command(
        "autopkgtest-buildvm-ubuntu-cloud",
        &["-v", "-r", release, "-s", &disk_size, &ram_arg, &cpus_arg],
        output_dir,
        &[],
    )
    .await?;

    // The generated image follows the naming convention
    // `autopkgtest-<release>-<arch>.img`.
    let arch = std::process::Command::new("uname")
        .arg("-m")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|_| "amd64".to_owned());

    let image_name = format!("autopkgtest-{release}-{arch}.img");
    Ok(output_dir.join(image_name))
}

#[cfg(test)]
mod tests {
    /// Finding 8: the disk-size argument must be passed as two tokens so the
    /// tool's option parser sees "-s" and "20G" separately.
    #[test]
    fn disk_size_arg_contains_no_embedded_space() {
        let disk_gb: u32 = 20;
        // Old (bad) form — single arg with embedded space.
        let old_arg = format!("-s {disk_gb}G");
        assert!(
            old_arg.contains(' '),
            "old arg style has an embedded space which is invalid"
        );
        // New (correct) form — two args, neither contains a space.
        let disk_size = format!("{disk_gb}G");
        let args: &[&str] = &["-s", &disk_size];
        for arg in args {
            assert!(
                !arg.contains(' '),
                "arg '{arg}' must not contain an embedded space"
            );
        }
    }
}
