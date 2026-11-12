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

//! The environment overlay: `<PREFIX>A<SEP>B<SEP>C` becomes the key path `a.b.c`, checked against
//! the caller's known paths **before** it is inserted, so an unknown path can neither replace a
//! known value nor be erased by a later one. What the overlay does not take comes back as data,
//! for the caller's own exemptions and rule texts.
//!
//! The parameters cover the consumers' differences: which paths are maps whose keys keep their
//! case (Go's `envKey`), which keys are lists split in one variable, whether the items are
//! trimmed, what an empty value makes, and whether a variable without the separator is ignored
//! or mapped to a one-segment path.
//!
//! Segments are lowercased with `str::to_lowercase`; Go's `strings.ToLower` differs only on
//! characters no variable name of a known path holds. A segment is kept whole: a `.` inside one
//! (`..._INCLUDE_LABELS__com.example.tier`) stays in that map key, where koanf, joining the
//! segments with `.` and splitting them again, nests it; a `known` predicate that joins the
//! segments with `.` sees the two alike. An empty separator splits nothing: under
//! [`Unseparated::Mapped`] the whole name after the prefix is one segment, under
//! [`Unseparated::Ignored`] every variable is ignored. An empty prefix takes the whole
//! environment, so every variable that is not a known path comes back in [`Overlay::unknown`].
//!
//! Extracted from bitgauge/src/config/load.rs at 9be3b0d20765 (`environment_tree`,
//! `environment_value`), with the map-path rule of runhold/internal/config/load.go (`envKey`); the
//! author relicenses it from GPL-3.0-or-later to MIT OR Apache-2.0 for this repository.

use std::ffi::OsString;
use std::fmt;

use crate::merge::insert_path;
use crate::tree::{Map, Node, Origin, Raw};

/// What a variable that starts with the prefix but holds no separator is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Unseparated {
    /// Not configuration (a Kubernetes service link such as `BITGAUGE_PORT`): listed in
    /// [`Overlay::ignored`].
    Ignored,
    /// A one-segment path, as koanf's env provider maps it.
    Mapped,
}

/// Whether the items of a list variable are trimmed of white space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ItemTrim {
    /// `a, b` is `["a", "b"]`.
    Trimmed,
    /// `a, b` is `["a", " b"]`, as Go's `strings.Split`.
    Untrimmed,
}

/// What an empty list variable makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EmptyValue {
    /// An empty list, which clears the file's list.
    NoItems,
    /// One empty item, as Go's `strings.Split("", ",")`.
    OneEmptyItem,
}

/// How a list key's variable is split into items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct ListSplit {
    /// The item separator, such as `,`.
    pub separator: char,
    /// Whether the items are trimmed.
    pub items: ItemTrim,
    /// What an empty value makes.
    pub empty: EmptyValue,
}

impl ListSplit {
    /// Items split at `separator`, trimmed or not, with `empty` for an empty value.
    #[must_use]
    pub const fn new(separator: char, items: ItemTrim, empty: EmptyValue) -> Self {
        Self {
            separator,
            items,
            empty,
        }
    }
}

type PathPredicate<'rules> = Box<dyn Fn(&[&str]) -> bool + Send + Sync + 'rules>;

/// The rules that turn environment variables into a tree. Built with [`EnvOverlay::new`], then
/// [`map_paths`](Self::map_paths) and [`lists`](Self::lists) where the caller has them; applied
/// with [`overlay`](Self::overlay).
pub struct EnvOverlay<'rules> {
    prefix: String,
    separator: String,
    unseparated: Unseparated,
    known: PathPredicate<'rules>,
    map_path: Option<PathPredicate<'rules>>,
    lists: Option<(PathPredicate<'rules>, ListSplit)>,
}

impl fmt::Debug for EnvOverlay<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnvOverlay")
            .field("prefix", &self.prefix)
            .field("separator", &self.separator)
            .field("unseparated", &self.unseparated)
            .field("map_paths", &self.map_path.is_some())
            .field("lists", &self.lists.as_ref().map(|(_, split)| split))
            .finish_non_exhaustive()
    }
}

/// A variable the overlay did not insert: its path is not one the caller knows. Its value may be
/// a secret (a variable a `*_env` key names, for one), so its `Debug` shows only the value's
/// length, and a caller reports it with care.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EnvVar {
    /// The key path it maps to, segment by segment.
    pub path: Vec<String>,
    /// The variable's name.
    pub name: String,
    /// The variable's value, as given.
    pub value: String,
}

impl fmt::Debug for EnvVar {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnvVar")
            .field("path", &self.path)
            .field("name", &self.name)
            .field("value", &format_args!("<{} bytes>", self.value.len()))
            .finish()
    }
}

impl EnvVar {
    /// The path with its segments joined by `.`.
    #[must_use]
    pub fn dotted(&self) -> String {
        self.path.join(".")
    }
}

/// What the overlay made of an environment.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Overlay {
    /// The known paths, as a tree to merge over the file's; a later variable with the same path
    /// replaces an earlier one.
    pub tree: Map,
    /// The variables whose path the caller does not know, sorted by path, then name.
    pub unknown: Vec<EnvVar>,
    /// The names of the variables without a separator, under [`Unseparated::Ignored`], sorted.
    pub ignored: Vec<String>,
}

