use std::path::Path;
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::watch;
use tracing::{debug, info};

use crate::error::{Result, ThermiteError};

/// Global verbosity level for this process.
///
/// | Level | Flag  | Effect                                                   |
/// |-------|-------|----------------------------------------------------------|
/// | 0     | (none)| Normal output only                                       |
/// | 1     | `-v`  | Print each external command before it is executed        |
/// | 2     | `-vv` | Level 1 + per-phase explanations with documentation links|
///
/// Set once at startup via [`set_verbosity`]; read anywhere via
/// [`verbosity`] or the convenience helper [`is_verbose`].
static VERBOSITY: OnceLock<u8> = OnceLock::new();

/// Global output sink for the TUI.
///
/// When set, every line that would normally be printed to stdout/stderr
/// (external command output, `verbose` command echoes, `ui` helpers, etc.)
/// is sent to this channel instead.  The TUI event loop consumes the channel
/// and renders the lines in its log pane, so the alternate screen is never
/// corrupted by stray writes.
///
/// Set via [`set_output_sink`] before entering the TUI and cleared via
/// [`clear_output_sink`] on exit.  When unset, all output goes to the real
/// terminal as before.
static OUTPUT_SINK: Mutex<Option<UnboundedSender<String>>> = Mutex::new(None);

/// Route all process output into the given channel.
pub fn set_output_sink(tx: UnboundedSender<String>) {
    *OUTPUT_SINK.lock().expect("OUTPUT_SINK mutex poisoned") = Some(tx);
}

/// Remove the output sink, restoring direct terminal output.
pub fn clear_output_sink() {
    *OUTPUT_SINK.lock().expect("OUTPUT_SINK mutex poisoned") = None;
}

/// Whether a TUI output sink is currently active.
pub fn is_sink_active() -> bool {
    OUTPUT_SINK
        .lock()
        .expect("OUTPUT_SINK mutex poisoned")
        .is_some()
}

// ---------------------------------------------------------------------------
// Cancellation token
// ---------------------------------------------------------------------------

/// Global cancellation token sender.
///
/// Installed in TUI mode via [`install_cancellation`].  Flipping the token to
/// `true` with [`cancel`] makes every [`run_command`] invocation kill its
/// child process (or refuse to spawn a new one) and return
/// [`ThermiteError::Cancelled`].  The token is **level-triggered**: it stays
/// cancelled until [`reset_cancellation`] flips it back, so a multi-step
/// workflow (e.g. a tarball generate that runs uscan, then a vendor rule)
/// aborts at the next step boundary after the user cancels.
///
/// Commands run outside a cancellation context (the plain CLI) are never
/// cancellable — `run_command` simply watches a channel that never fires.
static CANCEL: Mutex<Option<watch::Sender<bool>>> = Mutex::new(None);

/// Install the process-wide cancellation token.  No-op if already installed.
pub fn install_cancellation() {
    let mut guard = CANCEL.lock().expect("CANCEL mutex poisoned");
    if guard.is_none() {
        *guard = Some(watch::Sender::new(false));
    }
}

/// Remove the cancellation token (TUI teardown).
pub fn clear_cancellation() {
    *CANCEL.lock().expect("CANCEL mutex poisoned") = None;
}

/// Cancel the currently running command (and any subsequent steps until a
/// reset).  No-op when no token is installed.
pub fn cancel() {
    if let Some(tx) = CANCEL.lock().expect("CANCEL mutex poisoned").as_ref() {
        // send() silently drops the value when no receiver is alive (e.g.
        // between two steps), leaving the token stale — send_replace always
        // stores it.
        tx.send_replace(true);
    }
}

/// Reset the token to "not cancelled" — called when a new TUI command starts.
pub fn reset_cancellation() {
    if let Some(tx) = CANCEL.lock().expect("CANCEL mutex poisoned").as_ref() {
        // See cancel(): send() fails with zero receivers, which wedged the
        // token at "cancelled" and killed every subsequent TUI command.
        tx.send_replace(false);
    }
}

/// A `watch::Receiver<bool>` for the global token, or `None` outside TUI mode.
fn cancellation_receiver() -> Option<watch::Receiver<bool>> {
    CANCEL
        .lock()
        .expect("CANCEL mutex poisoned")
        .as_ref()
        .map(watch::Sender::subscribe)
}

/// Emit one line of process output.
///
/// Lines go to the output sink when a TUI is running, otherwise straight to
/// stdout.  All user-facing printing in code that may run under the TUI must
/// go through this helper (or a `ui::print_*` helper, which uses it) rather
/// than calling `println!` directly, so that the TUI's alternate screen is
/// never corrupted.
pub fn out_line(line: &str) {
    if let Some(tx) = OUTPUT_SINK
        .lock()
        .expect("OUTPUT_SINK mutex poisoned")
        .as_ref()
    {
        let _ = tx.send(line.to_owned());
    } else {
        println!("{line}");
    }
}

