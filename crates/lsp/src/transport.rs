//! JSON-RPC framing, as the Language Server Protocol defines it.
//!
//! Messages are a `Content-Length` header, a blank line, then that many bytes
//! of JSON:
//!
//! ```text
//! Content-Length: 42\r\n
//! \r\n
//! {"jsonrpc":"2.0", ...}
//! ```
//!
//! Two details bite implementations that skim the specification. The length is
//! in **bytes**, not characters, so a message containing any non-ASCII text is
//! truncated by an implementation that counts characters. And a server may send
//! other headers — `Content-Type` is permitted — so the reader has to skip
//! headers it does not recognise rather than assuming the blank line comes
//! second.

use std::io::{BufRead, Write};

use anyhow::{Context, Result, bail};

/// Largest message accepted, as a guard against a desynchronised stream
/// claiming a preposterous length and being believed.
const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

/// Write one message.
///
/// # Errors
/// If the underlying stream fails.
pub fn write_message(writer: &mut impl Write, payload: &str) -> Result<()> {
    write!(writer, "Content-Length: {}\r\n\r\n", payload.len())?;
    writer.write_all(payload.as_bytes())?;
    writer.flush()?;
    Ok(())
}

/// Read one message, blocking until it arrives.
///
/// Returns `Ok(None)` at a clean end of stream, which is what a server exiting
/// looks like.
///
/// # Errors
/// If the stream fails, or sends something that is not a valid frame.
pub fn read_message(reader: &mut impl BufRead) -> Result<Option<String>> {
    let mut content_length: Option<usize> = None;

    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).context("reading a header")?;
        if read == 0 {
            // End of stream. Mid-header is a truncated message, which is worth
            // distinguishing from a clean exit between messages.
            return if content_length.is_some() {
                bail!("the stream ended part-way through a message header")
            } else {
                Ok(None)
            };
        }

        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break; // blank line: headers are done
        }

        if let Some((name, value)) = trimmed.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = Some(
                value
                    .trim()
                    .parse()
                    .with_context(|| format!("Content-Length is not a number: {value:?}"))?,
            );
        }
        // Any other header is skipped rather than treated as an error.
    }

    let length = content_length.context("a message arrived with no Content-Length")?;
    if length > MAX_MESSAGE_BYTES {
        bail!("a message claimed to be {length} bytes, which is implausible");
    }

    let mut buffer = vec![0u8; length];
    reader
        .read_exact(&mut buffer)
        .context("reading a message body")?;
    String::from_utf8(buffer)
        .context("a message body was not valid UTF-8")
        .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;

    fn frame(payload: &str) -> Vec<u8> {
        let mut out = Vec::new();
        write_message(&mut out, payload).expect("writes");
        out
    }

    fn read_all(bytes: &[u8]) -> Vec<String> {
        let mut reader = BufReader::new(bytes);
        let mut messages = Vec::new();
        while let Some(message) = read_message(&mut reader).expect("reads") {
            messages.push(message);
        }
        messages
    }

    #[test]
    fn a_message_round_trips() {
        let payload = r#"{"jsonrpc":"2.0","method":"initialized"}"#;
        assert_eq!(read_all(&frame(payload)), [payload]);
    }

    #[test]
    fn the_framing_matches_the_specification() {
        let framed = String::from_utf8(frame("{}")).expect("utf-8");
        assert_eq!(framed, "Content-Length: 2\r\n\r\n{}");
    }

    #[test]
    fn several_messages_in_one_stream_are_read_in_order() {
        let mut bytes = frame(r#"{"id":1}"#);
        bytes.extend(frame(r#"{"id":2}"#));
        bytes.extend(frame(r#"{"id":3}"#));

        assert_eq!(
            read_all(&bytes),
            [r#"{"id":1}"#, r#"{"id":2}"#, r#"{"id":3}"#]
        );
    }

    #[test]
    fn the_length_is_counted_in_bytes_not_characters() {
        // The classic bug: a message containing any non-ASCII text is
        // truncated by an implementation that counts characters, because the
        // server sent more bytes than it read.
        let payload = "{\"message\":\"caf\u{e9} na\u{ef}ve \u{1f600}\"}";
        assert!(
            payload.len() > payload.chars().count(),
            "the test payload must actually contain multi-byte characters"
        );

        assert_eq!(read_all(&frame(payload)), [payload]);
    }

    #[test]
    fn unknown_headers_are_skipped() {
        // Content-Type is permitted by the specification, and some servers
        // send it.
        let raw = b"Content-Type: application/vscode-jsonrpc; charset=utf-8\r\n\
                    Content-Length: 2\r\n\
                    \r\n\
                    {}";
        assert_eq!(read_all(raw), ["{}"]);
    }

    #[test]
    fn header_names_are_matched_case_insensitively() {
        let raw = b"content-length: 2\r\n\r\n{}";
        assert_eq!(read_all(raw), ["{}"]);
    }

    #[test]
    fn a_clean_end_of_stream_is_not_an_error() {
        let mut reader = BufReader::new(&b""[..]);
        assert!(read_message(&mut reader).expect("no error").is_none());
    }

    #[test]
    fn a_truncated_header_is_an_error_rather_than_a_clean_exit() {
        // Distinguishing these matters: one means the server exited, the other
        // means it died mid-write and should be restarted.
        let mut reader = BufReader::new(&b"Content-Length: 10\r\n"[..]);
        assert!(read_message(&mut reader).is_err());
    }

    #[test]
    fn a_truncated_body_is_an_error() {
        let mut reader = BufReader::new(&b"Content-Length: 100\r\n\r\n{}"[..]);
        assert!(read_message(&mut reader).is_err());
    }

    #[test]
    fn a_missing_content_length_is_an_error() {
        let mut reader = BufReader::new(&b"Content-Type: x\r\n\r\n{}"[..]);
        assert!(read_message(&mut reader).is_err());
    }

    #[test]
    fn a_nonsense_content_length_is_rejected_rather_than_believed() {
        let mut reader = BufReader::new(&b"Content-Length: banana\r\n\r\n{}"[..]);
        assert!(read_message(&mut reader).is_err());

        // A desynchronised stream can claim a preposterous length; allocating
        // it would be an easy way to exhaust memory.
        let mut reader = BufReader::new(&b"Content-Length: 999999999999\r\n\r\n"[..]);
        assert!(read_message(&mut reader).is_err());
    }

    #[test]
    fn an_empty_message_body_is_handled() {
        let mut reader = BufReader::new(&b"Content-Length: 0\r\n\r\n"[..]);
        assert_eq!(
            read_message(&mut reader).expect("reads"),
            Some(String::new())
        );
    }
}
