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

use std::io::{BufRead, Read, Write};

use anyhow::{bail, Context, Result};

/// Beyond this, the header lied. The largest thing a server sends us is a
/// semantic-token stream or a completion list for a wide scope: a few hundred
/// kilobytes. Sixty-four megabytes is far above anything real and still small
/// enough that a corrupt header cannot make us allocate the machine's memory.
const MAX_FRAME: usize = 64 * 1024 * 1024;

/// Beyond this, a header line is not one. A real header is `Content-Length`
/// and a number, or `Content-Type` and a media type: a few dozen bytes. What
/// has no line feed after this many is a server writing its log to stdout —
/// the one mistake that actually happens — and reading it to the end would
/// hold the whole of that log in memory, for a line we then throw away.
const MAX_HEADER_LINE: usize = 4096;

/// Reads one message. `Ok(None)` is a clean end of stream — the server exited —
/// which is not an error: the session turns it into an event of its own.
pub fn read(input: &mut impl BufRead) -> Result<Option<String>> {
    let mut length: Option<usize> = None;
    let mut raw = Vec::new();
    loop {
        raw.clear();
        // Bounded, and read as bytes: `read_line` would grow without end on a
        // line that never ends, and fail outright on one that is not UTF-8.
        let read = input
            .by_ref()
            .take(MAX_HEADER_LINE as u64)
            .read_until(b'\n', &mut raw)?;
        if read == 0 {
            // End of stream between two messages: the server is gone.
            return Ok(None);
        }
        if !raw.ends_with(b"\n") && raw.len() >= MAX_HEADER_LINE {
            bail!("a header line longer than {MAX_HEADER_LINE} bytes");
        }
        let line = String::from_utf8_lossy(&raw);
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
    // **Lossy, not refused.** The frame is intact — its length said where it
    // ends — and what is not UTF-8 in it is one message's text: a diagnostic
    // quoting a Latin-1 file, a path read off a disk as bytes. Refusing it
    // ended the reader, and with it the whole session, for one string the
    // view would have shown with a replacement character.
    Ok(Some(match String::from_utf8(payload) {
        Ok(text) => text,
        Err(e) => {
            log::debug!("a frame that is not UTF-8, read lossily");
            String::from_utf8_lossy(e.as_bytes()).into_owned()
        }
    }))
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

    /// A frame that is not UTF-8 is one bad string, not a dead session: it is
    /// read with a replacement character, and the next frame is still there.
    #[test]
    fn a_payload_that_is_not_utf8_is_read_lossily() {
        let mut raw = b"Content-Length: 11\r\n\r\n{\"m\":\"\xe9t\xe9\"}".to_vec();
        raw.extend_from_slice(b"Content-Length: 2\r\n\r\n{}");
        let mut reader = std::io::Cursor::new(raw);
        assert_eq!(
            read(&mut reader).unwrap().unwrap(),
            "{\"m\":\"\u{fffd}t\u{fffd}\"}"
        );
        assert_eq!(read(&mut reader).unwrap().unwrap(), "{}");
    }

    /// A server logging to stdout sends a line that never ends in a header: it
    /// is refused at the cap rather than held in memory to its end.
    #[test]
    fn a_header_line_without_an_end_is_refused_at_the_cap() {
        let mut reader = std::io::Cursor::new(vec![b'x'; MAX_HEADER_LINE * 4]);
        let error = read(&mut reader).unwrap_err();
        assert!(error.to_string().contains("header line"), "{error}");
        // And it stopped reading there, not at the end of the stream.
        assert_eq!(reader.position() as usize, MAX_HEADER_LINE);
    }

    #[test]
    fn a_frame_without_a_length_is_refused() {
        let mut reader = std::io::Cursor::new(b"Content-Type: x\r\n\r\n{}".to_vec());
        assert!(read(&mut reader).is_err());
    }
}
