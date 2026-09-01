use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use thermite::commands::tarball::TarballTarget;
use thermite::commands::{backport, tarball, update};
use thermite::error::{Result, ThermiteError};
use thermite::shell;
use thermite::types::params::{BackportParams, TarballAction, TarballParams, UpdateParams};

/// thermite — Ubuntu Rust toolchain packaging tool.
#[derive(Debug, Parser)]
#[command(name = "thermite", version, about)]
struct Cli {
    /// Increase output verbosity.
    ///
    /// Pass once (-v) to print each external command before it runs.
    /// Pass twice (-vv) to also show a concise explanation with documentation
    /// links at the start of every phase.
    #[arg(short = 'v', long, action = ArgAction::Count, global = true)]
    verbose: u8,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Package a new upstream Rust toolchain release for Ubuntu.
    Update {
        /// Full Rust version being packaged, in X.Y.Z format (e.g. 1.85.1).
        #[arg(short = 'u', long)]
        rust_update_version: String,

        /// Full Rust version being replaced, in X.Y.Z format (e.g. 1.84.0).
        #[arg(short = 'o', long)]
        rust_old_version: String,

        /// Target Ubuntu release adjective (e.g. noble).
        #[arg(short = 'r', long)]
        release: String,

        /// Launchpad username (also used as personal Git remote name).
        #[arg(short = 'l', long)]
        lpuser: String,

        /// Launchpad bug ID number for this work (digits only).
        #[arg(short = 'b', long)]
        lp_bug_number: String,

        /// Local Git remote name for the Foundations rustc repository.
        #[arg(short = 'g', long, default_value = "foundations")]
        git_remote: String,

        /// Path to the root of the Debian source package (defaults to the
        /// current working directory).
        #[arg(short = 'd', long)]
        repo_dir: Option<PathBuf>,
    },

    /// Backport an existing Rust toolchain package to an older Ubuntu release.
    Backport {
        /// Full Rust version to backport, in X.Y.Z format (e.g. 1.85.0).
        #[arg(short = 'u', long)]
        rust_version: String,

        /// Ubuntu release to backport FROM (e.g. noble, or 'devel' for the
        /// current Ubuntu development release).
        #[arg(short = 's', long)]
        source_release: String,

        /// Ubuntu release to backport TO (e.g. jammy).
        #[arg(short = 'r', long)]
        release: String,

        /// Launchpad username (also used as personal Git remote name).
        #[arg(short = 'l', long)]
        lpuser: String,

        /// Launchpad bug ID number for this backport (digits only).
        /// Omit for proactive backports that have no associated bug.
        #[arg(short = 'b', long)]
        lp_bug_number: Option<String>,

        /// Local Git remote name for the Foundations rustc repository.
        #[arg(short = 'g', long, default_value = "foundations")]
        git_remote: String,

        /// Path to the root of the Debian source package (defaults to the
        /// current working directory).
        #[arg(short = 'd', long)]
        repo_dir: Option<PathBuf>,

        /// Perform a dry run: skip all hard-to-revert operations (remote git
        /// push, PPA creation, dput uploads) and print what each would have
        /// done instead. All local operations (sbuild, lintian, etc.) still
        /// run normally.
        #[arg(long)]
        dry_run: bool,
    },

    /// Download or regenerate the orig and orig-vendor source tarballs.
    Tarball {
        #[command(subcommand)]
        action: TarballCommands,
    },
}

#[derive(Debug, Subcommand)]
enum TarballCommands {
    /// Download the tarball(s) from the staging PPA or the Ubuntu archive.
    Download {
        #[command(subcommand)]
        target: DownloadTarget,
    },
    /// Generate the tarball(s) locally (uscan or debian/rules vendor-tarball).
    Generate {
        #[command(subcommand)]
        target: GenerateTarget,
    },
}

/// Arguments shared by every `tarball` leaf.
#[derive(Debug, Args)]
struct TarballCommonArgs {
    /// Full Rust version in X.Y.Z format (e.g. 1.85.0).
    #[arg(short = 'u', long)]
    rust_version: String,

    /// Ubuntu release adjective used for backport-style tarball names
    /// (e.g. noble → '+dfsg~26.04'). Omit for plain update naming ('+dfsg').
    #[arg(long)]
    series: Option<String>,

    /// Path to the root of the Debian source package (defaults to the
    /// current working directory).
    #[arg(short = 'd', long)]
    repo_dir: Option<PathBuf>,
}

/// Overlay flags for vendor tarballs.
#[derive(Debug, Args)]
struct OverlayArgs {
    /// After obtaining the vendor tarball, extract its vendor/ directory
    /// into the repo directory (default).
    #[arg(long, overrides_with = "no_overlay")]
    overlay: bool,

    /// Do not extract the vendor tarball's vendor/ into the repo directory.
    #[arg(long)]
    no_overlay: bool,

    /// Remove the existing vendor/ directory before overlaying (clean
    /// replace) instead of merging the extraction over it.
    #[arg(long)]
    overlay_replace: bool,
}

impl OverlayArgs {
    /// Whether overlaying is enabled (`--no-overlay` opts out of the default).
    fn effective(&self) -> bool {
        self.overlay || !self.no_overlay
    }
}

/// Vendor-tarball download leaves (no `--force`: download reuses what exists).
#[derive(Debug, Subcommand)]
enum DownloadTarget {
    /// The orig tarball (filtered upstream Rust source).
    Orig(TarballCommonArgs),
    /// The orig-vendor tarball (vendored crate dependencies).
    Vendor(TarballVendorDownloadArgs),
    /// Both tarballs, orig first.
    All(TarballVendorDownloadArgs),
}

