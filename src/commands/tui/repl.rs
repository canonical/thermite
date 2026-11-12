//! The linear REPL for the accessible style.
//!
//! No alternate screen, no raw mode, no ratatui: everything is plain text in
//! the normal scrollback, so screen readers can follow the output linearly.
//! Menus are numbered lists, selections are typed numbers, and text prompts
//! are plain read-a-line questions.
//!
//! The `thermite tui` command runs this REPL when the style is `accessible`.

use std::io::{self, BufRead, Write};
use std::path::Path;

use crate::error::Result;
use crate::types::package_version::{BumpOperation, RustcPackageVersion};
use crate::types::ubuntu::UbuntuRelease;
use crate::types::versions::RustVersion;

use super::app::{
    InfoPane, InputSpec, MenuEntry, ResolvedCommand, TarballAction, TarballTarget,
    build_tarball_params, bump_menu_options, menu_bump_operation, read_info, tarball_action_label,
    tarball_target_to_command,
};
use super::execute;

/// The static command list shown in the REPL menu (plain text).
const COMMANDS: &[&str] = &[
    "build (sbuild)",
    "source build",
    "lintian",
    "bump version",
    "set version",
    "tarballs",
    "git fetch",
    "refresh info",
    "quit",
];

/// Run the REPL.
pub async fn run(repo_dir: &Path) -> Result<()> {
    let mut input = io::stdin().lock();
    run_with_input(repo_dir, &mut input).await
}

/// Run the REPL reading from `input` (testable seam).
pub(crate) async fn run_with_input(repo_dir: &Path, input: &mut dyn BufRead) -> Result<()> {
    let mut info = read_info(repo_dir);
    let mut last_release: Option<String> = None;
    println!("thermite tui (accessible) — plain-text mode for screen readers");
    println!("type a command number to run it, or q to quit");
    println!();
    print_info(&info);
    loop {
        print_menu();
        let Some(choice) = read_line(input, "Enter a number: ") else {
            return Ok(());
        };
        let choice = choice.trim();
        if choice.is_empty()
            || choice.eq_ignore_ascii_case("q")
            || choice.eq_ignore_ascii_case("quit")
        {
            return Ok(());
        }
        let index = match choice.parse::<usize>() {
            Ok(i) if (1..=COMMANDS.len()).contains(&i) => i - 1,
            _ => {
                println!("  invalid choice: {choice} (enter 1 to {})", COMMANDS.len());
                continue;
            }
        };
        match index {
            7 => {
                info = read_info(repo_dir);
                println!();
                print_info(&info);
                continue;
            }
            8 => return Ok(()),
            _ => {}
        }
        let command = match resolve_command(index, &info, &mut last_release, input) {
            Ok(Some(c)) => c,
            Ok(None) => continue,
            Err(e) => {
                println!("  {e}");
                continue;
            }
        };
        let description = command.description();
        let result = execute(&command, repo_dir).await;
        match result {
            Ok(summary) => println!("== {description}: {summary}"),
            Err(e) => println!("== {description} FAILED: {e}"),
        }
        // Re-read the info after every command; re-print it after ones that
        // change the changelog.
        let changed = matches!(
            command,
            ResolvedCommand::Bump { .. }
                | ResolvedCommand::SetVersion { .. }
                | ResolvedCommand::Tarball { .. }
        );
        let new_info = read_info(repo_dir);
        if changed {
            println!();
            print_info(&new_info);
        }
        info = new_info;
    }
}

/// Resolve a menu selection into a command, prompting for any inputs.
///
/// Returns `None` when the user cancelled (EOF) mid-prompt.
fn resolve_command(
    index: usize,
    info: &InfoPane,
    last_release: &mut Option<String>,
    input: &mut dyn BufRead,
) -> Result<Option<ResolvedCommand>> {
    let command = match index {
        0 => {
            // build (sbuild)
            let default = last_release.clone().unwrap_or_default();
            let Some(value) = read_line(input, &sbuild_release_prompt(&default)) else {
                return Ok(None);
            };
            let value = if value.trim().is_empty() && !default.is_empty() {
                default.clone()
            } else {
                value.trim().to_owned()
            };
            UbuntuRelease::parse(&value)?;
            *last_release = Some(value.clone());
            Some(ResolvedCommand::Build { release: value })
        }
        1 => Some(ResolvedCommand::SourceBuild),
        2 => Some(ResolvedCommand::Lintian),
        3 => resolve_bump_command(input)?,
        4 => {
            // set version
            let default = info.version.clone().unwrap_or_default();
            let Some(value) = read_line(input, &set_version_prompt(&default)) else {
                return Ok(None);
            };
            let value = if value.trim().is_empty() && !default.is_empty() {
                default.clone()
            } else {
                value.trim().to_owned()
            };
            RustcPackageVersion::parse(&value)?;
            Some(ResolvedCommand::SetVersion { version: value })
        }
        5 => resolve_tarball_command(info, input)?,
        6 => Some(ResolvedCommand::GitFetch),
        _ => return Ok(None),
    };
    Ok(command)
}

