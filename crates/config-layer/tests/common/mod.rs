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

//! A small loader built from the crate's pieces, the way a consumer builds its own: a schema of
//! typed keys, the file, the environment overlay, the merge, the typed walk and the unknown-key
//! sweep, every problem collected.

#![expect(dead_code, reason = "each test file uses part of the helper")]

use std::collections::BTreeMap;
use std::path::Path;

use config_layer::{
    EnvOverlay, Map, Node, Problem, Problems, Raw, Source, TypeError, Typing, merge, null_keys,
    yaml,
};

/// The file name the problems carry.
pub const FILE: &str = "config.yaml";

/// A key's type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Int,
    Number,
    Bool,
    Duration,
    TextList,
    /// A map of text values whose keys are data.
    TextMap,
}

/// A decoded value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Text(String),
    Int(i64),
    Number(f64),
    Bool(bool),
    Duration(jiff::SignedDuration),
    TextList(Vec<String>),
    TextMap(BTreeMap<String, String>),
}

/// What a load gives.
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    pub values: BTreeMap<String, Value>,
    pub problems: Vec<Problem>,
    pub ignored: Vec<String>,
}

impl Loaded {
    /// The paths of the problems, in order.
    pub fn problem_paths(&self) -> Vec<&str> {
        self.problems
            .iter()
            .map(|problem| problem.path.as_str())
            .collect()
    }

    pub fn value(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }
}

/// A schema: its keys with their types.
pub struct Schema {
    pub fields: &'static [(&'static str, Kind)],
}

impl Schema {
    pub fn field(&self, key: &str) -> Option<Kind> {
        self.fields
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, kind)| *kind)
    }

    /// A proper prefix of some key.
    pub fn is_section(&self, key: &str) -> bool {
        self.fields.iter().any(|(name, _)| {
            name.strip_prefix(key)
                .is_some_and(|rest| rest.starts_with('.'))
        })
    }

    /// Whether an environment path is one the loader reads: a key, or an entry of a map key.
    pub fn knows(&self, path: &[&str]) -> bool {
        let dotted = path.join(".");
        if self.field(&dotted).is_some() {
            return true;
        }
        path.split_last()
            .is_some_and(|(_, parent)| self.field(&parent.join(".")) == Some(Kind::TextMap))
    }
}

/// Loads `text` and `environment` under `typing`, with `overlay` built over `schema`.
#[expect(
    clippy::expect_used,
    reason = "a test helper: a broken fixture fails the test"
)]
pub fn load(
    schema: &Schema,
    typing: Typing,
    overlay: &EnvOverlay<'_>,
    text: &str,
    environment: &[&str],
) -> Result<Loaded, yaml::YamlError> {
    let mut tree = yaml::parse(text)?;
    let pairs: Vec<(&str, &str)> = environment
        .iter()
        .map(|entry| entry.split_once('=').expect("NAME=value"))
        .collect();
    let env = overlay.overlay(pairs);
    let mut problems = Problems::new();
    for var in &env.unknown {
        let dotted = var.dotted();
        let message = if schema.is_section(&dotted) {
            "expected a mapping, not text"
        } else {
            "unknown key"
        };
        problems.push(Problem::new(
            dotted,
            Source::Environment {
                variable: var.name.clone(),
            },
            message,
        ));
    }
    merge(&mut tree, env.tree);
    let file = Path::new(FILE);
    for (path, node) in null_keys(&tree) {
        problems.push(Problem::new(
            path,
            Source::of(node, file),
            "key has no value",
        ));
    }
    let values = decode(schema, typing, &tree, &mut problems);
    unknown(schema, &tree, "", &mut problems);
    Ok(Loaded {
        values,
        problems: problems.into_sorted(),
        ignored: env.ignored,
    })
}

#[expect(
    clippy::expect_used,
    reason = "a test helper: a broken fixture fails the test"
)]
fn decode(
    schema: &Schema,
    typing: Typing,
    tree: &Map,
    problems: &mut Problems,
) -> BTreeMap<String, Value> {
    let file = Path::new(FILE);
    let mut values = BTreeMap::new();
    let mut failed_sections = Vec::new();
    for &(key, kind) in schema.fields {
        let segments: Vec<&str> = key.split('.').collect();
        let (leaf, sections) = segments.split_last().expect("a key");
        let mut map = Some(tree);
        for (depth, section) in sections.iter().enumerate() {
            let Some(node) = map.and_then(|level| level.get(*section)) else {
                map = None;
                break;
            };
            map = match &node.value {
                Raw::Map(nested) => Some(nested),
                Raw::Null => None,
                Raw::Bool(_)
                | Raw::Int(_)
                | Raw::Wide(_)
                | Raw::Float(_)
                | Raw::Text(_)
                | Raw::List(_) => {
                    let path = segments[..=depth].join(".");
                    if !failed_sections.contains(&path) {
                        failed_sections.push(path.clone());
                        problems.push(Problem::new(
                            path,
                            Source::of(node, file),
                            format!("expected a map, got {}", node.value.kind()),
                        ));
                    }
                    None
                }
            };
            if map.is_none() {
                break;
            }
        }
        let Some(node) = map.and_then(|level| level.get(*leaf)) else {
            continue;
        };
        if matches!(node.value, Raw::Null) {
            continue;
        }
        let mut report = |path: String, node: &Node, error: TypeError| {
            problems.push(Problem::new(
                path,
                Source::of(node, file),
                error.to_string(),
            ));
        };
        let decoded = match kind {
            Kind::Text => typing.text(node).map(Value::Text),
            Kind::Int => typing.integer(node).map(Value::Int),
            Kind::Number => typing.number(node).map(Value::Number),
            Kind::Bool => typing.boolean(node).map(Value::Bool),
            Kind::Duration => typing.duration(node).map(Value::Duration),
            Kind::TextList => typing.list(node).map(|items| {
                let mut texts = Vec::new();
                for (index, item) in items.into_iter().enumerate() {
                    match typing.text(item) {
                        Ok(text) => texts.push(text),
                        Err(error) => report(format!("{key}[{index}]"), item, error),
                    }
                }
                Value::TextList(texts)
            }),
            Kind::TextMap => match &node.value {
                Raw::Map(entries) => {
                    let mut texts = BTreeMap::new();
                    for (name, entry) in entries {
                        match typing.text(entry) {
                            Ok(text) => {
                                texts.insert(name.clone(), text);
                            }
                            Err(error) => report(format!("{key}.{name}"), entry, error),
                        }
                    }
                    Ok(Value::TextMap(texts))
                }
                other @ (Raw::Null
                | Raw::Bool(_)
                | Raw::Int(_)
                | Raw::Wide(_)
                | Raw::Float(_)
                | Raw::Text(_)
                | Raw::List(_)) => Err(TypeError::Expected {
                    expected: "a map",
                    got: other.kind(),
                }),
            },
        };
        match decoded {
            Ok(value) => {
                values.insert(key.to_owned(), value);
            }
            Err(error) => report(key.to_owned(), node, error),
        }
    }
    values
}

fn unknown(schema: &Schema, map: &Map, prefix: &str, problems: &mut Problems) {
    let file = Path::new(FILE);
    for (name, node) in map {
        let key = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}.{name}")
        };
        if schema.field(&key).is_some() {
            continue;
        }
        if schema.is_section(&key) {
            if let Raw::Map(nested) = &node.value {
                unknown(schema, nested, &key, problems);
            }
            continue;
        }
        problems.push(Problem::new(key, Source::of(node, file), "unknown key"));
    }
}
