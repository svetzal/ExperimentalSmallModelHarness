//! A pseudo-terminal driver that renders output into a screen model.
//!
//! `portable-pty` runs the binary on a real pseudo-terminal (ConPTY on
//! Windows), and `vt100` parses everything it writes into a cell grid, so
//! tests assert on rows, cells, attributes and colour rather than bytes. A
//! reader thread drains the terminal continuously, so the binary never
//! blocks on a full buffer, and keeps the raw bytes and a history of rendered
//! screens for the few tests that need them.
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long a wait lasts before the test fails with the current screen.
pub const WAIT: Duration = Duration::from_secs(20);
/// Most rendered screens kept in the history.
const MAX_FRAMES: usize = 20_000;

struct Shared {
    raw: Vec<u8>,
    parser: vt100::Parser,
    /// The screen's text after each chunk of output.
    frames: Vec<String>,
}

pub struct Tui {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    shared: Arc<Mutex<Shared>>,
    reader: Option<JoinHandle<()>>,
    /// Frames before this index are ignored by `wait_for_frame`.
    frame_cursor: usize,
    #[cfg(unix)]
    initial_modes: Option<[libc::tcflag_t; 3]>,
}

/// Builds a terminal session.
pub struct Launch {
    command: CommandBuilder,
    rows: u16,
    cols: u16,
}

impl Launch {
    pub fn new(binary: &str) -> Self {
        let mut command = CommandBuilder::new(binary);
        command.env("TERM", "xterm-256color");
        // Colour stays on unless a test asks otherwise.
        command.env_remove("NO_COLOR");
        Self {
            command,
            rows: 30,
            cols: 120,
        }
    }

    pub fn arg(mut self, arg: impl AsRef<OsStr>) -> Self {
        self.command.arg(arg);
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.command.args(args);
        self
    }

    pub fn cwd(mut self, directory: &Path) -> Self {
        self.command.cwd(directory);
        self
    }

    pub fn env(mut self, key: &str, value: impl AsRef<OsStr>) -> Self {
        self.command.env(key, value);
        self
    }

    pub fn env_remove(mut self, key: &str) -> Self {
        self.command.env_remove(key);
        self
    }

    pub fn size(mut self, rows: u16, cols: u16) -> Self {
        self.rows = rows;
        self.cols = cols;
        self
    }

    pub fn spawn(self) -> Tui {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: self.rows,
                cols: self.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("opening a pseudo-terminal");
        #[cfg(unix)]
        let initial_modes = modes(pair.master.as_ref());
        let child = pair
            .slave
            .spawn_command(self.command)
            .expect("starting the binary on the pseudo-terminal");
        drop(pair.slave);
        let mut reader = pair.master.try_clone_reader().expect("terminal reader");
        let writer = pair.master.take_writer().expect("terminal writer");
        let shared = Arc::new(Mutex::new(Shared {
            raw: Vec::new(),
            parser: vt100::Parser::new(self.rows, self.cols, 0),
            frames: Vec::new(),
        }));
        let sink = shared.clone();
        let reader = std::thread::spawn(move || {
            let mut buffer = [0u8; 65_536];
            while let Ok(count) = reader.read(&mut buffer) {
                if count == 0 {
                    break;
                }
                let mut shared = sink.lock().expect("terminal state");
                shared.raw.extend_from_slice(&buffer[..count]);
                shared.parser.process(&buffer[..count]);
                let frame = shared.parser.screen().contents();
                if shared.frames.len() < MAX_FRAMES {
                    shared.frames.push(frame);
                }
            }
        });
        Tui {
            master: pair.master,
            writer,
            child,
            shared,
            reader: Some(reader),
            frame_cursor: 0,
            #[cfg(unix)]
            initial_modes,
        }
    }
}

#[cfg(unix)]
fn modes(master: &dyn MasterPty) -> Option<[libc::tcflag_t; 3]> {
    let fd = master.as_raw_fd()?;
    // SAFETY: `termios` is plain data that `tcgetattr` fully initialises on
    // success, and `fd` is the open master descriptor owned by `master`.
    unsafe {
        let mut termios: libc::termios = std::mem::zeroed();
        (libc::tcgetattr(fd, &mut termios) == 0).then_some([
            termios.c_iflag,
            termios.c_oflag,
            termios.c_lflag,
        ])
    }
}

/// A copy of the rendered screen at one moment.
pub struct Snapshot(vt100::Screen);

impl Snapshot {
    /// Row `row`, with trailing blanks removed.
    pub fn row(&self, row: u16) -> String {
        let (_, cols) = self.0.size();
        self.0
            .rows(0, cols)
            .nth(usize::from(row))
            .unwrap_or_default()
            .trim_end()
            .to_owned()
    }

    pub fn rows(&self) -> Vec<String> {
        let (rows, _) = self.0.size();
        (0..rows).map(|row| self.row(row)).collect()
    }

    pub fn text(&self) -> String {
        self.rows().join("\n")
    }

    /// Every row from column `col` to the right edge, with trailing blanks
    /// removed, for a pane that sits beside other text.
    pub fn columns_from(&self, col: u16) -> Vec<String> {
        let (_, cols) = self.0.size();
        self.0
            .rows(col, cols.saturating_sub(col))
            .map(|row| row.trim_end().to_owned())
            .collect()
    }

    pub fn contains(&self, needle: &str) -> bool {
        self.rows().iter().any(|row| row.contains(needle))
    }

    /// The first row that contains `needle`.
    pub fn find(&self, needle: &str) -> Option<u16> {
        self.rows()
            .iter()
            .position(|row| row.contains(needle))
            .map(|row| row as u16)
    }

