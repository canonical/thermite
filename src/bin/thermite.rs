use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use thermite::commands::update;
use thermite::error::Result;
use thermite::shell;
use thermite::types::params::UpdateParams;

/// thermite — Ubuntu Rust toolchain packaging tool.
#[derive(Debug, Parser)]
#[command(name = "thermite", version, about)]
struct Cli {
    /// Print each external command before it is executed.
    #[arg(short = 'v', long, global = true)]
    verbose: bool,

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

    shell::set_verbose(cli.verbose);

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

            // Finding 11: replace expect() with structured error propagation.
            let repo_path = match repo_dir {
                Some(p) => p,
                None => std::env::current_dir().map_err(thermite::error::ThermiteError::Io)?,
            };

            update::run(&params, &repo_path).await?;
        }
    }

    Ok(())
}