/// Set the process-wide verbosity level.
///
/// Must be called before any commands are run.  Subsequent calls are silently
/// ignored (the level is immutable after the first call).
pub fn set_verbosity(level: u8) {
    let _ = VERBOSITY.set(level);
}

/// Returns the current verbosity level (0 = quiet, 1 = `-v`, 2 = `-vv`).
pub fn verbosity() -> u8 {
    VERBOSITY.get().copied().unwrap_or(0)
}

/// Returns `true` if at least one `-v` flag was given (`verbosity >= 1`).
pub fn is_verbose() -> bool {
    verbosity() >= 1
}

/// Output captured from a completed external command.
#[derive(Debug)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub status: std::process::ExitStatus,
}

/// Wait for a cancellation signal on `rx`, returning whether it fired.
///
/// A dropped sender (TUI teardown while a command is still running) is
/// treated as "no cancellation" — the command then just runs to completion.
async fn wait_for_cancel(rx: &mut watch::Receiver<bool>) -> bool {
    let _ = rx.changed().await;
    *rx.borrow()
}

/// Run an external command, streaming its output to the terminal in real time
/// and collecting it into a [`CommandOutput`].
///
/// `env` entries are added to the child process environment on top of the
/// current process's environment.
///
/// In TUI mode (cancellation token installed) the command is cancellable: a
/// [`cancel`] signal kills the child and returns [`ThermiteError::Cancelled`].
pub async fn run_command(
    program: &str,
    args: &[&str],
    cwd: &Path,
    env: &[(&str, &str)],
) -> Result<CommandOutput> {
    let cancel_rx = cancellation_receiver();
    run_command_with_cancel(cancel_rx, program, args, cwd, env).await
}