#[derive(Debug, Args)]
struct TarballVendorDownloadArgs {
    #[command(flatten)]
    common: TarballCommonArgs,

    #[command(flatten)]
    overlay: OverlayArgs,
}

/// Generate leaves (with `--force` to overwrite an existing tarball).
#[derive(Debug, Subcommand)]
enum GenerateTarget {
    /// The orig tarball (filtered upstream Rust source).
    Orig(TarballOrigGenerateArgs),
    /// The orig-vendor tarball (vendored crate dependencies).
    Vendor(TarballVendorGenerateArgs),
    /// Both tarballs, orig first.
    All(TarballVendorGenerateArgs),
}

#[derive(Debug, Args)]
struct TarballOrigGenerateArgs {
    #[command(flatten)]
    common: TarballCommonArgs,

    /// Overwrite the tarball if it already exists in the parent directory.
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Args)]
struct TarballVendorGenerateArgs {
    #[command(flatten)]
    common: TarballCommonArgs,

    /// Overwrite the tarball if it already exists in the parent directory.
    #[arg(long)]
    force: bool,

    #[command(flatten)]
    overlay: OverlayArgs,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    if let Err(e) = run().await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// Resolve the repo directory to an absolute canonical path.
fn resolve_repo_dir(repo_dir: Option<PathBuf>) -> Result<PathBuf> {
    // Canonicalize so that `repo_dir.parent().unwrap_or(repo_dir)` in
    // downstream steps resolves to the real parent directory even when a
    // relative path with no parent component (e.g. `.`) is passed:
    // `Path::new(".").parent()` returns `Some("")`, which would otherwise
    // resolve to the process cwd (the source tree itself) and cause
    // logs/tarballs to be written there.
    match repo_dir {
        Some(p) => std::fs::canonicalize(&p).map_err(ThermiteError::Io),
        None => std::env::current_dir().map_err(ThermiteError::Io),
    }
}

fn tarball_download_params(
    common: &TarballCommonArgs,
    overlay: &OverlayArgs,
) -> Result<TarballParams> {
    TarballParams::new(
        TarballAction::Download,
        &common.rust_version,
        common.series.as_deref(),
        false,
        overlay.effective(),
        overlay.overlay_replace,
    )
}

/// Overlay args for targets where overlaying does not apply (orig-only).
fn no_overlay_args() -> OverlayArgs {
    OverlayArgs {
        overlay: false,
        no_overlay: true,
        overlay_replace: false,
    }
}

fn tarball_generate_params(
    common: &TarballCommonArgs,
    force: bool,
    overlay: Option<&OverlayArgs>,
) -> Result<TarballParams> {
    TarballParams::new(
        TarballAction::Generate,
        &common.rust_version,
        common.series.as_deref(),
        force,
        overlay.is_some_and(OverlayArgs::effective),
        overlay.is_some_and(|o| o.overlay_replace),
    )
}

async fn run() -> Result<()> {
    let cli = Cli::parse();

    shell::set_verbosity(cli.verbose);

    match cli.command {
        Commands::Update {
            rust_update_version,
            rust_old_version,
            release,
            lpuser,
            lp_bug_number,
            git_remote,
            repo_dir,
        } => {
            let params = UpdateParams::new(
                &rust_update_version,
                &rust_old_version,
                &release,
                &lpuser,
                &git_remote,
                &lp_bug_number,
            )?;

            let repo_path = resolve_repo_dir(repo_dir)?;

            update::run(&params, &repo_path).await?;
        }

        Commands::Backport {
            rust_version,
            source_release,
            release,
            lpuser,
            lp_bug_number,
            git_remote,
            repo_dir,
            dry_run,
        } => {
            let params = BackportParams::new(
                &rust_version,
                &source_release,
                &release,
                &lpuser,
                &git_remote,
                lp_bug_number.as_deref(),
                dry_run,
            )?;

            let repo_path = resolve_repo_dir(repo_dir)?;

            backport::run(&params, &repo_path).await?;
        }

        Commands::Tarball { action } => {
            let (params, repo_dir, target) = match action {
                TarballCommands::Download { target } => match target {
                    DownloadTarget::Orig(args) => {
                        let params = tarball_download_params(&args, &no_overlay_args())?;
                        (params, args.repo_dir, TarballTarget::Orig)
                    }
                    DownloadTarget::Vendor(args) => {
                        let params = tarball_download_params(&args.common, &args.overlay)?;
                        (params, args.common.repo_dir, TarballTarget::Vendor)
                    }
                    DownloadTarget::All(args) => {
                        let params = tarball_download_params(&args.common, &args.overlay)?;
                        (params, args.common.repo_dir, TarballTarget::All)
                    }
                },
                TarballCommands::Generate { target } => match target {
                    GenerateTarget::Orig(args) => {
                        let params = tarball_generate_params(&args.common, args.force, None)?;
                        (params, args.common.repo_dir, TarballTarget::Orig)
                    }
                    GenerateTarget::Vendor(args) => {
                        let params =
                            tarball_generate_params(&args.common, args.force, Some(&args.overlay))?;
                        (params, args.common.repo_dir, TarballTarget::Vendor)
                    }
                    GenerateTarget::All(args) => {
                        let params =
                            tarball_generate_params(&args.common, args.force, Some(&args.overlay))?;
                        (params, args.common.repo_dir, TarballTarget::All)
                    }
                },
            };

            let repo_path = resolve_repo_dir(repo_dir)?;

            tarball::run(&params, &repo_path, target).await?;
        }
    }

    Ok(())
}
