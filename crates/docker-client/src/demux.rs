/*
 * Copyright (c) 2026 Roberto Leinardi
 *
 * Licensed under either of
 *
 *  * Apache License, Version 2.0 (LICENSE-APACHE or https://www.apache.org/licenses/LICENSE-2.0)
 *  * MIT license (LICENSE-MIT or https://opensource.org/licenses/MIT)
 *
 * at your option.
 *
 * SPDX-License-Identifier: MIT OR Apache-2.0
 */

//! An engine's output stream split into stdout and stderr, with the framing the caller knows it
//! has.
//!
//! The engine multiplexes a stream that has no TTY: each piece is an 8-byte header (the stream's
//! byte, three zero bytes, the payload's length as a big-endian `u32`) and the payload. A TTY
//! stream is raw bytes. Only the caller knows which it reads, from the TTY flag it set (moby's Go
//! client decides it the same way); bollard guesses from the first byte of each chunk, which a TTY
//! session or a log line starting with `\x01` defeats. So the framing here is a parameter, never
//! a guess.
//!
//! [`Framing::Multiplexed`] reads as moby's `stdcopy.StdCopy` does:
//!
//! * a header or a payload cut short by the end of the stream ends it cleanly, the partial frame
//!   dropped;
//! * stream byte 0 (stdin) is reported as stdout, 1 is stdout, 2 stderr;
//! * stream byte 3 is an error from the engine, `error from daemon in stream: <payload>`, once the
//!   whole payload is in;
//! * any other byte is `unrecognized stream: <byte>`, as soon as the header is in.
//!
//! Either error ends the reading: the decoder yields nothing after it (a failed read does not, as
//! the reader may be retried). A frame with an empty payload is skipped, as `StdCopy` writes
//! nothing for it. [`Framing::Raw`] reports every chunk as stdout.
//!
//! The reading rules are moby/moby api/pkg/stdcopy/stdcopy.go's at api/v1.56.1 (`StdCopy`); the
//! code is written anew, not extracted.

use std::io;

use bytes::{Buf as _, Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt as _};

/// How the stream to read is framed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Framing {
    /// Headers and payloads, as the engine sends a stream that has no TTY.
    Multiplexed,
    /// Raw bytes, as the engine sends a TTY stream; all of it is stdout.
    Raw,
}

/// The stream a [`Frame`] was written to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pipe {
    /// Standard output (and standard input, which the engine never sends in practice).
    Stdout,
    /// Standard error.
    Stderr,
}

/// A piece of output: its bytes as the engine framed them, never split or joined across frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Where the bytes were written.
    pub pipe: Pipe,
    /// The bytes, never empty.
    pub bytes: Bytes,
}

/// A multiplexed stream that cannot be read on.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DemuxError {
    /// The engine sent an error in the stream (stream byte 3); the payload is its text, invalid
    /// UTF-8 replaced.
    #[error("error from daemon in stream: {0}")]
    Daemon(String),
    /// A header names a stream byte other than 0 to 3.
    #[error("unrecognized stream: {0}")]
    Unrecognized(u8),
    /// Reading the stream failed.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// The length of a multiplexed frame's header.
const HEADER: usize = 8;

/// The room [`Demuxer::read_frame`] makes for each read, `StdCopy`'s starting buffer.
const READ_SIZE: usize = 32 * 1024;

/// A decoder fed with the stream's chunks as they come. It holds at most one partial frame.
#[derive(Debug)]
pub struct Demuxer {
    framing: Framing,
    buffer: BytesMut,
    ended: bool,
}

impl Demuxer {
    /// A decoder for a stream framed as `framing`.
    #[must_use]
    pub fn new(framing: Framing) -> Self {
        Self {
            framing,
            buffer: BytesMut::new(),
            ended: false,
        }
    }

    /// Adds the next chunk of the stream. Ignored once the decoder has ended: after an error, or
    /// after [`read_frame`](Self::read_frame) reached the end of its reader.
    pub fn push(&mut self, chunk: &[u8]) {
        if !self.ended {
            self.buffer.extend_from_slice(chunk);
        }
    }

