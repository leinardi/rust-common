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

//! The YAML file into the untyped tree, through serde-saphyr with spans. Only `true`, `True`,
//! `TRUE`, `false`, `False` and `FALSE` are booleans (`tRue` and `yes` are text), `010` is
//! octal, a duplicate key is an error, and `.nan` and `.inf` reach the typed read, which refuses
//! them with a rule of its own. An empty file, or one holding only comments or a null, is an
//! empty mapping; any other top-level value but a mapping is an error. Errors are one line with
//! their location, without source excerpts or library hints, in `Debug` as in `Display`.
//!
//! Extracted from bitgauge/src/config/raw.rs (the visitor and `from_yaml`) and
//! bitgauge/src/config/load.rs (the file read and the diagnostic) at 9be3b0d20765; the author
//! relicenses them from GPL-3.0-or-later to MIT OR Apache-2.0 for this repository.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use serde_core::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_saphyr::Spanned;

use crate::tree::{Location, Map, Node, Origin, Raw};

/// Reads YAML text into the untyped tree.
///
/// # Errors
///
/// A [`YamlError`] for text that is not valid YAML, holds a duplicate key, more than one document
/// or a mapping key that is not text, or is not a mapping at the top.
pub fn parse(text: &str) -> Result<Map, YamlError> {
    let options = serde_saphyr::options! {
        // Only true and false are booleans; `yes`, `on` and `y` stay text, as yaml.v3 decodes
        // them into an untyped value.
        strict_booleans: true,
        // `010` is 8, as yaml.v3 reads an integer with base prefixes.
        legacy_octal_numbers: true,
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::Error,
        // .nan and .inf reach the typed read, which refuses them with a rule of their own.
        non_finite_float_policy: serde_saphyr::NonFiniteFloatPolicy::PassThrough,
    };
    let root: Option<Parsed> = serde_saphyr::from_str_with_options(text, options)
        .map_err(|error| YamlError(Kind::parse(&error)))?;
    match root.map(|parsed| parsed.0) {
        None | Some(Raw::Null) => Ok(Map::new()),
        Some(Raw::Map(entries)) => Ok(entries),
        Some(
            other @ (Raw::Bool(_)
            | Raw::Int(_)
            | Raw::Wide(_)
            | Raw::Float(_)
            | Raw::Text(_)
            | Raw::List(_)),
        ) => Err(YamlError(Kind::NotAMapping(other.kind()))),
    }
}

/// What to do when the file to read does not exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MissingFile {
    /// An error: the path was given explicitly.
    Refused,
    /// An empty mapping: the path is a default that need not exist.
    Empty,
}

/// Reads the YAML file at `path` into the untyped tree; a missing file is an error or an empty
/// mapping, as `missing` says.
///
/// # Errors
///
/// [`FileError::NotFound`] for a missing file under [`MissingFile::Refused`],
/// [`FileError::Read`] for a file that cannot be read (a directory, no permission, not UTF-8),
/// [`FileError::Yaml`] for one [`parse`] refuses.
pub fn read_file(path: &Path, missing: MissingFile) -> Result<Map, FileError> {
    let text = match std::fs::metadata(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => match missing {
            MissingFile::Refused => {
                return Err(FileError::NotFound {
                    path: path.to_owned(),
                });
            }
            MissingFile::Empty => return Ok(Map::new()),
        },
        Err(source) => {
            return Err(FileError::Read {
                path: path.to_owned(),
                source,
            });
        }
        Ok(_) => std::fs::read_to_string(path).map_err(|source| FileError::Read {
            path: path.to_owned(),
            source,
        })?,
    };
    parse(&text).map_err(|source| FileError::Yaml {
        path: path.to_owned(),
        source,
    })
}

/// A configuration file that cannot be read at all.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FileError {
    /// The file does not exist, and its path was given explicitly.
    #[error("configuration file not found: {}", path.display())]
    NotFound {
        /// The path given.
        path: PathBuf,
    },
    /// Reading the file failed.
    #[error("cannot read the configuration file {}: {source}", path.display())]
    Read {
        /// The path read.
        path: PathBuf,
        /// Why it could not be read.
        source: io::Error,
    },
    /// The file is not a valid YAML mapping.
    #[error("cannot read the configuration file {}: {source}", path.display())]
    Yaml {
        /// The path read.
        path: PathBuf,
        /// What the parser refused.
        source: YamlError,
    },
}

