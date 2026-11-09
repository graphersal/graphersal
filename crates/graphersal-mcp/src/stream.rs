//! MCP's stdio framing on any byte stream ([`serve_lines`]): one JSON-RPC message per line,
//! UTF-8, no line end inside a message, in both directions.

use std::io::{self, BufRead, Write};
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Condvar, Mutex, PoisonError};

use serde_json::Value;

use crate::connection::Connection;
use crate::host::McpError;
use crate::protocol::{INTERNAL_ERROR, INVALID_REQUEST, PARSE_ERROR, error_body, parse_message};
use crate::stats::Refusal;
use crate::transport::MAX_BODY_BYTES;

/// Most requests of one connection answered at the same time; the next line is read when one
/// of them is done (the client waits on the stream, nothing is refused).
const MAX_IN_FLIGHT: usize = 32;

/// Serves `connection` over newline-delimited JSON-RPC (the MCP stdio transport): reads one
/// message per line from `read` and writes every answer and every message of the server's own
/// (progress, a changed tool list) as one line to `write`, flushed per line. Blocking std I/O on
/// the calling thread, plus a writer thread and one thread per request while it is answered (at
/// most 32 at a time), so a `notifications/cancelled` or a `ping` is read while a long call
/// runs; notifications, responses and `ping` are answered on the reading thread. Answers may
/// come in another order than their requests (JSON-RPC ids pair them).
///
/// A line that is not a JSON-RPC message gets a JSON-RPC error with id `null` (`-32700` for
/// malformed JSON or text that is not UTF-8, `-32600` for a batch or a line over 16 MiB) and the
/// connection goes on; an empty line is skipped. The end of `read` ends the connection: the
/// runs of requests still running are cancelled ([`Connection::close`]), their answers are
/// still written, and the function returns once they are. `Err` ([`McpError::Io`]) is the
/// first read or write failure, or a writer thread that could not start or panicked (after a write failure the remaining answers are dropped; reading goes on to the
/// end of `read`).
///
/// A host that reads a handshake line first (graphersal-memory's daemon) passes the same
/// buffered reader on, so no byte it buffered is lost.
pub fn serve_lines<R: BufRead, W: Write + Send>(
    connection: &Connection,
    mut read: R,
    write: W,
) -> Result<(), McpError> {
    let (sender, lines) = channel::<String>();
    connection.set_outbound(Some(sender.clone()));
    let running = (Mutex::new(0usize), Condvar::new());
    std::thread::scope(|scope| {
        let writer = std::thread::Builder::new()
            .name("mcp-writer".into())
            .spawn_scoped(scope, move || write_lines(write, lines));
        let writer = match writer {
            Ok(writer) => writer,
            Err(err) => {
                connection.set_outbound(None);
                return Err(McpError::io(&err));
            }
        };
        let send = |message: Value| {
            let _ = sender.send(message.to_string());
        };
        let refuse = |code: i64, message: &str| {
            connection
                .mcp_stats()
                .record_refusal(Refusal::InvalidRequest);
            send(error_body(Value::Null, code, message));
        };
        let read_result = loop {
            let bytes = match read_line(&mut read) {
                Ok(Line::End) => break Ok(()),
                Ok(Line::TooLong) => {
                    refuse(
                        INVALID_REQUEST,
                        &format!("the line is longer than {MAX_BODY_BYTES} bytes"),
                    );
                    continue;
                }
                Ok(Line::Text(bytes)) => bytes,
                Err(err) => break Err(err),
            };
            let Ok(text) = String::from_utf8(bytes) else {
                refuse(PARSE_ERROR, "the line is not UTF-8");
                continue;
            };
            if text.trim().is_empty() {
                continue;
            }
            let message = match parse_message(&text) {
                Ok(message) => message,
                Err(outcome) => {
                    for line in connection.outcome_lines(outcome) {
                        let _ = sender.send(line);
                    }
                    continue;
                }
            };
            let request = message.get("id").is_some()
                && message
                    .get("method")
                    .and_then(Value::as_str)
                    .is_some_and(|method| method != "ping");
            if !request {
                for line in connection.handle_value(&message) {
                    let _ = sender.send(line);
                }
                continue;
            }
            let (count, freed) = &running;
            let mut busy = count.lock().unwrap_or_else(PoisonError::into_inner);
            while *busy >= MAX_IN_FLIGHT {
                busy = freed.wait(busy).unwrap_or_else(PoisonError::into_inner);
            }
            *busy += 1;
            drop(busy);
            let id = message.get("id").cloned().unwrap_or(Value::Null);
            let answers = sender.clone();
            let running = &running;
            let spawned = std::thread::Builder::new()
                .name("mcp-request".into())
                .spawn_scoped(scope, move || {
                    for line in connection.handle_value(&message) {
                        let _ = answers.send(line);
                    }
                    done(running);
                });
            if let Err(err) = spawned {
                done(running);
                send(error_body(
                    id,
                    INTERNAL_ERROR,
                    &format!("the server could not start a thread for the request: {err}"),
                ));
            }
        };
        // The client is gone: its running requests are cancelled, their answers still written.
        connection.close();
        drop(sender);
        let written = writer
            .join()
            .unwrap_or_else(|_| Err(io::Error::other("the writer thread panicked")));
        read_result.and(written).map_err(McpError::from)
    })
}

/// A request thread is done.
fn done(running: &(Mutex<usize>, Condvar)) {
    let mut busy = running.0.lock().unwrap_or_else(PoisonError::into_inner);
    *busy = busy.saturating_sub(1);
    drop(busy);
    running.1.notify_one();
}

/// Writes every line until the last sender is gone; the first failure is the result (later
/// lines are dropped).
fn write_lines<W: Write>(mut write: W, lines: Receiver<String>) -> io::Result<()> {
    let mut result = Ok(());
    for line in lines {
        if result.is_ok() {
            result = write
                .write_all(line.as_bytes())
                .and_then(|()| write.write_all(b"\n"))
                .and_then(|()| write.flush());
        }
    }
    result
}

/// One line of the input.
enum Line {
    /// The line's bytes without the line end.
    Text(Vec<u8>),
    /// A line over [`MAX_BODY_BYTES`] (read to its end, not kept).
    TooLong,
    /// The end of the input.
    End,
}

/// Reads one line of at most [`MAX_BODY_BYTES`] bytes (a last line without a line end counts).
fn read_line(read: &mut impl BufRead) -> io::Result<Line> {
    let mut line = Vec::new();
    let mut too_long = false;
    let mut any = false;
    loop {
        let buffer = match read.fill_buf() {
            Ok(buffer) => buffer,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        if buffer.is_empty() {
            return Ok(match (any, too_long) {
                (false, _) => Line::End,
                (true, true) => Line::TooLong,
                (true, false) => Line::Text(line),
            });
        }
        any = true;
        let end = buffer.iter().position(|byte| *byte == b'\n');
        let chunk = &buffer[..end.unwrap_or(buffer.len())];
        if !too_long {
            if line.len() + chunk.len() > MAX_BODY_BYTES {
                too_long = true;
                line = Vec::new();
            } else {
                line.extend_from_slice(chunk);
            }
        }
        let used = end.map_or(buffer.len(), |at| at + 1);
        read.consume(used);
        if end.is_some() {
            return Ok(if too_long {
                Line::TooLong
            } else {
                Line::Text(line)
            });
        }
    }
}