/// Resolve the bump-version command (menu + optional follow-up input).
fn resolve_bump_command(input: &mut dyn BufRead) -> Result<Option<ResolvedCommand>> {
    let options = bump_menu_options();
    let Some(index) = select_numbered(input, "bump operation", &options) else {
        return Ok(None);
    };
    match menu_bump_operation(index) {
        MenuEntry::Run(operation) => Ok(Some(ResolvedCommand::Bump { operation })),
        MenuEntry::Input(spec) => {
            let title = match &spec {
                InputSpec::BumpPatchUpstream => "new upstream version (X.Y.Z)",
                InputSpec::BumpSeries { .. } => {
                    "target series (e.g. 24.04) or release adjective (e.g. noble)"
                }
                _ => "value",
            };
            let Some(value) = read_line(input, &format!("  {title}: ")) else {
                return Ok(None);
            };
            let value = value.trim().to_owned();
            match spec {
                InputSpec::BumpPatchUpstream => {
                    RustVersion::parse(&value)?;
                    Ok(Some(ResolvedCommand::Bump {
                        operation: BumpOperation::PatchRelease { upstream: value },
                    }))
                }
                InputSpec::BumpSeries { marker } => {
                    // Accept either a numeric series (e.g. "24.04") or an
                    // Ubuntu release adjective (e.g. "noble").
                    let (series, release): (Option<String>, Option<String>) = if value.contains('.')
                    {
                        (Some(value), None)
                    } else {
                        UbuntuRelease::parse(&value)?;
                        (None, Some(value))
                    };
                    let operation = match marker {
                        BumpOperation::Backport { .. } => {
                            BumpOperation::Backport { series, release }
                        }
                        _ => BumpOperation::Retarget { series, release },
                    };
                    Ok(Some(ResolvedCommand::Bump { operation }))
                }
                _ => Ok(None),
            }
        }
    }
}

/// Resolve the tarball command (menu + version/series/flag wizard).
fn resolve_tarball_command(
    info: &InfoPane,
    input: &mut dyn BufRead,
) -> Result<Option<ResolvedCommand>> {
    let Some(index) = select_numbered(input, "tarball action", &tarball_menu_options()) else {
        return Ok(None);
    };
    let (action, target) = tarball_menu_entry(index);
    let Some(version) = read_tarball_version(info, input)? else {
        return Ok(None);
    };
    let Some(series) = read_tarball_series(info, input)? else {
        return Ok(None);
    };
    let needs_flag = matches!(
        (action, target),
        (TarballAction::Generate, _)
            | (
                TarballAction::Overlay,
                TarballTarget::Vendor | TarballTarget::All
            )
    );
    let flag = if needs_flag {
        let question = if action == TarballAction::Generate {
            "overwrite an existing tarball? (y/N)"
        } else {
            "clean-replace vendor/ before overlay? (y/N)"
        };
        let Some(value) = read_line(input, &format!("  {question}: ")) else {
            return Ok(None);
        };
        matches!(value.trim(), "y" | "Y" | "yes" | "Yes")
    } else {
        false
    };
    let series: Option<&str> = (!series.is_empty()).then_some(series.as_str());
    let params = build_tarball_params(action, &version, series, flag)?;
    Ok(Some(ResolvedCommand::Tarball {
        params,
        target: tarball_target_to_command(target),
    }))
}

/// Print the package info block.
fn print_info(info: &InfoPane) {
    println!("Package info:");
    for line in info.display_lines() {
        println!("{line}");
    }
    println!();
}

/// Print the numbered command menu.
fn print_menu() {
    println!("Commands:");
    for (i, label) in COMMANDS.iter().enumerate() {
        println!("  {}. {label}", i + 1);
    }
}

/// Print a numbered list and return the parsed index (re-prompting on
/// invalid input). Returns `None` on EOF.
fn select_numbered(input: &mut dyn BufRead, prompt: &str, options: &[String]) -> Option<usize> {
    println!("{prompt}:");
    for (i, option) in options.iter().enumerate() {
        println!("  {}. {option}", i + 1);
    }
    loop {
        let value = read_line(input, &format!("  {prompt} (1 to {}): ", options.len()))?;
        match value.trim().parse::<usize>() {
            Ok(i) if (1..=options.len()).contains(&i) => return Some(i - 1),
            _ => {
                println!(
                    "  invalid choice: {} (enter 1 to {})",
                    value.trim(),
                    options.len()
                );
            }
        }
    }
}

