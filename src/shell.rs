use std::path::Path;
use std::process::Stdio;
use std::sync::OnceLock;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tracing::{debug, info};

use crate::error::{Result, ThermiteError};

/// Whether verbose output is enabled for this process.
///
/// Set once at startup via [`set_verbose`]; read anywhere via [`is_verbose`].
static VERBOSE: OnceLock<bool> = OnceLock::new();

/// Enable or disable verbose command output.
///
/// Must be called before any commands are run.  Subsequent calls are silently
/// ignored (the flag is immutable after the first call).
pub fn set_verbose(verbose: bool) {
    let _ = VERBOSE.set(verbose);
}

/// Returns `true` if verbose mode is active.
pub fn is_verbose() -> bool {
    VERBOSE.get().copied().unwrap_or(false)
}

/// Output captured from a completed external command.
#[derive(Debug)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub status: std::process::ExitStatus,
}

/// Run an external command, streaming its output to the terminal in real time
/// and collecting it into a [`CommandOutput`].
///
/// `env` entries are added to the child process environment on top of the
/// current process's environment.
pub async fn run_command(
    program: &str,
    args: &[&str],
    cwd: &Path,
    env: &[(&str, &str)],
) -> Result<CommandOutput> {
    let display_cmd = format!("{} {}", program, args.join(" "));
    debug!("running: {display_cmd}");
    if is_verbose() {
        println!("+ {display_cmd}");
    }

    // Check the program exists on PATH before spawning so we surface a clear
    // error rather than a cryptic OS error.
    which(program)?;

    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    for (k, v) in env {
        cmd.env(k, v);
    }

    // Finding 14 (clippy): use the variant directly instead of a redundant closure.
    let mut child = cmd.spawn().map_err(ThermiteError::Io)?;

    // Finding 11: replace expect() with recoverable errors so a misconfiguration
    // returns a structured error rather than aborting the process.
    let stdout_handle = child.stdout.take().ok_or_else(|| {
        ThermiteError::Io(std::io::Error::other(
            "stdout was not piped — this is a bug in run_command",
        ))
    })?;
    let stderr_handle = child.stderr.take().ok_or_else(|| {
        ThermiteError::Io(std::io::Error::other(
            "stderr was not piped — this is a bug in run_command",
        ))
    })?;

    let mut stdout_buf = String::new();
    let mut stderr_buf = String::new();

    let mut stdout_reader = BufReader::new(stdout_handle).lines();
    let mut stderr_reader = BufReader::new(stderr_handle).lines();

    loop {
        tokio::select! {
            line = stdout_reader.next_line() => {
                match line.map_err(ThermiteError::Io)? {
                    Some(l) => {
                        println!("{l}");
                        stdout_buf.push_str(&l);
                        stdout_buf.push('\n');
                    }
                    None => break,
                }
            }
            line = stderr_reader.next_line() => {
                match line.map_err(ThermiteError::Io)? {
                    Some(l) => {
                        eprintln!("{l}");
                        stderr_buf.push_str(&l);
                        stderr_buf.push('\n');
                    }
                    None => break,
                }
            }
        }
    }

    // Drain any remaining lines after the select loop exits.
    while let Some(l) = stdout_reader.next_line().await.map_err(ThermiteError::Io)? {
        println!("{l}");
        stdout_buf.push_str(&l);
        stdout_buf.push('\n');
    }
    while let Some(l) = stderr_reader.next_line().await.map_err(ThermiteError::Io)? {
        eprintln!("{l}");
        stderr_buf.push_str(&l);
        stderr_buf.push('\n');
    }

    let status = child.wait().await.map_err(ThermiteError::Io)?;
    info!("{display_cmd} exited with {status}");

    if !status.success() {
        let code = status.code().unwrap_or(-1);
        // Finding 1: include captured stdout so callers can parse tool output
        // that was written to stdout on the failure path.
        return Err(ThermiteError::CommandFailed {
            cmd: display_cmd,
            code,
            stdout: stdout_buf,
            stderr: stderr_buf,
        });
    }

    Ok(CommandOutput {
        stdout: stdout_buf,
        stderr: stderr_buf,
        status,
    })
}

/// Run an external command that requires direct terminal access.
///
/// Unlike [`run_command`], this variant inherits stdin, stdout, and stderr
/// from the parent process so that interactive programs (e.g. `git rebase -i`
/// with an editor, or `quilt push -f --merge` with a pager) get a real TTY.
///
/// Finding 4: the piped runner used by `run_command` is not suitable for
/// interactive editor-driven workflows.
pub async fn run_interactive_command(
    program: &str,
    args: &[&str],
    cwd: &Path,
    env: &[(&str, &str)],
) -> Result<()> {
    let display_cmd = format!("{} {}", program, args.join(" "));
    debug!("running interactive: {display_cmd}");

    which(program)?;

    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(cwd);
    for (k, v) in env {
        cmd.env(k, v);
    }
    // stdin/stdout/stderr are inherited by default in tokio::process::Command.
    let status = cmd.status().await.map_err(ThermiteError::Io)?;
    info!("{display_cmd} exited with {status}");

    if !status.success() {
        let code = status.code().unwrap_or(-1);
        return Err(ThermiteError::CommandFailed {
            cmd: display_cmd,
            code,
            stdout: String::new(),
            stderr: String::new(),
        });
    }
    Ok(())
}

/// Check that `program` is available on `PATH`, returning
/// [`ThermiteError::CommandNotFound`] if it is not.
pub fn which(program: &str) -> Result<()> {
    let found = std::process::Command::new("which")
        .arg(program)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if found {
        Ok(())
    } else {
        Err(ThermiteError::CommandNotFound(program.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Finding 11: which() returns a structured error for unknown programs rather
    /// than panicking or returning a confusing OS-level error.
    #[test]
    fn which_returns_error_for_unknown_program() {
        let result = which("this-program-does-not-exist-xyz-thermite");
        assert!(
            result.is_err(),
            "expected CommandNotFound for unknown program"
        );
        assert!(matches!(result, Err(ThermiteError::CommandNotFound(_))));
    }

    /// Finding 4 / Finding 11: run_interactive_command is available as a
    /// separate function with inherited stdio (compile-time check).
    /// A real functional test would require a TTY.
    #[test]
    fn run_interactive_command_is_exported() {
        // Verify the symbol is reachable at compile time.
        // Runtime behaviour (TTY inheritance) is verified via integration tests.
        let _ = run_interactive_command as fn(_, _, _, _) -> _;
    }
}
