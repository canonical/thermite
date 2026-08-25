use std::path::PathBuf;

use clap::{ArgAction, Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use thermite::commands::{backport, update};
use thermite::error::Result;
use thermite::shell;
use thermite::types::params::{BackportParams, UpdateParams};

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

            // Canonicalize so that `repo_dir.parent().unwrap_or(repo_dir)`
            // in downstream steps resolves to the real parent directory even
            // when a relative path with no parent component (e.g. `.`) is
            // passed: `Path::new(".").parent()` returns `Some("")`, which
            // would otherwise resolve to the process cwd (the source tree
            // itself) and cause logs/tarballs to be written there.
            let repo_path = match repo_dir {
                Some(p) => std::fs::canonicalize(&p).map_err(thermite::error::ThermiteError::Io)?,
                None => std::env::current_dir().map_err(thermite::error::ThermiteError::Io)?,
            };

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

            let repo_path = match repo_dir {
                Some(p) => std::fs::canonicalize(&p).map_err(thermite::error::ThermiteError::Io)?,
                None => std::env::current_dir().map_err(thermite::error::ThermiteError::Io)?,
            };

            backport::run(&params, &repo_path).await?;
        }
    }

    Ok(())
}