impl<'rules> EnvOverlay<'rules> {
    /// The overlay for variables starting with `prefix` (such as `RUNHOLD_`), whose levels are
    /// split by `separator` (such as `__`; a single underscore stays inside a key name).
    /// `known` says whether a key path, segment by segment, is one the caller reads; it is asked
    /// before a variable is inserted. No path is a map path and no key a list until
    /// [`map_paths`](Self::map_paths) and [`lists`](Self::lists) say so.
    #[must_use]
    pub fn new(
        prefix: impl Into<String>,
        separator: impl Into<String>,
        unseparated: Unseparated,
        known: impl Fn(&[&str]) -> bool + Send + Sync + 'rules,
    ) -> Self {
        Self {
            prefix: prefix.into(),
            separator: separator.into(),
            unseparated,
            known: Box::new(known),
            map_path: None,
            lists: None,
        }
    }

    /// The map paths: the segment right after a path for which `is_map` holds is a map key and
    /// keeps its case; the segments after it are lowercased again (a field of the map's value).
    /// `is_map` is asked with the lowercased segments up to each level, and only until it first
    /// holds, as Go's `envKey`.
    #[must_use]
    pub fn map_paths(mut self, is_map: impl Fn(&[&str]) -> bool + Send + Sync + 'rules) -> Self {
        self.map_path = Some(Box::new(is_map));
        self
    }

    /// The list keys: a variable whose path `is_list` holds for is split into items by `split`.
    /// Any other variable is one text value.
    #[must_use]
    pub fn lists(
        mut self,
        is_list: impl Fn(&[&str]) -> bool + Send + Sync + 'rules,
        split: ListSplit,
    ) -> Self {
        self.lists = Some((Box::new(is_list), split));
        self
    }

    /// The tree of the variables in `environment`, in order (a later duplicate path wins), with
    /// the variables it did not insert. A variable not starting with the prefix (case-sensitive)
    /// is skipped silently.
    #[must_use]
    pub fn overlay<I, N, V>(&self, environment: I) -> Overlay
    where
        I: IntoIterator<Item = (N, V)>,
        N: AsRef<str>,
        V: AsRef<str>,
    {
        let mut overlay = Overlay::default();
        for (name, value) in environment {
            let (name, value) = (name.as_ref(), value.as_ref());
            let Some(rest) = name.strip_prefix(&self.prefix) else {
                continue;
            };
            let separated = !self.separator.is_empty() && rest.contains(&self.separator);
            if !separated && self.unseparated == Unseparated::Ignored {
                overlay.ignored.push(name.to_owned());
                continue;
            }
            let path = self.path(rest);
            let segments: Vec<&str> = path.iter().map(String::as_str).collect();
            if !(self.known)(&segments) {
                overlay.unknown.push(EnvVar {
                    path,
                    name: name.to_owned(),
                    value: value.to_owned(),
                });
                continue;
            }
            let origin = Origin::Environment(name.to_owned());
            let raw = match &self.lists {
                Some((is_list, split)) if is_list(&segments) => {
                    Raw::List(split_items(value, *split, &origin))
                }
                Some(_) | None => Raw::Text(value.to_owned()),
            };
            insert_path(&mut overlay.tree, &segments, Node::new(raw, origin));
        }
        overlay.unknown.sort();
        overlay.ignored.sort();
        overlay
    }

    /// The key path of a variable's name after the prefix.
    fn path(&self, rest: &str) -> Vec<String> {
        let mut segments: Vec<String> = if self.separator.is_empty() {
            vec![rest.to_owned()]
        } else {
            rest.split(self.separator.as_str())
                .map(str::to_owned)
                .collect()
        };
        let mut key_at = None;
        for index in 0..segments.len() {
            if key_at != Some(index) {
                segments[index] = segments[index].to_lowercase();
            }
            if key_at.is_none()
                && let Some(is_map) = &self.map_path
            {
                let prefix: Vec<&str> = segments[..=index].iter().map(String::as_str).collect();
                if is_map(&prefix) {
                    key_at = Some(index + 1);
                }
            }
        }
        segments
    }
}

fn split_items(value: &str, split: ListSplit, origin: &Origin) -> Vec<Node> {
    if value.is_empty() && split.empty == EmptyValue::NoItems {
        return Vec::new();
    }
    value
        .split(split.separator)
        .map(|item| match split.items {
            ItemTrim::Trimmed => item.trim(),
            ItemTrim::Untrimmed => item,
        })
        .map(|item| Node::new(Raw::Text(item.to_owned()), origin.clone()))
        .collect()
}

/// The process environment as name and value pairs, a name or value that is not UTF-8 converted
/// lossily (where `std::env::vars` would panic).
#[must_use]
pub fn process_environment() -> Vec<(String, String)> {
    std::env::vars_os()
        .map(|(name, value)| (lossy(name), lossy(value)))
        .collect()
}

fn lossy(text: OsString) -> String {
    text.into_string()
        .unwrap_or_else(|text| text.to_string_lossy().into_owned())
}
