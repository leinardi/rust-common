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

//! Problems: a key path, where its value came from, and what is wrong with it. A loader collects
//! every problem instead of stopping at the first, sorts them by path and reports them all.
//!
//! The `Display` is Rust wording; the rule texts in a problem's message are the caller's, and so
//! is logging them.
//!
//! Extracted from bitgauge/src/config/mod.rs and bitgauge/src/config/load.rs at 9be3b0d20765
//! (`Problem`, the stable sort by key), with the location widened into a source; the author
//! relicenses them from GPL-3.0-or-later to MIT OR Apache-2.0 for this repository.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::tree::{Location, Node, Origin};

/// Where a problem's value came from. No variant holds a location the input did not give.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Source {
    /// The YAML file, at a value's location.
    Yaml {
        /// The file.
        file: PathBuf,
        /// Where in it.
        location: Location,
    },
    /// The YAML file, with no location: a problem with the file as a whole.
    YamlFile {
        /// The file.
        file: PathBuf,
    },
    /// An environment variable.
    Environment {
        /// The variable's name.
        variable: String,
    },
    /// The file a `*_file` key names.
    SecretFile {
        /// The key, such as `notify.smtp.password_file`.
        key: String,
        /// The file's path.
        path: PathBuf,
    },
    /// Nowhere: a required key that is not set.
    Missing,
}

impl Source {
    /// Where `node` came from, `file` being the YAML file the loader read.
    #[must_use]
    pub fn of(node: &Node, file: &Path) -> Self {
        match &node.origin {
            Origin::Yaml(location) => Self::Yaml {
                file: file.to_owned(),
                location: *location,
            },
            Origin::Environment(variable) => Self::Environment {
                variable: variable.clone(),
            },
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Yaml { file, location } => write!(formatter, "{}:{location}", file.display()),
            Self::YamlFile { file } => write!(formatter, "{}", file.display()),
            Self::Environment { variable } => write!(formatter, "environment variable {variable}"),
            Self::SecretFile { key, path } => {
                write!(formatter, "file {} named by {key}", path.display())
            }
            Self::Missing => formatter.write_str("not set"),
        }
    }
}

/// One problem with one key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Problem {
    /// The key path, such as `server.listen` or `server.labels[1].cidr`.
    pub path: String,
    /// Where the value came from.
    pub source: Source,
    /// What is wrong, in the caller's words.
    pub message: String,
}

impl Problem {
    /// A problem at `path`.
    #[must_use]
    pub fn new(path: impl Into<String>, source: Source, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            source,
            message: message.into(),
        }
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}: {} ({})",
            self.path, self.message, self.source
        )
    }
}

/// Every problem a load found, in the order found until [`Problems::into_sorted`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Problems(Vec<Problem>);

impl Problems {
    /// No problems yet.
    #[must_use]
    pub const fn new() -> Self {
        Self(Vec::new())
    }

    /// Adds a problem.
    pub fn push(&mut self, problem: Problem) {
        self.0.push(problem);
    }

    /// Whether there is none.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// How many there are.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    /// The problems in the order found.
    pub fn iter(&self) -> std::slice::Iter<'_, Problem> {
        self.0.iter()
    }

    /// The problems sorted by path, stably: problems at one path keep the order they were found
    /// in, so a loader that looks for nulls, then decodes, then looks for unknown keys lists a
    /// key's problems in that order.
    #[must_use]
    pub fn into_sorted(mut self) -> Vec<Problem> {
        self.0.sort_by(|left, right| left.path.cmp(&right.path));
        self.0
    }
}

impl Extend<Problem> for Problems {
    fn extend<I: IntoIterator<Item = Problem>>(&mut self, problems: I) {
        self.0.extend(problems);
    }
}

impl FromIterator<Problem> for Problems {
    fn from_iter<I: IntoIterator<Item = Problem>>(problems: I) -> Self {
        Self(problems.into_iter().collect())
    }
}

impl<'problems> IntoIterator for &'problems Problems {
    type Item = &'problems Problem;
    type IntoIter = std::slice::Iter<'problems, Problem>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}
