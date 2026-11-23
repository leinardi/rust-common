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
//! Unlike `StdCopy`, a decoder refuses a frame whose payload passes the limit it was given, as
//! soon as the header is in and whatever the stream byte names, an engine's error included; an
//! unknown stream byte keeps its own error. The limit is on one payload, not on the buffer: fed by
//! [`Demuxer::read_frame`], or by [`Demuxer::push`] with chunks of at most 32 KiB and drained
//! with [`Demuxer::next_frame`] after each, a multiplexed decoder holds fewer than
//! `8 + max_frame_payload + 32 KiB` bytes. Its capacity can exceed that by `BytesMut`'s amortized
//! growth.
//!
//! Any of these errors ends the reading: the decoder yields nothing after it (a failed read does not, as
//! the reader may be retried). A frame with an empty payload is skipped, as `StdCopy` writes
//! nothing for it. [`Framing::Raw`] reports every chunk as stdout.
//!
//! The reading rules are moby/moby api/pkg/stdcopy/stdcopy.go's at api/v1.56.1 (`StdCopy`); the
//! code is written anew, not extracted.

use std::io;
use std::num::NonZeroUsize;

use bytes::{Buf as _, BufMut as _, Bytes, BytesMut};
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
    /// A header announces a payload past the decoder's limit.
    #[error("output frame of {length} bytes exceeds {max} bytes")]
    FrameTooLarge {
        /// The payload length the header announces.
        length: u32,
        /// The decoder's limit.
        max: usize,
    },
    /// Reading the stream failed.
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// The length of a multiplexed frame's header.
const HEADER: usize = 8;

/// The room [`Demuxer::read_frame`] makes for each read, `StdCopy`'s starting buffer.
pub(crate) const READ_SIZE: usize = 32 * 1024;

/// A decoder fed with the stream's chunks as they come. Drained after each chunk, it holds at most
/// one partial frame and the read-ahead behind it.
#[derive(Debug)]
pub struct Demuxer {
    framing: Framing,
    buffer: BytesMut,
    max_frame_payload: usize,
    ended: bool,
    /// The most bytes held at once, for the tests of the bound.
    #[cfg(test)]
    pub(crate) peak: usize,
}

impl Demuxer {
    /// A decoder for a stream framed as `framing`, refusing a multiplexed frame whose payload
    /// passes `max_frame_payload` bytes. A raw stream has no frames, so the limit does not apply
    /// to it. A client's is [`Limits::output_frame`](crate::Limits::output_frame).
    #[must_use]
    pub fn new(framing: Framing, max_frame_payload: NonZeroUsize) -> Self {
        Self {
            framing,
            buffer: BytesMut::new(),
            max_frame_payload: max_frame_payload.get(),
            ended: false,
            #[cfg(test)]
            peak: 0,
        }
    }

    /// Adds the next chunk of the stream. Ignored once the decoder has ended: after an error, or
    /// after [`read_frame`](Self::read_frame) reached the end of its reader.
    ///
    /// The decoder's bound holds only if the caller drains it with
    /// [`next_frame`](Self::next_frame) after each `push`: one `push` holds its whole chunk until
    /// then, so a caller reading untrusted data feeds it in small pieces.
    pub fn push(&mut self, chunk: &[u8]) {
        if !self.ended {
            self.buffer.extend_from_slice(chunk);
            #[cfg(test)]
            {
                self.peak = self.peak.max(self.buffer.len());
            }
        }
    }

