//! Every byte the binary prints goes through here, never through `print!`/`eprintln!` (the
//! crate denies `clippy::print_stdout`/`clippy::print_stderr`): those macros panic when the
//! reading end of the pipe is gone (`graphersal -e '..' | head -1`, `graphersal --server 2>&1 |
//! head`, a REPL whose terminal went away), since Rust ignores SIGPIPE and every write then
//! fails with `EPIPE`.
//!
//! - stderr (`errln!`) is best effort: a failed write is ignored, and after a broken pipe
//!   nothing more is tried. The dev server keeps serving without its log reader.
//! - stdout (`out!`, `outln!`) is the result: the first failed write is remembered and
//!   every later write is dropped. A one-shot run stops at the next script boundary, the REPL
//!   ends, and [`exit_code`] turns a broken pipe into [`EXIT_CLOSED_PIPE`] (any other write
//!   error into exit code 1, reported once on stderr). The process never resets SIGPIPE to its
//!   default action: that would kill the dev server with its log reader.

use std::fmt;
use std::io::{self, Write as _};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// Exit code when stdout was closed before every result was written (128 + SIGPIPE, as a
/// shell reports a process killed by a closed pipe). A failing script keeps its own code.
pub const EXIT_CLOSED_PIPE: i32 = 141;

/// stdout accepts writes.
const OPEN: u8 = 0;
/// The reader of stdout went away (`BrokenPipe`).
const CLOSED: u8 = 1;
/// Another write error (a full disk, ...), reported on stderr.
const FAILED: u8 = 2;

static STDOUT: AtomicU8 = AtomicU8::new(OPEN);
static STDERR_CLOSED: AtomicBool = AtomicBool::new(false);

/// Prints to stdout without a newline; see the module docs.
macro_rules! out {
    ($($arg:tt)*) => {
        $crate::out::write_stdout(format_args!($($arg)*))
    };
}

/// Prints a line to stdout; see the module docs.
macro_rules! outln {
    () => {
        $crate::out::write_stdout(format_args!("\n"))
    };
    ($($arg:tt)*) => {
        $crate::out::write_stdout(format_args!("{}\n", format_args!($($arg)*)))
    };
}

/// Prints a line to stderr, best effort; see the module docs.
macro_rules! errln {
    ($($arg:tt)*) => {
        $crate::out::write_stderr(format_args!("{}\n", format_args!($($arg)*)))
    };
}

/// Writes `args` to stdout and flushes, unless an earlier write failed. A failure is
/// remembered for [`stdout_open`] and [`exit_code`].
pub fn write_stdout(args: fmt::Arguments<'_>) {
    if STDOUT.load(Ordering::Relaxed) != OPEN {
        return;
    }
    let mut stdout = io::stdout().lock();
    let written = stdout.write_fmt(args).and_then(|()| stdout.flush());
    if let Err(err) = written {
        if err.kind() == io::ErrorKind::BrokenPipe {
            STDOUT.store(CLOSED, Ordering::Relaxed);
        } else if STDOUT.swap(FAILED, Ordering::Relaxed) == OPEN {
            write_stderr(format_args!("Error: cannot write to stdout: {err}\n"));
        }
    }
}

/// Writes `args` to stderr, ignoring a failure; after a broken pipe nothing more is tried.
pub fn write_stderr(args: fmt::Arguments<'_>) {
    if STDERR_CLOSED.load(Ordering::Relaxed) {
        return;
    }
    if let Err(err) = io::stderr().lock().write_fmt(args)
        && err.kind() == io::ErrorKind::BrokenPipe
    {
        STDERR_CLOSED.store(true, Ordering::Relaxed);
    }
}

/// Whether stdout still takes output (no write to it has failed yet).
pub fn stdout_open() -> bool {
    STDOUT.load(Ordering::Relaxed) == OPEN
}

/// The process exit code for a run that ended with `code`: a run that otherwise succeeded
/// (`code` 0) ends with [`EXIT_CLOSED_PIPE`] when stdout was closed and with 1 when another
/// stdout write failed; any other `code` stays.
pub fn exit_code(code: i32) -> i32 {
    if code != 0 {
        return code;
    }
    match STDOUT.load(Ordering::Relaxed) {
        CLOSED => EXIT_CLOSED_PIPE,
        FAILED => 1,
        _ => code,
    }
}