/// YAML text that is not a valid configuration: a syntax error, a duplicate key, a second
/// document, a mapping key that is not text, or a top-level value that is not a mapping. Its
/// `Display` and `Debug` are one line, with the location where the parser gives one, and it has
/// no `source`: serde-saphyr's own error renders an excerpt of the file, which may hold a
/// secret, so only the excerpt-free rendering is kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YamlError(Kind);

#[derive(Debug, Clone, PartialEq, Eq)]
enum Kind {
    Parse {
        message: String,
        location: Option<Location>,
    },
    NotAMapping(&'static str),
}

impl Kind {
    fn parse(error: &serde_saphyr::Error) -> Self {
        // Operator-facing: the location, not a source excerpt or a library API hint, on one line.
        let mut options = serde_saphyr::RenderOptions::new(&serde_saphyr::UserMessageFormatter);
        options.snippets = serde_saphyr::SnippetMode::Off;
        Self::Parse {
            message: error
                .render_with_options(options)
                .escape_debug()
                .to_string(),
            location: error.location().map(|location| Location {
                line: location.line(),
                column: location.column(),
            }),
        }
    }
}

impl YamlError {
    /// Where the parser found the problem, when it says.
    #[must_use]
    pub const fn location(&self) -> Option<Location> {
        match &self.0 {
            Kind::Parse { location, .. } => *location,
            Kind::NotAMapping(_) => None,
        }
    }
}

impl fmt::Display for YamlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Kind::Parse { message, .. } => formatter.write_str(message),
            Kind::NotAMapping(kind) => {
                write!(formatter, "the configuration must be a mapping, not {kind}")
            }
        }
    }
}

impl std::error::Error for YamlError {}

/// A value read from the YAML; a private wrapper, so the public tree carries no serde impl.
struct Parsed(Raw);

impl<'de> Deserialize<'de> for Parsed {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(RawVisitor).map(Parsed)
    }
}

/// A value read with its location.
fn placed(spanned: Spanned<Parsed>) -> Node {
    Node::new(
        spanned.value.0,
        Origin::Yaml(Location {
            line: spanned.defined.line(),
            column: spanned.defined.column(),
        }),
    )
}

struct RawVisitor;

impl<'de> Visitor<'de> for RawVisitor {
    type Value = Raw;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a YAML value")
    }

    fn visit_unit<E: de::Error>(self) -> Result<Raw, E> {
        Ok(Raw::Null)
    }

    fn visit_none<E: de::Error>(self) -> Result<Raw, E> {
        Ok(Raw::Null)
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<Raw, D::Error> {
        Parsed::deserialize(deserializer).map(|parsed| parsed.0)
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Raw, E> {
        Ok(Raw::Bool(value))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Raw, E> {
        Ok(Raw::Int(value))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Raw, E> {
        Ok(i64::try_from(value).map_or(Raw::Wide(i128::from(value)), Raw::Int))
    }

    fn visit_i128<E: de::Error>(self, value: i128) -> Result<Raw, E> {
        Ok(i64::try_from(value).map_or(Raw::Wide(value), Raw::Int))
    }

    fn visit_u128<E: de::Error>(self, value: u128) -> Result<Raw, E> {
        // Beyond i128 it is still only "too large" to every typed read.
        Ok(Raw::Wide(i128::try_from(value).unwrap_or(i128::MAX)))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Raw, E> {
        Ok(Raw::Float(value))
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Raw, E> {
        Ok(Raw::Text(value.to_owned()))
    }

    fn visit_string<E: de::Error>(self, value: String) -> Result<Raw, E> {
        Ok(Raw::Text(value))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut access: A) -> Result<Raw, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = access.next_element::<Spanned<Parsed>>()? {
            items.push(placed(item));
        }
        Ok(Raw::List(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Raw, A::Error> {
        let mut entries = Map::new();
        while let Some(key) = access.next_key::<Parsed>()? {
            let Raw::Text(key) = key.0 else {
                return Err(de::Error::custom(format!(
                    "a mapping key must be text, not {}",
                    key.0.kind()
                )));
            };
            let value = access.next_value::<Spanned<Parsed>>()?;
            entries.insert(key, placed(value));
        }
        Ok(Raw::Map(entries))
    }
}