    /// The next whole frame among the chunks pushed, or `None` until more come. What is left at
    /// the end of the stream is a partial frame, which `StdCopy` drops too.
    ///
    /// # Errors
    ///
    /// [`DemuxError::Daemon`], [`DemuxError::Unrecognized`] and [`DemuxError::FrameTooLarge`] as
    /// the module says; after any of them, this returns `Ok(None)` for good.
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
                    let max = self.max_frame_payload;
                    let Some(whole) = usize::try_from(length)
                        .ok()
                        .filter(|payload| *payload <= max)
                        .and_then(|payload| payload.checked_add(HEADER))
                    else {
                        return Err(self.end(DemuxError::FrameTooLarge { length, max }));
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
            // `read_buf` fills all the spare capacity, so only the limit bounds the read-ahead.
            let read = match self.framing {
                Framing::Multiplexed => {
                    reader
                        .read_buf(&mut (&mut self.buffer).limit(READ_SIZE))
                        .await?
                }
                Framing::Raw => reader.read_buf(&mut self.buffer).await?,
            };
            if read == 0 {
                self.ended = true;
                self.buffer = BytesMut::new();
                return Ok(None);
            }
            #[cfg(test)]
            {
                self.peak = self.peak.max(self.buffer.len());
            }
        }
    }

    fn end(&mut self, error: DemuxError) -> DemuxError {
        self.ended = true;
        self.buffer = BytesMut::new();
        error
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAX: NonZeroUsize = NonZeroUsize::new(1024 * 1024).unwrap();

    fn limited(max: usize) -> Demuxer {
        Demuxer::new(Framing::Multiplexed, NonZeroUsize::new(max).unwrap())
    }

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
        let mut demuxer = Demuxer::new(Framing::Multiplexed, MAX);
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
            let mut demuxer = Demuxer::new(Framing::Multiplexed, MAX);
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
            let mut demuxer = Demuxer::new(Framing::Multiplexed, MAX);
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
        let mut demuxer = Demuxer::new(Framing::Multiplexed, MAX);
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
        let mut demuxer = Demuxer::new(Framing::Multiplexed, MAX);
        demuxer.push(&header(7, 1_000));
        let error = demuxer.next_frame().unwrap_err();
        assert!(matches!(error, DemuxError::Unrecognized(7)), "{error:?}");
        assert_eq!(error.to_string(), "unrecognized stream: 7");
        assert!(demuxer.next_frame().unwrap().is_none());
    }

    /// A length near `u32::MAX` is refused from its header, with no payload sent.
    #[test]
    fn a_huge_announced_length_is_refused_at_once() {
        let mut demuxer = limited(1024);
        demuxer.push(&header(1, u32::MAX));
        let error = demuxer.next_frame().unwrap_err();
        assert!(
            matches!(
                error,
                DemuxError::FrameTooLarge {
                    length: u32::MAX,
                    max: 1024
                }
            ),
            "{error:?}"
        );
        assert_eq!(
            error.to_string(),
            "output frame of 4294967295 bytes exceeds 1024 bytes"
        );
        assert!(demuxer.next_frame().unwrap().is_none());
    }

    #[test]
    fn a_payload_at_the_limit_passes_and_one_byte_over_is_refused_from_its_header() {
        let mut demuxer = limited(4);
        demuxer.push(&frame(1, b"abcd"));
        assert_eq!(drain(&mut demuxer), [(Pipe::Stdout, b"abcd".to_vec())]);

        let mut demuxer = limited(4);
        demuxer.push(&header(2, 5));
        let error = demuxer.next_frame().unwrap_err();
        assert!(
            matches!(error, DemuxError::FrameTooLarge { length: 5, max: 4 }),
            "{error:?}"
        );
    }

    #[test]
    fn an_oversized_daemon_error_is_refused_from_its_header() {
        let mut demuxer = limited(4);
        demuxer.push(&header(3, 5));
        let error = demuxer.next_frame().unwrap_err();
        assert!(
            matches!(error, DemuxError::FrameTooLarge { length: 5, max: 4 }),
            "{error:?}"
        );
    }

    #[test]
    fn an_unknown_stream_byte_takes_precedence_over_the_limit() {
        let mut demuxer = limited(4);
        demuxer.push(&header(7, u32::MAX));
        let error = demuxer.next_frame().unwrap_err();
        assert!(matches!(error, DemuxError::Unrecognized(7)), "{error:?}");
    }

    #[test]
    fn many_small_frames_in_one_large_chunk_pass_with_their_bounds() {
        let chunk = frame(1, b"abcd").repeat(10_000);
        let mut demuxer = limited(4);
        demuxer.push(&chunk);
        let frames = drain(&mut demuxer);
        assert_eq!(frames.len(), 10_000);
        assert!(frames.iter().all(|(_, bytes)| bytes == b"abcd"));
    }

    /// A reader that always offers more than it is asked for, cycling through `bytes`.
    struct Flood {
        bytes: Vec<u8>,
        at: usize,
        largest: usize,
    }

    impl AsyncRead for Flood {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            let flood = self.get_mut();
            flood.largest = flood.largest.max(buf.remaining());
            while buf.remaining() > 0 {
                buf.put_slice(&[flood.bytes[flood.at]]);
                flood.at = (flood.at + 1) % flood.bytes.len();
            }
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn the_read_ahead_is_bounded_on_a_multiplexed_stream() {
        let max = 100_000;
        let mut reader = Flood {
            bytes: frame(1, &vec![b'x'; max]),
            at: 0,
            largest: 0,
        };
        let mut demuxer = limited(max);
        for _ in 0..5 {
            let frame = demuxer.read_frame(&mut reader).await.unwrap().unwrap();
            assert_eq!(frame.bytes.len(), max);
        }
        assert!(reader.largest <= READ_SIZE, "{}", reader.largest);
        // Measured after each read, before a frame is taken out: a partial frame and its read-ahead.
        assert!(demuxer.peak > max, "{}", demuxer.peak);
        assert!(demuxer.peak < HEADER + max + READ_SIZE, "{}", demuxer.peak);
    }

    #[tokio::test]
    async fn a_terminal_error_and_the_end_of_the_reader_release_the_buffer() {
        let mut demuxer = limited(4);
        demuxer.push(&[header(1, 5), vec![b'x'; 1024]].concat());
        assert!(demuxer.next_frame().is_err());
        assert_eq!(demuxer.buffer.capacity(), 0);

        let mut demuxer = limited(4);
        demuxer.push(&[header(7, 1), vec![b'x'; 1024]].concat());
        assert!(demuxer.next_frame().is_err());
        assert_eq!(demuxer.buffer.capacity(), 0);

        let mut demuxer = limited(1024);
        let partial = [header(1, 10), b"short".to_vec()].concat();
        let mut reader = partial.as_slice();
        assert!(demuxer.read_frame(&mut reader).await.unwrap().is_none());
        assert_eq!(demuxer.buffer.capacity(), 0);
    }

    /// Raw: every chunk is stdout, as it is, `\x01` first byte and no newline included.
    #[tokio::test]
    async fn raw_chunks_are_stdout_as_they_are() {
        let mut demuxer = Demuxer::new(Framing::Raw, MAX);
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
        let error = Demuxer::new(Framing::Multiplexed, MAX)
            .read_frame(&mut Broken)
            .await
            .unwrap_err();
        assert!(matches!(error, DemuxError::Io(_)), "{error:?}");
    }
}
