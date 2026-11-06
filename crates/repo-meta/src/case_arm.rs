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

//! Reads the shell `case` arms that choose a download by platform, each setting an asset and its
//! sha256 (`amd64) triple=x86_64-unknown-linux-musl; std_sha256=<digest> ;;`), and refuses what it
//! cannot read rather than skipping it. An arm's pattern may list alternatives (`amd64|x86_64)`),
//! open with `(` and quote its words; each alternative is one label. Every `asset=…; sha=…` pair
//! and every `sha=` assignment is also counted on its own, so an arm whose pattern is not read
//! here still fails the check instead of escaping it.

use std::collections::{BTreeMap, BTreeSet};

use crate::pattern;

/// One word of a `case` pattern: quoted parts and unquoted characters that are no shell operator.
const WORD: &str = r#"(?:"[^"\n]*"|'[^'\n]*'|[^\s|()"';&<>\\]+)+"#;
/// A value as an assignment writes it: a literal, `${VAR}`, `$VAR` or a quoted string.
const VALUE: &str = r#"(?:"[^"\n]*"|'[^'\n]*'|[^;\s"']+)+"#;

/// One arm that sets the pair: the labels of its pattern, unquoted, and the two values as
/// written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Arm {
    pub(crate) labels: Vec<String>,
    pub(crate) asset: String,
    pub(crate) sha: String,
}

/// What `text` sets the pair with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Case {
    /// The arms read, in order.
    pub(crate) arms: Vec<Arm>,
    /// Every `asset=…; sha=…` pair, inside an arm read or not, with its two values.
    pub(crate) pairs: Vec<(String, String)>,
    /// How many times `sha=` is assigned, in any shape.
    pub(crate) assignments: usize,
}

/// The arms of `text` that set `asset_var` and then `sha_var`.
pub(crate) fn arms(text: &str, asset_var: &str, sha_var: &str) -> Case {
    let asset = regex::escape(asset_var);
    let sha = regex::escape(sha_var);
    let arm = pattern(&format!(
        r"(?:^|[\s;])\(?[ \t]*({WORD}(?:[ \t]*\|[ \t]*{WORD})*)[ \t]*\)\s*{asset}=({VALUE});\s*{sha}=({VALUE})"
    ));
    let label = pattern(WORD);
    let arms = arm
        .captures_iter(text)
        .map(|captures| Arm {
            labels: label
                .find_iter(&captures[1])
                .map(|word| word.as_str().replace(['"', '\''], ""))
                .collect(),
            asset: captures[2].to_owned(),
            sha: captures[3].to_owned(),
        })
        .collect();
    let pairs = pattern(&format!(r"\b{asset}=({VALUE});\s*{sha}=({VALUE})"))
        .captures_iter(text)
        .map(|captures| (captures[1].to_owned(), captures[2].to_owned()))
        .collect();
    let assignments = pattern(&format!(r"\b{sha}=")).find_iter(text).count();
    Case {
        arms,
        pairs,
        assignments,
    }
}

impl Case {
    /// Each label with the pair its arm sets.
    pub(crate) fn labelled(&self) -> BTreeSet<(String, String, String)> {
        self.arms
            .iter()
            .flat_map(|arm| {
                arm.labels
                    .iter()
                    .map(|label| (label.clone(), arm.asset.clone(), arm.sha.clone()))
            })
            .collect()
    }

    /// What is wrong beyond the labelled set, as messages that open with a verb: a label in more
    /// than one place (the `case` takes the first), a pair or `sha=` assignment no arm read here
    /// holds, and a pair whose values are not among `want`'s.
    pub(crate) fn problems(
        &self,
        asset_var: &str,
        sha_var: &str,
        want: &BTreeSet<(String, String, String)>,
    ) -> Vec<String> {
        let mut problems = Vec::new();
        let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
        for label in self.arms.iter().flat_map(|arm| &arm.labels) {
            *seen.entry(label).or_default() += 1;
        }
        for (label, count) in seen {
            if count > 1 {
                problems.push(format!(
                    "labels {count} arms that set {sha_var} {label:?}; the case takes the first"
                ));
            }
        }
        let read = self.arms.len();
        if self.pairs.len() != read || self.assignments != read {
            problems.push(format!(
                "sets {asset_var} with {sha_var} {} times and {sha_var} {} times, but {read} \
                 case arms are read; a pair outside a plain arm is not checked",
                self.pairs.len(),
                self.assignments
            ));
        }
        let wanted: BTreeSet<(&str, &str)> = want
            .iter()
            .map(|(_, asset, sha)| (asset.as_str(), sha.as_str()))
            .collect();
        for (asset, sha) in &self.pairs {
            if !wanted.contains(&(asset.as_str(), sha.as_str())) {
                problems.push(format!(
                    "sets {asset_var}={asset} with {sha_var}={sha}, which no wanted arm holds"
                ));
            }
        }
        problems
    }
}

#[cfg(test)]
mod tests {
    use super::arms;

    /// Each pattern shape is read with its labels unquoted, and every pair and assignment counts.
    #[test]
    fn every_pattern_shape_is_read() {
        for (text, labels) in [
            ("in amd64) t=a; s=b ;; esac", vec!["amd64"]),
            ("in (amd64) t=a; s=b ;; esac", vec!["amd64"]),
            ("in \"amd64\") t=a; s=b ;; esac", vec!["amd64"]),
            ("in 'amd64') t=a; s=b ;; esac", vec!["amd64"]),
            ("in amd64|x86_64) t=a; s=b ;; esac", vec!["amd64", "x86_64"]),
            (
                "in ( amd64 | \"x86_64\" ) t=a; s=b ;; esac",
                vec!["amd64", "x86_64"],
            ),
            ("in *) t=a; s=b ;; esac", vec!["*"]),
            ("in arm[0-9]*) t=a; s=b ;; esac", vec!["arm[0-9]*"]),
        ] {
            let read = arms(text, "t", "s");
            assert_eq!(read.arms.len(), 1, "{text:?}");
            assert_eq!(read.arms[0].labels, labels, "{text:?}");
            assert_eq!((read.pairs.len(), read.assignments), (1, 1), "{text:?}");
        }
    }

    /// A value of any shape is a pair; an arm pattern this reader does not take is still counted.
    #[test]
    fn every_pair_counts() {
        let read = arms(
            "amd64) t=a; s=${S} ;; arm64) t=\"b\"; s=$S ;; x\\)) t=c; s='d' ;; t=e s=f",
            "t",
            "s",
        );
        assert_eq!(read.arms.len(), 2);
        assert_eq!(
            read.pairs,
            [
                ("a".to_owned(), "${S}".to_owned()),
                ("\"b\"".to_owned(), "$S".to_owned()),
                ("c".to_owned(), "'d'".to_owned()),
            ]
        );
        assert_eq!(read.assignments, 4);
    }
}
