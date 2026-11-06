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

//! What the implementation identity checks share: an image or binary under test must be the
//! implementation the suite was asked to test, never a Go build standing in for the Rust one or
//! the reverse. Go's `flag` package takes `-help`, prints its usage and exits 0; the Rust ports
//! refuse every single-dash long flag with exit 2 and a refusal of their own. So the answer to
//! [`IDENTITY_FLAG`] tells the two apart, matched whole by an [`Answer`], and anything else fails
//! the run: a mismatch is never a skip. Also here, since it needs no daemon either: whether an ELF
//! binary is static ([`is_static_elf`]).
//!
//! The answers, the error types and their wording stay the suites' own:
//!
//! - swarm-device-access: Rust is exit 2, [`Text::Empty`] stdout, [`Text::Exact`] its refusal on
//!   stderr; Go is exit 0, [`Text::Empty`] stdout, [`Text::Prefix`] its usage on stderr; its
//!   messages quote with [`Quoting::Go`].
//! - bitgauge: Rust is exit 2, [`Text::Empty`] stdout, [`Text::Exact`] its refusal and usage on
//!   stderr; Go is exit 0, [`Text::Exact`] its usage on stdout, [`Text::Prefix`] the flag
//!   defaults on stderr; its messages quote with [`Quoting::Debug`].
//!
//! Extracted from swarm-device-access/testenv/src/identity.rs at 8b4ab3a3e417 (`is_static_elf`,
//! `excerpt`, the identity flag) and bitgauge/testenv/src/identity.rs at 600fc3905200 (`excerpt`
//! with Rust's quoting, the Go answer's stdout); the suites' answers, `check_identity`,
//! `check_go_identity`, `check_help` and the image checks stay in the suites.

use go_compat::quote;

use crate::Error;

/// The argument both checks run the binary with.
pub const IDENTITY_FLAG: &str = "-help";

/// What one stream of an answer must hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Text {
    /// Nothing.
    Empty,
    /// Exactly this, byte for byte.
    Exact(&'static str),
    /// This, then anything.
    Prefix(&'static str),
}

impl Text {
    /// Whether `text` is what this asks for.
    #[must_use]
    pub fn matches(self, text: &str) -> bool {
        match self {
            Self::Empty => text.is_empty(),
            Self::Exact(want) => text == want,
            Self::Prefix(want) => text.starts_with(want),
        }
    }
}

/// An implementation's whole answer to [`IDENTITY_FLAG`]: its exit code and both streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Answer {
    /// The exit code.
    pub exit_code: i64,
    /// What stdout holds.
    pub stdout: Text,
    /// What stderr holds.
    pub stderr: Text,
}

impl Answer {
    /// Whether a process that exited with `exit_code` (`None`: killed by a signal) and wrote
    /// `stdout` and `stderr` gave this answer. Every part counts.
    #[must_use]
    pub fn matches(&self, exit_code: Option<i64>, stdout: &str, stderr: &str) -> bool {
        exit_code == Some(self.exit_code)
            && self.stdout.matches(stdout)
            && self.stderr.matches(stderr)
    }
}

/// How [`excerpt`] quotes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quoting {
    /// Go's `strconv.Quote` (swarm-device-access).
    Go,
    /// Rust's `{:?}` (bitgauge).
    Debug,
}

/// The first line of `text`, quoted, with the byte count when there is more:
/// `"<first line>" (first line of <n> bytes)`.
#[must_use]
pub fn excerpt(text: &str, quoting: Quoting) -> String {
    let quoted = |text: &str| match quoting {
        Quoting::Go => quote::quote(text),
        Quoting::Debug => format!("{text:?}"),
    };
    let first = text.lines().next().unwrap_or_default();
    if first.len() + 1 >= text.len() {
        return quoted(text);
    }
    format!("{} (first line of {} bytes)", quoted(first), text.len())
}

/// The ELF program header type of the dynamic loader's path: a binary that has one needs a loader
/// (and its libraries) that a static runtime image does not have.
const PT_INTERP: u32 = 3;

/// Whether the ELF binary `bytes` is static: it has no `PT_INTERP` program header, as a static or
/// static-pie binary has none.
///
/// # Errors
///
/// `bytes` is not a 64-bit little-endian ELF file (the two host architectures the suite builds
/// for), or its program headers lie outside it.
pub fn is_static_elf(bytes: &[u8]) -> Result<bool, Error> {
    let not_elf = || Error::new("not a 64-bit little-endian ELF file");
    if bytes.get(..4) != Some(b"\x7fELF".as_slice())
        || bytes.get(4) != Some(&2)
        || bytes.get(5) != Some(&1)
    {
        return Err(not_elf());
    }
    let table = read_u64(bytes, 0x20).ok_or_else(not_elf)?;
    let entry_size = read_u16(bytes, 0x36).ok_or_else(not_elf)?;
    let entries = read_u16(bytes, 0x38).ok_or_else(not_elf)?;
    let table = usize::try_from(table).map_err(|_| not_elf())?;
    for index in 0..usize::from(entries) {
        let offset = index
            .checked_mul(usize::from(entry_size))
            .and_then(|relative| relative.checked_add(table))
            .ok_or_else(not_elf)?;
        let kind = read_u32(bytes, offset)
            .ok_or_else(|| Error::new(format!("program header {index} lies outside the file")))?;
        if kind == PT_INTERP {
            return Ok(false);
        }
    }
    Ok(true)
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let end = offset.checked_add(2)?;
    bytes
        .get(offset..end)?
        .try_into()
        .ok()
        .map(u16::from_le_bytes)
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    bytes
        .get(offset..end)?
        .try_into()
        .ok()
        .map(u32::from_le_bytes)
}

fn read_u64(bytes: &[u8], offset: usize) -> Option<u64> {
    let end = offset.checked_add(8)?;
    bytes
        .get(offset..end)?
        .try_into()
        .ok()
        .map(u64::from_le_bytes)
}

#[cfg(test)]
mod tests;