    /// The next whole frame among the chunks pushed, or `None` until more come. What is left at
    /// the end of the stream is a partial frame, which `StdCopy` drops too.
    ///
    /// # Errors
    ///
    /// [`DemuxError::Daemon`] and [`DemuxError::Unrecognized`] as the module says; after either,
    /// this returns `Ok(None)` for good.
    pub fn next_frame(&mut self) -> Result<Option<Frame>, DemuxError> {
        loop {
            if self.ended {
                return Ok(None);
            }
            let frame = match self.framing {
                Framing::Raw => {
                    if self.buffer.is_empty() {
                        return Ok(None);
                    }
                    Frame {
                        pipe: Pipe::Stdout,
                        bytes: self.buffer.split().freeze(),
                    }
                }
                Framing::Multiplexed => {
                    let Some(header) = self.buffer.get(..HEADER) else {
                        return Ok(None);
                    };
                    let pipe = match header[0] {
                        0 | 1 => Some(Pipe::Stdout),
                        2 => Some(Pipe::Stderr),
                        3 => None,
                        other => return Err(self.end(DemuxError::Unrecognized(other))),
                    };
                    let length = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
                    // A length past the address space cannot be buffered whole: it stays partial,
                    // as an allocation failure would leave it in Go.
                    let Some(whole) = usize::try_from(length)
                        .ok()
                        .and_then(|length| length.checked_add(HEADER))
                    else {
                        return Ok(None);
                    };
                    if self.buffer.len() < whole {
                        return Ok(None);
                    }
                    self.buffer.advance(HEADER);
                    let bytes = self.buffer.split_to(whole - HEADER).freeze();
                    let Some(pipe) = pipe else {
                        let text = String::from_utf8_lossy(&bytes).into_owned();
                        return Err(self.end(DemuxError::Daemon(text)));
                    };
                    Frame { pipe, bytes }
                }
            };
            if !frame.bytes.is_empty() {
                return Ok(Some(frame));
            }
        }
    }

    /// Reads `reader` until the next whole frame, or its end.
    ///
    /// # Errors
    ///
    /// As [`next_frame`](Self::next_frame), and [`DemuxError::Io`] for a failed read.
    pub async fn read_frame<R>(&mut self, reader: &mut R) -> Result<Option<Frame>, DemuxError>
    where
        R: AsyncRead + Unpin + ?Sized,
    {
        loop {
            if let Some(frame) = self.next_frame()? {
                return Ok(Some(frame));
            }
            if self.ended {
                return Ok(None);
            }
            self.buffer.reserve(READ_SIZE);
            if reader.read_buf(&mut self.buffer).await? == 0 {
                self.ended = true;
                self.buffer.clear();
                return Ok(None);
            }
        }
    }

