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

//! A `*_file` key's file: the secret it holds, read when the caller asks (never by rewriting
//! keys behind its back). An error names the key and the file's path, never what the file holds.
//!
//! The two trimming rules are Go's: `strings.TrimRight(text, " \t\r\n")` (runhold's
//! registry, notification and audit sink secrets) and `strings.TrimSpace` (its API tokens and
//! the local admin's password hash). A file that is not UTF-8 is refused, where Go kept the bytes.

use std::io;
use std::path::{Path, PathBuf};

/// How the file's content is trimmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SecretTrim {
    /// Spaces, tabs, carriage returns and line feeds at the end, as Go's
    /// `strings.TrimRight(text, " \t\r\n")`.
    TrailingWhitespace,
    /// White space at both ends, as Go's `strings.TrimSpace`.
    Whitespace,
}

/// Whether a file that holds nothing after trimming is accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EmptySecret {
    /// The empty text is returned.
    Allowed,
    /// [`SecretError::Empty`].
    Refused,
}

/// A secret file that cannot be used. No variant holds the file's content.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SecretError {
    /// The file cannot be read.
    #[error("{key}: cannot read the file {}: {source}", path.display())]
    Read {
        /// The key that names the file.
        key: String,
        /// The file's path.
        path: PathBuf,
        /// Why it could not be read.
        source: io::Error,
    },
    /// The file is not UTF-8 text.
    #[error("{key}: the file {} is not UTF-8 text", path.display())]
    NotUtf8 {
        /// The key that names the file.
        key: String,
        /// The file's path.
        path: PathBuf,
    },
    /// The file holds nothing after trimming, under [`EmptySecret::Refused`].
    #[error("{key}: the file {} holds nothing", path.display())]
    Empty {
        /// The key that names the file.
        key: String,
        /// The file's path.
        path: PathBuf,
    },
}

/// Reads the secret in the file at `path`, which the key `key` (a path such as
/// `notify.smtp.password_file`) names, trimmed by `trim`.
///
/// # Errors
///
/// A [`SecretError`] naming `key` and `path` when the file cannot be read, is not UTF-8, or
/// holds nothing and `empty` refuses that.
pub fn read_secret_file(
    key: &str,
    path: &Path,
    trim: SecretTrim,
    empty: EmptySecret,
) -> Result<String, SecretError> {
    let bytes = std::fs::read(path).map_err(|source| SecretError::Read {
        key: key.to_owned(),
        path: path.to_owned(),
        source,
    })?;
    let text = String::from_utf8(bytes).map_err(|_| SecretError::NotUtf8 {
        key: key.to_owned(),
        path: path.to_owned(),
    })?;
    let trimmed = match trim {
        SecretTrim::TrailingWhitespace => text.trim_end_matches([' ', '\t', '\r', '\n']),
        SecretTrim::Whitespace => text.trim(),
    };
    if trimmed.is_empty() && empty == EmptySecret::Refused {
        return Err(SecretError::Empty {
            key: key.to_owned(),
            path: path.to_owned(),
        });
    }
    Ok(trimmed.to_owned())
}