/// Read a line from `input` (trimmed). Returns `None` on EOF.
fn read_line(input: &mut dyn BufRead, prompt: &str) -> Option<String> {
    print!("{prompt}");
    io::stdout().flush().ok();
    let mut buf = String::new();
    let n = input.read_line(&mut buf).ok()?;
    if n == 0 {
        return None; // EOF with no content
    }
    Some(buf.trim().to_owned())
}

/// Prompt for the sbuild release, prefilled from the last release used.
fn sbuild_release_prompt(default: &str) -> String {
    if default.is_empty() {
        "  sbuild release (Ubuntu adjective, e.g. noble): ".to_owned()
    } else {
        format!("  sbuild release (Ubuntu adjective, e.g. noble) [{default}]: ")
    }
}

/// Prompt for the set-version value, prefilled from the changelog version.
fn set_version_prompt(default: &str) -> String {
    if default.is_empty() {
        "  set version (full rustc package version string): ".to_owned()
    } else {
        format!("  set version (full rustc package version string) [{default}]: ")
    }
}

/// The tarball menu option labels (index-aligned with
/// [`tarball_menu_entry`]).
fn tarball_menu_options() -> Vec<String> {
    let mut options = Vec::new();
    for action in [
        TarballAction::Download,
        TarballAction::Generate,
        TarballAction::Overlay,
    ] {
        for target in [
            TarballTarget::Orig,
            TarballTarget::Vendor,
            TarballTarget::All,
        ] {
            options.push(format!(
                "{} {}",
                tarball_action_label(action),
                target.label()
            ));
        }
    }
    options
}

/// Map a tarball menu selection index to its `(action, target)` pair
/// (download orig/vendor/all = 0..2, generate = 3..5, overlay = 6..8).
fn tarball_menu_entry(index: usize) -> (TarballAction, TarballTarget) {
    let action = match index {
        0..=2 => TarballAction::Download,
        3..=5 => TarballAction::Generate,
        _ => TarballAction::Overlay,
    };
    let target = match index {
        0 | 3 | 6 => TarballTarget::Orig,
        1 | 4 | 7 => TarballTarget::Vendor,
        _ => TarballTarget::All,
    };
    (action, target)
}

/// Prompt for the tarball rust version (prefilled from the changelog upstream
/// version).
fn read_tarball_version(info: &InfoPane, input: &mut dyn BufRead) -> Result<Option<String>> {
    let default = info.upstream.clone().unwrap_or_default();
    let prompt = if default.is_empty() {
        "  tarball rust version (X.Y.Z): ".to_owned()
    } else {
        format!("  tarball rust version (X.Y.Z) [{default}]: ")
    };
    let Some(value) = read_line(input, &prompt) else {
        return Ok(None);
    };
    let value = if value.trim().is_empty() && !default.is_empty() {
        default.clone()
    } else {
        value.trim().to_owned()
    };
    RustVersion::parse(&value)?;
    Ok(Some(value))
}

