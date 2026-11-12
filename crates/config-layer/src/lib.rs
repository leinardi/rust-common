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

//! A layered configuration loader: a YAML file and the environment merged into one untyped tree
//! that remembers where each value came from, typed key by key, with every problem collected.
//!
//! The pieces, in the order a loader uses them:
//!
//! 1. [`yaml::parse`] (or [`yaml::read_file`]) reads the file into a [`Map`] of [`Node`]s, each
//!    with its line and column.
//! 2. [`EnvOverlay`] turns the process environment into a second tree, checking every path
//!    against the caller's known paths before inserting it; the variables it did not take come
//!    back as data.
//! 3. [`merge`] lays the environment tree over the file's, as koanf merges its providers;
//!    [`null_keys`] finds the keys written without a value.
//! 4. The caller walks its own schema over the tree and reads each value through a [`Typing`]
//!    profile (integers, numbers, booleans, text, lists, durations), whose rules can differ by
//!    the value's [`Origin`].
//! 5. [`read_secret_file`] reads a `*_file` key's file, when the caller has one.
//! 6. Every problem is a [`Problem`] with its [`Source`], collected by [`Problems`] and sorted by
//!    path.
//!
//! The schema, the rule texts, the exemptions and the logging stay the caller's: this crate
//! decides nothing a consumer may want to decide differently, and takes a parameter instead.
//!
//! Extracted from bitgauge/src/config/{raw,load,decode,types,text,mod}.rs at 9be3b0d20765 (each
//! module names its sources; `report.rs` is bitgauge's logging and stays there); the author
//! relicenses these files from GPL-3.0-or-later to MIT OR Apache-2.0 for this repository.

mod duration;
mod env;
mod merge;
mod problem;
mod secret;
pub mod text;
mod tree;
mod typing;
pub mod yaml;

pub use duration::{BareZero, DayUnit, DurationError, DurationSyntax, parse_duration};
pub use env::{
    EmptyValue, EnvOverlay, EnvVar, ItemTrim, ListSplit, Overlay, Unseparated, process_environment,
};
pub use merge::{insert_path, merge, null_keys};
pub use problem::{Problem, Problems, Source};
pub use secret::{EmptySecret, SecretError, SecretTrim, read_secret_file};
pub use tree::{Location, Map, Node, Origin, Raw};
pub use typing::{
    BoolSpelling, IntegerSyntax, IntegerText, QuotedScalars, ScalarLists, TypeError, Typing,
};