    fn end(&mut self, error: DemuxError) -> DemuxError {
        self.ended = true;
        self.buffer.clear();
        error
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(stream: u8, length: u32) -> Vec<u8> {
        let mut header = vec![stream, 0, 0, 0];
        header.extend_from_slice(&length.to_be_bytes());
        header
    }

    fn frame(stream: u8, payload: &[u8]) -> Vec<u8> {
        let mut bytes = header(stream, u32::try_from(payload.len()).unwrap());
        bytes.extend_from_slice(payload);
        bytes
    }

    fn drain(demuxer: &mut Demuxer) -> Vec<(Pipe, Vec<u8>)> {
        let mut frames = Vec::new();
        while let Some(frame) = demuxer.next_frame().unwrap() {
            frames.push((frame.pipe, frame.bytes.to_vec()));
        }
        frames
    }

    /// Stdin (0) and stdout (1) are stdout, 2 is stderr, an empty payload is skipped, and frames
    /// keep their bounds.
    #[test]
    fn multiplexed_frames_are_sorted_by_their_stream_byte() {
        let mut demuxer = Demuxer::new(Framing::Multiplexed);
        demuxer.push(
            &[
                frame(1, b"out\n"),
                frame(2, b"err"),
                frame(0, b"in"),
                frame(1, b""),
                frame(1, b"\x01raw"),
            ]
            .concat(),
        );
        assert_eq!(
            drain(&mut demuxer),
            [
                (Pipe::Stdout, b"out\n".to_vec()),
                (Pipe::Stderr, b"err".to_vec()),
                (Pipe::Stdout, b"in".to_vec()),
                (Pipe::Stdout, b"\x01raw".to_vec()),
            ]
        );
    }

    /// A frame split anywhere, header included, comes out whole once its last byte is in.
    #[test]
    fn a_frame_split_across_chunks_comes_out_whole() {
        let bytes = [frame(2, b"hello"), frame(1, b"world")].concat();
        for cut in 1..bytes.len() {
            let mut demuxer = Demuxer::new(Framing::Multiplexed);
            demuxer.push(&bytes[..cut]);
            let mut frames = drain(&mut demuxer);
            demuxer.push(&bytes[cut..]);
            frames.extend(drain(&mut demuxer));
            assert_eq!(
                frames,
                [
                    (Pipe::Stderr, b"hello".to_vec()),
                    (Pipe::Stdout, b"world".to_vec())
                ],
                "cut at {cut}"
            );
        }
    }

    /// `StdCopy`: a header or a payload cut short by the end of the stream ends it cleanly.
    #[tokio::test]
    async fn a_partial_frame_at_the_end_is_dropped_without_an_error() {
        for tail in [
            &b"\x01\x00\x00"[..],
            &[header(1, 10), b"short".to_vec()].concat(),
        ] {
            let bytes = [frame(1, b"whole"), tail.to_vec()].concat();
            let mut reader = bytes.as_slice();
            let mut demuxer = Demuxer::new(Framing::Multiplexed);
            let first = demuxer.read_frame(&mut reader).await.unwrap().unwrap();
            assert_eq!(first.bytes.as_ref(), b"whole");
            assert!(demuxer.read_frame(&mut reader).await.unwrap().is_none());
            assert!(demuxer.read_frame(&mut reader).await.unwrap().is_none());
        }
    }

    /// `StdCopy`: stream byte 3 is the engine's error, once its whole payload is in; nothing is
    /// read after it.
    #[test]
    fn stream_byte_three_is_the_engines_error_after_its_payload() {
        let error = frame(3, b"oci runtime failed");
        let mut demuxer = Demuxer::new(Framing::Multiplexed);
        demuxer.push(&error[..error.len() - 1]);
        assert!(demuxer.next_frame().unwrap().is_none());
        demuxer.push(&[&error[error.len() - 1..], &frame(1, b"after")[..]].concat());
        let error = demuxer.next_frame().unwrap_err();
        assert!(matches!(error, DemuxError::Daemon(_)), "{error:?}");
        assert_eq!(
            error.to_string(),
            "error from daemon in stream: oci runtime failed"
        );
        demuxer.push(&frame(1, b"later"));
        assert!(demuxer.next_frame().unwrap().is_none());
    }

    /// `StdCopy`: an unknown stream byte is an error as soon as the header is in, before its
    /// payload.
    #[test]
    fn an_unknown_stream_byte_is_an_error_at_its_header() {
        let mut demuxer = Demuxer::new(Framing::Multiplexed);
        demuxer.push(&header(7, 1_000));
        let error = demuxer.next_frame().unwrap_err();
        assert!(matches!(error, DemuxError::Unrecognized(7)), "{error:?}");
        assert_eq!(error.to_string(), "unrecognized stream: 7");
        assert!(demuxer.next_frame().unwrap().is_none());
    }

    /// A length near `u32::MAX` waits for its payload; nothing panics or allocates it up front.
    #[test]
    fn a_huge_announced_length_waits_for_its_payload() {
        let mut demuxer = Demuxer::new(Framing::Multiplexed);
        demuxer.push(&header(1, u32::MAX));
        demuxer.push(b"some");
        assert!(demuxer.next_frame().unwrap().is_none());
    }

    /// Raw: every chunk is stdout, as it is, `\x01` first byte and no newline included.
    #[tokio::test]
    async fn raw_chunks_are_stdout_as_they_are() {
        let mut demuxer = Demuxer::new(Framing::Raw);
        demuxer.push(b"\x01\x00\x00\x00\x00\x00\x00\x05prompt $ ");
        assert_eq!(
            drain(&mut demuxer),
            [(
                Pipe::Stdout,
                b"\x01\x00\x00\x00\x00\x00\x00\x05prompt $ ".to_vec()
            )]
        );
        let mut reader: &[u8] = b"\x02abc";
        let frame = demuxer.read_frame(&mut reader).await.unwrap().unwrap();
        assert_eq!(
            (frame.pipe, frame.bytes.as_ref()),
            (Pipe::Stdout, &b"\x02abc"[..])
        );
        assert!(demuxer.read_frame(&mut reader).await.unwrap().is_none());
    }

    /// A failed read is an I/O error.
    #[tokio::test]
    async fn a_failed_read_is_an_io_error() {
        struct Broken;
        impl AsyncRead for Broken {
            fn poll_read(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
                _: &mut tokio::io::ReadBuf<'_>,
            ) -> std::task::Poll<io::Result<()>> {
                std::task::Poll::Ready(Err(io::ErrorKind::ConnectionReset.into()))
            }
        }
        let error = Demuxer::new(Framing::Multiplexed)
            .read_frame(&mut Broken)
            .await
            .unwrap_err();
        assert!(matches!(error, DemuxError::Io(_)), "{error:?}");
    }
}