    /// The first row and column where `needle` starts.
    pub fn locate(&self, needle: &str) -> Option<(u16, u16)> {
        let (rows, cols) = self.0.size();
        for row in 0..rows {
            let mut text = String::new();
            let mut starts = Vec::new();
            for col in 0..cols {
                let cell = self.0.cell(row, col)?;
                if cell.is_wide_continuation() {
                    continue;
                }
                starts.push((text.len(), col));
                text.push_str(if cell.has_contents() {
                    cell.contents()
                } else {
                    " "
                });
            }
            if let Some(byte) = text.find(needle) {
                let col = starts
                    .iter()
                    .rev()
                    .find(|(offset, _)| *offset <= byte)
                    .map_or(0, |(_, col)| *col);
                return Some((row, col));
            }
        }
        None
    }

    pub fn cell(&self, row: u16, col: u16) -> &vt100::Cell {
        self.0.cell(row, col).expect("cell inside the screen")
    }
}

impl std::fmt::Display for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, row) in self.rows().iter().enumerate() {
            writeln!(f, "{index:2}|{row}")?;
        }
        Ok(())
    }
}

/// A named 256-colour index, as crossterm writes ANSI colours.
pub fn ansi(index: u8) -> vt100::Color {
    vt100::Color::Idx(index)
}

impl Tui {
    pub fn snapshot(&self) -> Snapshot {
        Snapshot(
            self.shared
                .lock()
                .expect("terminal state")
                .parser
                .screen()
                .clone(),
        )
    }

    /// Everything the binary has written, escape sequences included.
    pub fn raw(&self) -> Vec<u8> {
        self.shared.lock().expect("terminal state").raw.clone()
    }

    /// Rendered screens recorded since the last `mark_frames`.
    pub fn frames_since_mark(&self) -> Vec<String> {
        let shared = self.shared.lock().expect("terminal state");
        shared.frames[self.frame_cursor.min(shared.frames.len())..].to_vec()
    }

    /// Start a new window for `wait_for_frame` and `frames_since_mark`.
    pub fn mark_frames(&mut self) {
        self.frame_cursor = self.shared.lock().expect("terminal state").frames.len();
    }

    /// Wait until the current screen satisfies `test`.
    pub fn wait_for(&self, what: &str, test: impl Fn(&Snapshot) -> bool) -> Snapshot {
        let deadline = Instant::now() + WAIT;
        loop {
            let snapshot = self.snapshot();
            if test(&snapshot) {
                return snapshot;
            }
            if Instant::now() > deadline {
                panic!("timed out waiting for {what}; screen:\n{snapshot}");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Wait until the screen shows `needle`.
    pub fn wait_text(&self, needle: &str) -> Snapshot {
        self.wait_for(&format!("{needle:?}"), |screen| screen.contains(needle))
    }

    /// Wait until some screen rendered since the last mark satisfies `test`,
    /// so a transient state is not missed between polls. Returns that frame.
    pub fn wait_for_frame(&self, what: &str, test: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(frame) = self
                .frames_since_mark()
                .into_iter()
                .find(|frame| test(frame))
            {
                return frame;
            }
            if Instant::now() > deadline {
                panic!(
                    "timed out waiting for a frame with {what}; screen:\n{}",
                    self.snapshot()
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn send(&mut self, bytes: &[u8]) {
        self.writer
            .write_all(bytes)
            .expect("writing to the terminal");
        self.writer.flush().expect("flushing the terminal");
    }

    /// Bracketed paste, as a terminal sends pasted text.
    pub fn paste(&mut self, text: &str) {
        self.send(format!("\x1b[200~{text}\x1b[201~").as_bytes());
    }

    /// Paste a line and press Enter.
    pub fn submit(&mut self, text: &str) {
        self.paste(text);
        self.send(b"\r");
    }

    pub fn esc(&mut self) {
        self.send(b"\x1b");
    }

    pub fn ctrl_c(&mut self) {
        self.send(b"\x03");
    }

    pub fn down(&mut self) {
        self.send(b"\x1b[B");
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("resizing the terminal");
        self.shared
            .lock()
            .expect("terminal state")
            .parser
            .screen_mut()
            .set_size(rows, cols);
    }

    /// Wait for the binary to exit, and return whether it succeeded.
    pub fn wait_exit(&mut self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().expect("polling the binary") {
                return status.success();
            }
            if Instant::now() > deadline {
                panic!("the binary did not exit; screen:\n{}", self.snapshot());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Press Esc on an idle screen and require a clean exit that restores
    /// the terminal's modes.
    pub fn quit(&mut self) {
        self.esc();
        assert!(self.wait_exit(), "the binary exited with an error");
        self.assert_terminal_restored();
    }

    /// The terminal's input, output and local modes match those before launch.
    pub fn assert_terminal_restored(&self) {
        #[cfg(unix)]
        {
            assert!(
                self.initial_modes.is_some(),
                "could not read the terminal modes"
            );
            assert_eq!(
                modes(self.master.as_ref()),
                self.initial_modes,
                "raw terminal mode was not restored"
            );
        }
    }

    /// The binary has put the terminal in raw mode, so the restore check
    /// has something to compare.
    pub fn assert_raw_mode_active(&self) {
        #[cfg(unix)]
        assert_ne!(
            modes(self.master.as_ref()),
            self.initial_modes,
            "the terminal is not in raw mode"
        );
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        // The reader ends at end of file once the child and the writer are gone.
        drop(self.reader.take());
    }
}
