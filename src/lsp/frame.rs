//! The Language Server Protocol's framing: `Content-Length`, a blank line, and
//! a JSON payload.
//!
//! Hand-written, like `runtime::wire`, and for the same reason: it is thirty
//! lines and one invariant, where a crate would bring an async runtime and a
//! service abstraction to read a header.
//!
//! The one thing that is not obvious is the header parsing. A server is allowed
//! to send headers we do not know (`Content-Type` is in the specification), the
//! separator is CRLF and not LF, and the length is in **bytes** of the payload
//! — not characters, which matters as soon as a diagnostic message carries an
//! accent.

use std::io::{BufRead, Write};

use anyhow::{bail, Context, Result};

/// Beyond this, the header lied. The largest thing a server sends us is a
/// semantic-token stream or a completion list for a wide scope: a few hundred
/// kilobytes. Sixty-four megabytes is far above anything real and still small
/// enough that a corrupt header cannot make us allocate the machine's memory.
const MAX_FRAME: usize = 64 * 1024 * 1024;

/// Reads one message. `Ok(None)` is a clean end of stream — the server exited —
/// which is not an error: the session turns it into an event of its own.
pub fn read(input: &mut impl BufRead) -> Result<Option<String>> {
    let mut length: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        if input.read_line(&mut line)? == 0 {
            // End of stream between two messages: the server is gone.
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break; // the blank line: the headers are over
        }
        if let Some(value) = header(trimmed, "content-length") {
            length = Some(value.trim().parse().context("content-length")?);
        }
    }
    let Some(length) = length else {
        bail!("a frame without Content-Length");
    };
    if length > MAX_FRAME {
        bail!("a frame of {length} bytes, which is not one");
    }
    let mut payload = vec![0u8; length];
    input.read_exact(&mut payload)?;
    Ok(Some(String::from_utf8(payload)?))
}

/// The header's value, the name being compared without regard to case — the
/// specification says `Content-Length`, servers write it as they please.
fn header<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let (key, value) = line.split_once(':')?;
    key.trim().eq_ignore_ascii_case(name).then_some(value)
}

/// Writes one message and flushes it. The flush is not optional: the payload
/// sits in the pipe's buffer otherwise, and the server waits for a request that
/// has not left.
pub fn write(output: &mut impl Write, payload: &str) -> Result<()> {
    write!(output, "Content-Length: {}\r\n\r\n", payload.len())?;
    output.write_all(payload.as_bytes())?;
    output.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_survives_the_round_trip() {
        let mut buffer = Vec::new();
        write(&mut buffer, r#"{"jsonrpc":"2.0"}"#).unwrap();
        assert_eq!(
            String::from_utf8(buffer.clone()).unwrap(),
            "Content-Length: 17\r\n\r\n{\"jsonrpc\":\"2.0\"}"
        );
        let mut reader = std::io::Cursor::new(buffer);
        assert_eq!(read(&mut reader).unwrap().unwrap(), r#"{"jsonrpc":"2.0"}"#);
    }

    /// The length is in bytes: an accented message read as characters cuts the
    /// frame short and desynchronises everything that follows.
    #[test]
    fn the_length_counts_bytes_and_not_characters() {
        let payload = r#"{"m":"déjà vu"}"#;
        let mut buffer = Vec::new();
        write(&mut buffer, payload).unwrap();
        assert!(String::from_utf8_lossy(&buffer).starts_with("Content-Length: 17\r\n"));
        let mut reader = std::io::Cursor::new(buffer);
        assert_eq!(read(&mut reader).unwrap().unwrap(), payload);
    }

    /// Unknown headers are skipped, and the name is not case-sensitive.
    #[test]
    fn other_headers_are_skipped() {
        let raw = "content-length: 2\r\nContent-Type: application/vscode-jsonrpc\r\n\r\n{}";
        let mut reader = std::io::Cursor::new(raw.as_bytes().to_vec());
        assert_eq!(read(&mut reader).unwrap().unwrap(), "{}");
    }

    /// A server that exits between two messages is not an error here: it is the
    /// session's business to say so, once, as an event.
    #[test]
    fn the_end_of_the_stream_is_not_an_error() {
        let mut reader = std::io::Cursor::new(Vec::new());
        assert!(read(&mut reader).unwrap().is_none());
    }

    #[test]
    fn a_frame_without_a_length_is_refused() {
        let mut reader = std::io::Cursor::new(b"Content-Type: x\r\n\r\n{}".to_vec());
        assert!(read(&mut reader).is_err());
    }
}
