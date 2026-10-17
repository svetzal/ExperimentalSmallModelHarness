//! Waiting for terminal input without reading it.
//!
//! crossterm's Unix reader loops on `read(2)` until it has parsed an event.
//! A descriptor at end of file, or in error, returns at once with nothing,
//! so once the terminal has gone (its emulator quit, or the parent that held
//! the pseudo-terminal exited) `event::poll` never returns and the process
//! spins on one core. The wait here is therefore a `poll(2)` on standard
//! input, which reports a hang-up or an invalid descriptor without reading.
//! crossterm reads only after the wait, and only with a zero timeout.
//!
//! Standard input is the right descriptor because the conversation refuses
//! to start unless it is a terminal, and crossterm reads standard input
//! whenever it is one (it opens `/dev/tty` only otherwise).
use std::io;
use std::time::Duration;

/// What a wait for input found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Wait {
    /// Bytes are ready for crossterm to read.
    Ready,
    /// The wait ended with nothing to read.
    Idle,
    /// The terminal has gone: its other end hung up, it reported an error,
    /// or standard input is not an open descriptor.
    Closed,
}

/// Wait up to `timeout` for input on standard input.
#[cfg(unix)]
pub(super) fn wait(timeout: Duration) -> io::Result<Wait> {
    wait_on(libc::STDIN_FILENO, timeout)
}

/// Wait up to `timeout` for input on `fd`.
#[cfg(unix)]
fn wait_on(fd: std::os::fd::RawFd, timeout: Duration) -> io::Result<Wait> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let mut fds = [libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        }];
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        // Rounded up, so the wait never ends before the deadline.
        let millis =
            libc::c_int::try_from(left.as_micros().div_ceil(1_000)).unwrap_or(libc::c_int::MAX);
        // SAFETY: `fds` is one initialised `pollfd`, and the pointer and
        // count describe exactly that array for the duration of the call.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 1, millis) };
        if ready >= 0 {
            return Ok(classify(fds[0].revents));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
        // A signal ended the wait early. The hang-up that closes a
        // controlling terminal arrives as SIGHUP in this very way, so the
        // wait resumes and reports what the descriptor now says rather
        // than letting crossterm read it.
    }
}

/// Wait up to `timeout` for input, through crossterm where `poll(2)` is
/// not available.
#[cfg(not(unix))]
pub(super) fn wait(timeout: Duration) -> io::Result<Wait> {
    Ok(if crossterm::event::poll(timeout)? {
        Wait::Ready
    } else {
        Wait::Idle
    })
}

/// What the returned events of a `poll(2)` on the terminal mean. A hang-up
/// wins over readable bytes: nobody is left to see their effect.
#[cfg(unix)]
fn classify(revents: libc::c_short) -> Wait {
    if revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
        Wait::Closed
    } else if revents & libc::POLLIN != 0 {
        Wait::Ready
    } else {
        Wait::Idle
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::AsRawFd;

    #[test]
    fn readable_input_is_ready() {
        assert_eq!(classify(libc::POLLIN), Wait::Ready);
    }

    #[test]
    fn nothing_reported_is_idle() {
        assert_eq!(classify(0), Wait::Idle);
    }

    #[test]
    fn a_hang_up_closes_even_with_bytes_waiting() {
        assert_eq!(classify(libc::POLLIN | libc::POLLHUP), Wait::Closed);
        assert_eq!(classify(libc::POLLHUP), Wait::Closed);
    }

    #[test]
    fn an_error_or_invalid_descriptor_closes() {
        assert_eq!(classify(libc::POLLERR), Wait::Closed);
        assert_eq!(classify(libc::POLLNVAL), Wait::Closed);
    }

    #[test]
    fn an_empty_pipe_is_idle_until_the_timeout() {
        let (reader, _writer) = io::pipe().expect("a pipe");
        let started = std::time::Instant::now();
        let state = wait_on(reader.as_raw_fd(), Duration::from_millis(30)).expect("polling");
        assert_eq!(state, Wait::Idle);
        // The kernel's timer granularity is the only slack allowed.
        assert!(
            started.elapsed() >= Duration::from_millis(25),
            "the wait returned early"
        );
    }

    #[test]
    fn a_pipe_with_bytes_is_ready_at_once() {
        let (reader, mut writer) = io::pipe().expect("a pipe");
        writer.write_all(b"x").expect("writing");
        let state = wait_on(reader.as_raw_fd(), Duration::from_secs(5)).expect("polling");
        assert_eq!(state, Wait::Ready);
    }

    #[test]
    fn a_pipe_whose_writer_is_gone_is_closed_even_with_bytes_waiting() {
        let (reader, mut writer) = io::pipe().expect("a pipe");
        writer.write_all(b"x").expect("writing");
        drop(writer);
        let state = wait_on(reader.as_raw_fd(), Duration::from_secs(5)).expect("polling");
        assert_eq!(state, Wait::Closed);
    }
}