/// Like [`run_command`], but with an explicit cancellation receiver.
///
/// Passing `None` makes the command uncancellable (plain CLI behaviour).
/// Tests use their own private channel so the process-global token is never
/// involved; production code should use [`run_command`].
pub async fn run_command_with_cancel(
    mut cancel_rx: Option<watch::Receiver<bool>>,
    program: &str,
    args: &[&str],
    cwd: &Path,
    env: &[(&str, &str)],
) -> Result<CommandOutput> {
    let display_cmd = format!("{} {}", program, args.join(" "));
    debug!("running: {display_cmd}");
    if is_verbose() {
        out_line(&format!("+ {display_cmd}"));
    }

    // Refuse to spawn when the token is already cancelled: a multi-step
    // workflow aborts at the next step boundary after the user cancels.
    if cancel_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
        return Err(ThermiteError::Cancelled);
    }

    // Check the program exists before spawning so we surface a clear error
    // rather than a cryptic OS error.
    //
    // Programs that contain '/' are relative or absolute path references
    // (e.g. "debian/rules"), not PATH lookups; verify them as files relative
    // to `cwd` rather than searching PATH.
    if program.contains('/') {
        let full_path = cwd.join(program);
        if !full_path.exists() {
            return Err(ThermiteError::CommandNotFound(program.to_owned()));
        }
    } else {
        which(program)?;
    }

    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Put the child in its own process group so a cancellation can kill
        // the whole tree — tools like `git fetch` spawn helpers (remote
        // helpers, etc.) that inherit the output pipes and would otherwise
        // keep the drain loop below waiting for EOF forever.
        .process_group(0)
        // Safety net: if the child is dropped without being awaited (e.g. the
        // TUI exits while a command is running), make sure it dies.
        .kill_on_drop(true);

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

    let mut was_cancelled = false;
    loop {
        tokio::select! {
            line = stdout_reader.next_line() => {
                match line.map_err(ThermiteError::Io)? {
                    Some(l) => {
                        out_line(&l);
                        stdout_buf.push_str(&l);
                        stdout_buf.push('\n');
                    }
                    None => break,
                }
            }
            line = stderr_reader.next_line() => {
                match line.map_err(ThermiteError::Io)? {
                    Some(l) => {
                        out_line(&l);
                        stderr_buf.push_str(&l);
                        stderr_buf.push('\n');
                    }
                    None => break,
                }
            }
            cancelled = async {
                match cancel_rx.as_mut() {
                    Some(rx) => wait_for_cancel(rx).await,
                    None => std::future::pending().await,
                }
            } => {
                if cancelled {
                    was_cancelled = true;
                    debug!("cancelling: {display_cmd}");
                    out_line("  ^C — cancelling");
                    // Kill the whole process group: the direct child plus any
                    // helpers it spawned that share the output pipes.
                    if let Some(pid) = child.id() {
                        // SAFETY: killpg only signals the process group we
                        // created for this child; nothing else is touched.
                        unsafe {
                            libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
                        }
                    }
                    let _ = child.start_kill(); // belt and braces
                    break;
                }
                // Reset to false: a new TUI command started. Keep running.
            }
        }
    }

    // Drain any remaining lines after the select loop exits.  Skipped on
    // cancellation: the killed process group will EOF momentarily, but the
    // outcome is already known and there is no value in waiting for it.
    if !was_cancelled {
        while let Some(l) = stdout_reader.next_line().await.map_err(ThermiteError::Io)? {
            out_line(&l);
            stdout_buf.push_str(&l);
            stdout_buf.push('\n');
        }
        while let Some(l) = stderr_reader.next_line().await.map_err(ThermiteError::Io)? {
            out_line(&l);
            stderr_buf.push_str(&l);
            stderr_buf.push('\n');
        }
    }

    let status = child.wait().await.map_err(ThermiteError::Io)?;
    info!("{display_cmd} exited with {status}");

    // The user cancelled: report that instead of a CommandFailed for the
    // killed child's non-zero exit.
    if was_cancelled {
        return Err(ThermiteError::Cancelled);
    }

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
pub fn which(program: &str) -> Result<String> {
    let output = std::process::Command::new("which")
        .arg(program)
        .output()
        .map_err(|_e| ThermiteError::CommandNotFound(program.to_owned()))?;

    if !output.status.success() {
        return Err(ThermiteError::CommandNotFound(program.to_owned()));
    }

    let output = String::from_utf8(output.stdout)
        .map_err(|_e| ThermiteError::CommandOutputParseError(program.to_owned()))?;

    Ok(output)
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

    /// The TUI output sink routes `out_line` output into the channel and
    /// `is_sink_active` reflects the state.  The sink is global state, so the
    /// test must always clear it before returning.
    #[test]
    fn output_sink_routes_lines_and_clears() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        set_output_sink(tx);
        assert!(is_sink_active());

        out_line("hello tui");
        assert_eq!(
            rx.try_recv().expect("line should be in the sink"),
            "hello tui"
        );

        clear_output_sink();
        assert!(!is_sink_active());
    }

    /// Create a uniquely named scratch directory (project convention: no
    /// tempfile dev-dependency).
    fn make_scratch_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "thermite-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A pre-cancelled token must make `run_command` refuse to spawn: the
    /// next step of a multi-step workflow aborts after the user cancels.
    #[tokio::test]
    async fn cancel_before_spawn_returns_cancelled() {
        let (tx, rx) = tokio::sync::watch::channel(false);
        tx.send(true).unwrap();

        let result = run_command_with_cancel(
            Some(rx),
            "sleep",
            &["0.1"],
            &make_scratch_dir("cancel-before-spawn"),
            &[],
        )
        .await;
        assert!(matches!(result, Err(ThermiteError::Cancelled)));
    }

    /// Cancelling a running command kills the child (SIGKILL) and returns
    /// `Cancelled` quickly — the test must not wait for the full 30s sleep.
    #[tokio::test]
    async fn cancel_mid_run_kills_child() {
        let (tx, rx) = tokio::sync::watch::channel(false);
        let dir = make_scratch_dir("cancel-mid-run");

        // Cancel from a plain thread so the command future owns its own
        // borrows (tokio::spawn would require a 'static future).
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            tx.send(true).unwrap();
        });

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            run_command_with_cancel(Some(rx), "sleep", &["30"], &dir, &[]),
        )
        .await
        .expect("cancelled command should finish quickly");
        canceller.join().unwrap();
        assert!(matches!(result, Err(ThermiteError::Cancelled)));
    }

    /// Without a cancellation receiver (plain CLI), the command runs to
    /// completion normally — the watch branch never fires.
    #[tokio::test]
    async fn no_cancellation_receiver_runs_normally() {
        let result = run_command_with_cancel(
            None,
            "echo",
            &["hello"],
            &make_scratch_dir("no-cancel"),
            &[],
        )
        .await;
        let out = result.expect("echo should succeed");
        assert!(out.stdout.contains("hello"));
    }

    /// Regression: `cancel`/`reset_cancellation` must store the token value
    /// even when no receiver is alive (the window between two TUI commands).
    /// `watch::Sender::send` silently fails in that window, which wedged the
    /// token at "cancelled" and made every later TUI command refuse to spawn.
    #[test]
    fn cancel_and_reset_store_value_without_receivers() {
        install_cancellation();
        cancel();
        assert!(*cancellation_receiver().unwrap().borrow());
        reset_cancellation();
        assert!(!*cancellation_receiver().unwrap().borrow());
        clear_cancellation();
    }
}