/// Prompt for the tarball series adjective (empty = plain update naming,
/// prefilled from the changelog series).
fn read_tarball_series(info: &InfoPane, input: &mut dyn BufRead) -> Result<Option<String>> {
    let default = info
        .series
        .as_deref()
        .and_then(UbuntuRelease::release_from_series)
        .map(str::to_owned)
        .unwrap_or_default();
    let prompt = if default.is_empty() {
        "  tarball series adjective (empty = plain update naming): ".to_owned()
    } else {
        format!("  tarball series adjective (empty = plain update naming) [{default}]: ")
    };
    let Some(value) = read_line(input, &prompt) else {
        return Ok(None);
    };
    let value = if value.trim().is_empty() && !default.is_empty() {
        default.clone()
    } else {
        value.trim().to_owned()
    };
    // Empty = plain update naming; non-empty must be a valid release adjective.
    if !value.is_empty() {
        UbuntuRelease::parse(&value)?;
    }
    Ok(Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A package tree with a changelog, for the info-pane fixture.
    fn scratch_info() -> InfoPane {
        let dir = std::env::temp_dir().join(format!(
            "thermite-tui-repl-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("debian")).unwrap();
        std::fs::write(
            dir.join("debian/changelog"),
            "rustc-1.95 (1.95.0+dfsg-0ubuntu1) plucky; urgency=medium\n\n  * test\n",
        )
        .unwrap();
        read_info(&dir)
    }

    /// The tarball menu entries are index-aligned with the option labels.
    #[test]
    fn tarball_menu_entries_are_index_aligned() {
        let options = tarball_menu_options();
        assert_eq!(options.len(), 9);
        assert_eq!(options[0], "download orig");
        assert_eq!(options[8], "overlay all");
        assert_eq!(
            tarball_menu_entry(0),
            (TarballAction::Download, TarballTarget::Orig)
        );
        assert_eq!(
            tarball_menu_entry(2),
            (TarballAction::Download, TarballTarget::All)
        );
        assert_eq!(
            tarball_menu_entry(3),
            (TarballAction::Generate, TarballTarget::Orig)
        );
        assert_eq!(
            tarball_menu_entry(8),
            (TarballAction::Overlay, TarballTarget::All)
        );
    }

    #[test]
    fn command_menu_has_nine_entries() {
        assert_eq!(COMMANDS.len(), 9);
        assert_eq!(COMMANDS[0], "build (sbuild)");
        assert_eq!(COMMANDS[8], "quit");
    }

    #[test]
    fn prompts_prefill_with_default() {
        assert_eq!(
            sbuild_release_prompt(""),
            "  sbuild release (Ubuntu adjective, e.g. noble): "
        );
        assert_eq!(
            sbuild_release_prompt("noble"),
            "  sbuild release (Ubuntu adjective, e.g. noble) [noble]: "
        );
        assert_eq!(
            set_version_prompt("1.95.0+dfsg-0ubuntu1"),
            "  set version (full rustc package version string) [1.95.0+dfsg-0ubuntu1]: "
        );
    }

    #[test]
    fn build_flow_validates_release_adjective() {
        let info = scratch_info();
        let mut last_release: Option<String> = None;
        let mut input = &b"noble\n"[..];
        let command = resolve_command(0, &info, &mut last_release, &mut input)
            .unwrap()
            .unwrap();
        assert_eq!(command.description(), "sbuild (noble)");
        assert_eq!(last_release.as_deref(), Some("noble"));
        // Invalid adjective is rejected.
        let mut input = &b"not-a-release\n"[..];
        assert!(resolve_command(0, &info, &mut last_release, &mut input).is_err());
        // EOF (empty input) cancels the prompt.
        let mut input = &b""[..];
        assert!(
            resolve_command(0, &info, &mut last_release, &mut input)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn build_prompt_prefills_from_last_release() {
        let info = scratch_info();
        let mut last_release: Option<String> = None;
        let mut input = &b"noble\n"[..];
        resolve_command(0, &info, &mut last_release, &mut input).unwrap();
        // Empty input picks up the default from the last release.
        let mut input = &b"\n"[..];
        let command = resolve_command(0, &info, &mut last_release, &mut input)
            .unwrap()
            .unwrap();
        assert_eq!(command.description(), "sbuild (noble)");
    }

    #[test]
    fn no_input_commands_resolve_immediately() {
        let info = scratch_info();
        let mut last_release: Option<String> = None;
        for (index, expected) in [
            (1usize, "source build"),
            (2, "lintian"),
            (6, "git fetch --all"),
        ] {
            let mut input = &b""[..];
            let command = resolve_command(index, &info, &mut last_release, &mut input)
                .unwrap()
                .unwrap();
            assert_eq!(command.description(), expected);
        }
    }

    #[test]
    fn set_version_flow_validates() {
        let info = scratch_info();
        let mut last_release: Option<String> = None;
        // Valid version.
        let mut input = &b"1.96.0+dfsg-0ubuntu1\n"[..];
        let command = resolve_command(4, &info, &mut last_release, &mut input)
            .unwrap()
            .unwrap();
        assert_eq!(command.description(), "set version");
        // Invalid version is rejected.
        let mut input = &b"bogus\n"[..];
        assert!(resolve_command(4, &info, &mut last_release, &mut input).is_err());
        // Empty input picks up the changelog version as the default.
        let mut input = &b"\n"[..];
        let command = resolve_command(4, &info, &mut last_release, &mut input)
            .unwrap()
            .unwrap();
        assert_eq!(command.description(), "set version");
    }

    #[test]
    fn bump_menu_maps_entries() {
        let info = scratch_info();
        let mut last_release: Option<String> = None;
        // "ubuntu revision" (index 1 in the bump menu) runs immediately.
        let mut input = &b"2\n"[..];
        let command = resolve_command(3, &info, &mut last_release, &mut input)
            .unwrap()
            .unwrap();
        assert_eq!(command.description(), "bump version");
        // Invalid menu choice re-prompts ("99" then "1" = patch release).
        let mut input = &b"99\n1\n1.96.0\n"[..];
        let command = resolve_command(3, &info, &mut last_release, &mut input)
            .unwrap()
            .unwrap();
        assert_eq!(command.description(), "bump version");
        // EOF cancels the menu.
        let mut input = &b""[..];
        assert!(
            resolve_command(3, &info, &mut last_release, &mut input)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn bump_menu_validates_follow_up_input() {
        let info = scratch_info();
        let mut last_release: Option<String> = None;
        // Patch release with a valid upstream version.
        let mut input = &b"1\n1.96.0\n"[..];
        let command = resolve_command(3, &info, &mut last_release, &mut input)
            .unwrap()
            .unwrap();
        assert_eq!(command.description(), "bump version");
        // Patch release with an invalid upstream version is rejected.
        let mut input = &b"1\nbogus\n"[..];
        assert!(resolve_command(3, &info, &mut last_release, &mut input).is_err());
        // Backport with a numeric series.
        let mut input = &b"4\n24.04\n"[..];
        let command = resolve_command(3, &info, &mut last_release, &mut input)
            .unwrap()
            .unwrap();
        assert_eq!(command.description(), "bump version");
        // Backport with an invalid value is rejected.
        let mut input = &b"4\nbogus\n"[..];
        assert!(resolve_command(3, &info, &mut last_release, &mut input).is_err());
    }

    #[test]
    fn tarball_wizard_download_orig() {
        let info = scratch_info();
        let mut last_release: Option<String> = None;
        // download orig, version "1.96.0", empty series, no flag prompt.
        let mut input = &b"1\n1.96.0\n\n"[..];
        let command = resolve_command(5, &info, &mut last_release, &mut input)
            .unwrap()
            .unwrap();
        assert_eq!(command.description(), "tarball download orig");
    }

    #[test]
    fn tarball_wizard_generate_orig_flag() {
        let info = scratch_info();
        let mut last_release: Option<String> = None;
        // generate orig with the flag: version, series, "y".
        let mut input = &b"4\n1.96.0\n\ny\n"[..];
        let command = resolve_command(5, &info, &mut last_release, &mut input)
            .unwrap()
            .unwrap();
        assert_eq!(command.description(), "tarball generate orig");
        // generate orig declining the flag: "n".
        let mut input = &b"4\n1.96.0\n\nn\n"[..];
        let command = resolve_command(5, &info, &mut last_release, &mut input)
            .unwrap()
            .unwrap();
        assert_eq!(command.description(), "tarball generate orig");
        // generate orig with EOF at the flag prompt cancels.
        let mut input = &b"4\n1.96.0\n\n"[..];
        assert!(
            resolve_command(5, &info, &mut last_release, &mut input)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn tarball_wizard_series_adjective() {
        let info = scratch_info();
        let mut last_release: Option<String> = None;
        // download orig with a series adjective; the default is prefilled.
        let mut input = &b"1\n1.96.0\nnoble\n"[..];
        let command = resolve_command(5, &info, &mut last_release, &mut input)
            .unwrap()
            .unwrap();
        assert_eq!(command.description(), "tarball download orig");
        // Invalid series adjective is rejected.
        let mut input = &b"1\n1.96.0\nnot-a-release\n"[..];
        assert!(resolve_command(5, &info, &mut last_release, &mut input).is_err());
    }

    #[test]
    fn tarball_wizard_rejects_invalid_version() {
        let info = scratch_info();
        let mut last_release: Option<String> = None;
        let mut input = &b"1\nbogus\n"[..];
        assert!(resolve_command(5, &info, &mut last_release, &mut input).is_err());
    }

    #[test]
    fn quit_on_empty_input() {
        let dir = std::env::temp_dir().join(format!(
            "thermite-tui-repl-quit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(dir.join("debian")).unwrap();
        std::fs::write(
            dir.join("debian/changelog"),
            "rustc-1.95 (1.95.0+dfsg-0ubuntu1) plucky; urgency=medium\n\n  * test\n",
        )
        .unwrap();
        // Empty input quits immediately, without executing anything.
        let mut input = &b"\n"[..];
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            run_with_input(&dir, &mut input).await.unwrap();
        });
        // `q` also quits.
        let mut input = &b"q\n"[..];
        runtime.block_on(async {
            run_with_input(&dir, &mut input).await.unwrap();
        });
        // Invalid choice is reported and re-prompted; then quit.
        let mut input = &b"99\nquit\n"[..];
        runtime.block_on(async {
            run_with_input(&dir, &mut input).await.unwrap();
        });
    }
}
