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

//! Layering trees: a source merged over a target as koanf merges its providers, a value set at a
//! path, and the keys written without a value.
//!
//! Extracted from bitgauge/src/config/raw.rs at 9be3b0d20765 (`merge`, `insert_path`,
//! `null_keys`), with `insert_path` taking the node's origin; the author relicenses it from
//! GPL-3.0-or-later to MIT OR Apache-2.0 for this repository.

use crate::tree::{Map, Node, Raw};

/// Merges `source` over `target` as koanf merges its providers: a mapping on both sides merges
/// key by key, anything else on the source side replaces the target's value (a list included,
/// whole).
pub fn merge(target: &mut Map, source: Map) {
    for (key, node) in source {
        match (target.get_mut(&key), node) {
            (
                Some(Node {
                    value: Raw::Map(existing),
                    ..
                }),
                Node {
                    value: Raw::Map(nested),
                    ..
                },
            ) => merge(existing, nested),
            (_, node) => {
                target.insert(key, node);
            }
        }
    }
}

/// Sets `node` at the key path `path`, creating the mappings on the way (with the node's origin)
/// and replacing a non-mapping that stands in the path. An empty path sets nothing.
pub fn insert_path<S: AsRef<str>>(root: &mut Map, path: &[S], node: Node) {
    let Some((last, parents)) = path.split_last() else {
        return;
    };
    let mut current = root;
    for parent in parents {
        let slot = current
            .entry(parent.as_ref().to_owned())
            .or_insert_with(|| Node::new(Raw::Map(Map::new()), node.origin.clone()));
        if !matches!(slot.value, Raw::Map(_)) {
            *slot = Node::new(Raw::Map(Map::new()), node.origin.clone());
        }
        let Raw::Map(nested) = &mut slot.value else {
            return;
        };
        current = nested;
    }
    current.insert(last.as_ref().to_owned(), node);
}

/// The path and node of every null in the tree, mappings in key order and list elements by
/// index: `a.b`, `a.list[2]`.
#[must_use]
pub fn null_keys(root: &Map) -> Vec<(String, &Node)> {
    let mut keys = Vec::new();
    for (name, node) in root {
        collect_nulls(node, name.clone(), &mut keys);
    }
    keys
}

fn collect_nulls<'tree>(node: &'tree Node, path: String, keys: &mut Vec<(String, &'tree Node)>) {
    match &node.value {
        Raw::Null => keys.push((path, node)),
        Raw::Map(entries) => {
            for (name, nested) in entries {
                collect_nulls(nested, format!("{path}.{name}"), keys);
            }
        }
        Raw::List(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_nulls(item, format!("{path}[{index}]"), keys);
            }
        }
        Raw::Bool(_) | Raw::Int(_) | Raw::Wide(_) | Raw::Float(_) | Raw::Text(_) => {}
    }
}
