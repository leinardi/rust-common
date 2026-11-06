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

//! An I/O wrapper that closes a connection whose peer stopped taking what is written to it.
//!
//! The deadline is armed when a write returns `Pending` (the socket's send buffer is full) and
//! disarmed by the next write that returns, so it measures how long a write has waited without a
//! byte leaving, not how long a response takes. It sits under hyper, so it covers a body that
//! streams after the handler returned, and the writes of an upgraded connection too.
//!
//! Extracted from bitgauge/src/server/timeouts.rs at d841694700a5 (the write deadline of
//! `ConnIo`), without its request-phase tracking.

use std::io::{self, IoSlice};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::{Instant, Sleep, sleep_until};

/// `T`, with an error of kind `TimedOut` for a write that stays pending for `timeout`.
#[derive(Debug)]
pub(crate) struct StallIo<T> {
    inner: T,
    timeout: Option<Duration>,
    /// The deadline of the write that is pending now, if one is.
    deadline: Option<Pin<Box<Sleep>>>,
}

impl<T> StallIo<T> {
    pub(crate) fn new(inner: T, timeout: Option<Duration>) -> Self {
        Self {
            inner,
            timeout,
            deadline: None,
        }
    }

    /// A write, flush or shutdown just returned `Pending`: arms the deadline if it is not armed,
    /// and fails once it has passed. Polling the sleep registers its waker, so the task wakes at
    /// the deadline even when the peer sends nothing.
    fn stalled<R>(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<R>> {
        let Some(timeout) = self.timeout else {
            return Poll::Pending;
        };
        let deadline = self
            .deadline
            .get_or_insert_with(|| Box::pin(sleep_until(Instant::now() + timeout)));
        match deadline.as_mut().poll(cx) {
            Poll::Ready(()) => {
                self.deadline = None;
                Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the peer stopped reading",
                )))
            }
            Poll::Pending => Poll::Pending,
        }
    }

    /// The inner call returned. Pending: the deadline runs. Ready, with progress or an error: the
    /// pending write is over.
    fn settle<R>(
        &mut self,
        cx: &mut Context<'_>,
        result: Poll<io::Result<R>>,
    ) -> Poll<io::Result<R>> {
        if result.is_pending() {
            self.stalled(cx)
        } else {
            self.deadline = None;
            result
        }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for StallIo<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for StallIo<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let result = Pin::new(&mut self.inner).poll_write(cx, buf);
        self.settle(cx, result)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let result = Pin::new(&mut self.inner).poll_write_vectored(cx, bufs);
        self.settle(cx, result)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let result = Pin::new(&mut self.inner).poll_flush(cx);
        self.settle(cx, result)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let result = Pin::new(&mut self.inner).poll_shutdown(cx);
        self.settle(cx, result)
    }
}
