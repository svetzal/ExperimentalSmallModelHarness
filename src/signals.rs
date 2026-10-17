//! Requests from the operating system to stop: SIGINT, SIGTERM and SIGHUP.
//!
//! Each mode first asks its running work to stop, so a model request ends
//! with its recovery evidence intact and a running project command is
//! killed rather than abandoned. Registering a handler also removes the
//! signal's default disposition, under which it would have ended the process
//! at once, so a request that nothing honours must not leave the process
//! alive: after the grace period, or on a second signal, `force` runs and
//! ends the process with the conventional status for that signal.
use anyhow::{Context, Result};
use std::time::Duration;
use tokio::task::JoinHandle;

/// How long cooperative shutdown may take before the process ends anyway.
pub(crate) const GRACE: Duration = Duration::from_secs(2);

/// Watch for a termination signal. `request` runs once, on the first signal.
/// `force` runs with that signal's number if the process is still alive
/// after `grace` or when a second signal arrives, and must not return.
/// Abort the returned task once the work has ended on its own.
pub(crate) fn on_termination<R, F>(grace: Duration, request: R, force: F) -> Result<JoinHandle<()>>
where
    R: FnOnce() + Send + 'static,
    F: FnOnce(i32) + Send + 'static,
{
    let mut signals = Signals::new().context("registering termination signals")?;
    Ok(tokio::spawn(async move {
        let first = signals.next().await;
        request();
        tokio::select! {
            _ = tokio::time::sleep(grace) => {}
            _ = signals.next() => {}
        }
        force(first);
    }))
}

/// The exit status a shell reports for a process ended by `signal`.
pub(crate) fn exit_status(signal: i32) -> i32 {
    128 + signal
}

/// The signal's name, for a message.
pub(crate) fn name(signal: i32) -> &'static str {
    match signal {
        2 => "SIGINT",
        15 => "SIGTERM",
        1 => "SIGHUP",
        _ => "a termination signal",
    }
}

#[cfg(unix)]
struct Signals {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
    /// Absent when the process started with SIGHUP ignored, as under
    /// `nohup`: the ignore is kept, as POSIX shells expect.
    hangup: Option<tokio::signal::unix::Signal>,
}

#[cfg(unix)]
impl Signals {
    fn new() -> std::io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())?,
            terminate: signal(SignalKind::terminate())?,
            hangup: if hangup_ignored() {
                None
            } else {
                Some(signal(SignalKind::hangup())?)
            },
        })
    }

    /// The number of the next signal to arrive.
    async fn next(&mut self) -> i32 {
        let Self {
            interrupt,
            terminate,
            hangup,
        } = self;
        let hangup = async {
            match hangup {
                Some(hangup) => hangup.recv().await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            Some(()) = interrupt.recv() => libc::SIGINT,
            Some(()) = terminate.recv() => libc::SIGTERM,
            Some(()) = hangup => libc::SIGHUP,
            // A stream ends only when the runtime's signal driver is gone,
            // and then no signal is coming.
            else => std::future::pending().await,
        }
    }
}

/// Whether the process inherited SIGHUP as ignored.
#[cfg(unix)]
fn hangup_ignored() -> bool {
    // SAFETY: a null new action makes sigaction(2) only read the current
    // disposition into `old`, a zero-initialised struct of the right type
    // that lives for the whole call.
    unsafe {
        let mut old: libc::sigaction = std::mem::zeroed();
        libc::sigaction(libc::SIGHUP, std::ptr::null(), &mut old) == 0
            && old.sa_sigaction == libc::SIG_IGN
    }
}

#[cfg(not(unix))]
struct Signals;

#[cfg(not(unix))]
impl Signals {
    fn new() -> std::io::Result<Self> {
        Ok(Self)
    }

    async fn next(&mut self) -> i32 {
        let _ = tokio::signal::ctrl_c().await;
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_status_follows_the_shell_convention() {
        assert_eq!(exit_status(2), 130);
        assert_eq!(exit_status(15), 143);
        assert_eq!(exit_status(1), 129);
    }

    #[test]
    fn names_the_signals_it_handles() {
        assert_eq!(name(2), "SIGINT");
        assert_eq!(name(15), "SIGTERM");
        assert_eq!(name(1), "SIGHUP");
        assert_eq!(name(9), "a termination signal");
    }

    #[cfg(unix)]
    #[test]
    fn the_test_process_does_not_ignore_hangups() {
        // cargo starts tests with the default disposition; the check must
        // not mistake it for an ignore.
        assert!(!hangup_ignored());
    }
}
