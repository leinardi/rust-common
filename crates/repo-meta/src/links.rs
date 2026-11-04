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

//! The offline link check of the Markdown documentation, the port of the consumers' Go
//! `test/docs/links.go`: every relative link and fragment must resolve, fragments under GitHub's
//! heading slug rules. It needs no network and no tool beyond cargo, so it runs as a pre-commit
//! hook.
//!
//! The patterns are the Go ones with RE2's ASCII classes spelled out: Rust's `\s`, `\S` and `\b`
//! are Unicode-aware, Go's are not, so `[\t\n\f\r ]` stands in for them. Paths are cleaned with
//! Go's `path.Clean`, headings lower-cased with Go's `strings.ToLower` and targets quoted with
//! Go's `strconv.Quote`, from go-compat.
//!
//! Extracted from bitgauge/meta/tests/docs/links.rs at 600fc3905200 (the RE2 classes, Go's
//! lower-casing, quoting and `path.Dir`, and [`Checker::check_text`], which checks a text as if it
//! were a file of the repository), the same reader as
//! swarm-scheduler-exporter/meta/src/links.rs at c9e79afa8810 (whose negative controls the tests
//! keep), with a read failure reported as a problem rather than a panic.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use go_compat::filepath::clean;
use go_compat::quote::quote;
use go_compat::strings::to_lower;
use regex::Regex;

use crate::{Problem, regex};

static INLINE_LINK: LazyLock<Regex> = LazyLock::new(|| {
    regex(concat!(
        r"!?\[[^\]]*\]\([\t\n\f\r ]*(<[^>]*>|[^)\t\n\f\r ]+)",
        r#"(?:[\t\n\f\r ]+"[^"]*")?[\t\n\f\r ]*\)"#,
    ))
});
static REFERENCE_DEF: LazyLock<Regex> =
    LazyLock::new(|| regex(r"^[\t\n\f\r ]{0,3}\[[^\]]+\]:[\t\n\f\r ]*(<[^>]*>|[^\t\n\f\r ]+)"));
static ATX_HEADING: LazyLock<Regex> = LazyLock::new(|| {
    regex(r"^[\t\n\f\r ]{0,3}(#{1,6})[\t\n\f\r ]+(.*?)[\t\n\f\r ]*#*[\t\n\f\r ]*$")
});
static HTML_ANCHOR: LazyLock<Regex> =
    LazyLock::new(|| regex(r#"<a[\t\n\f\r ]+(?:id|name)="([^"]+)""#));
static CODE_SPAN: LazyLock<Regex> = LazyLock::new(|| regex("`+[^`]*`+"));
static LINK_IN_HEADING: LazyLock<Regex> = LazyLock::new(|| regex(r"!?\[([^\]]*)\]\([^)]*\)"));
static HTML_TAG: LazyLock<Regex> = LazyLock::new(|| regex(r"<[^>]+>"));
static EDGE_UNDERLINE: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(^|[^\p{L}\p{N}])_+|_+([^\p{L}\p{N}]|$)"));
static URI_SCHEME: LazyLock<Regex> = LazyLock::new(|| regex(r"^[a-zA-Z][a-zA-Z0-9+.-]*:"));
/// What a slug drops: everything but letters, marks, numbers, connector punctuation, spaces and
/// hyphens (Go's `unicode.IsLetter`, `IsMark`, `IsNumber` and `Pc`, by general category).
static SLUG_DROPPED: LazyLock<Regex> = LazyLock::new(|| regex(r"[^\p{L}\p{M}\p{N}\p{Pc} -]"));

/// The lines of a Markdown text that are prose, with fenced code blocks blanked out so line
/// numbers still match the file. Split on `\n` alone, as Go does, so a `\r` stays on its line.
#[must_use]
pub fn lines(content: &str) -> Vec<String> {
    let mut lines: Vec<String> = content.split('\n').map(str::to_owned).collect();
    let mut fence = "";
    for line in &mut lines {
        let trimmed = line.trim();
        if !fence.is_empty() {
            if trimmed.starts_with(fence) {
                fence = "";
            }
            line.clear();
            continue;
        }
        if let Some(marker) = ["```", "~~~"]
            .into_iter()
            .find(|marker| trimmed.starts_with(marker))
        {
            fence = marker;
            line.clear();
        }
    }
    lines
}

/// The fragments a Markdown text defines: its headings under GitHub's slug rules, numbered on
/// repetition, and any explicit HTML anchors.
#[must_use]
pub fn anchors(content: &str) -> HashSet<String> {
    let mut anchors = HashSet::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for line in lines(content) {
        for found in HTML_ANCHOR.captures_iter(&line) {
            anchors.insert(found[1].to_owned());
        }
        let Some(heading) = ATX_HEADING.captures(&line) else {
            continue;
        };
        let slug = slug(&heading[2]);
        let count = seen.entry(slug.clone()).or_default();
        if *count > 0 {
            anchors.insert(format!("{slug}-{count}"));
        } else {
            anchors.insert(slug);
        }
        *count += 1;
    }
    anchors
}

/// The anchor GitHub generates for a heading: the rendered text, lower-cased rune by rune, with
/// everything but letters, marks, numbers, connector punctuation, spaces and hyphens removed, and
/// each space turned into a hyphen.
#[must_use]
pub fn slug(heading: &str) -> String {
    SLUG_DROPPED
        .replace_all(&to_lower(&render_text(heading)), "")
        .replace(' ', "-")
}

/// The heading text as rendered: link targets, HTML tags, emphasis markers and code-span
/// backticks stripped, the code itself kept.
fn render_text(heading: &str) -> String {
    let mut rendered = String::new();
    let mut last = 0;
    for span in CODE_SPAN.find_iter(heading) {
        rendered.push_str(&strip_inline(&heading[last..span.start()]));
        rendered.push_str(span.as_str().trim_matches('`'));
        last = span.end();
    }
    rendered.push_str(&strip_inline(&heading[last..]));
    rendered
}

fn strip_inline(text: &str) -> String {
    let text = LINK_IN_HEADING.replace_all(text, "$1");
    let text = HTML_TAG.replace_all(&text, "");
    let text = text.replace('*', "");
    EDGE_UNDERLINE.replace_all(&text, "$1$2").into_owned()
}

/// The link targets on one prose line, inline code spans excluded.
#[must_use]
pub fn targets(line: &str) -> Vec<String> {
    let line = CODE_SPAN.replace_all(line, "");
    let mut targets: Vec<String> = INLINE_LINK
        .captures_iter(&line)
        .map(|found| found[1].trim_matches(['<', '>']).to_owned())
        .collect();
    if let Some(found) = REFERENCE_DEF.captures(&line) {
        targets.push(found[1].trim_matches(['<', '>']).to_owned());
    }
    targets
}

/// Whether a slash path names a Markdown file: Go's case-sensitive `.md` suffix.
#[must_use]
pub fn is_markdown(path: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|extension| extension == "md")
}

/// Resolves links against a repository root, caching each target file's anchors.
#[derive(Debug)]
pub struct Checker {
    root: PathBuf,
    anchors: HashMap<String, HashSet<String>>,
}

impl Checker {
    /// A checker for the repository at `root`.
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_owned(),
            anchors: HashMap::new(),
        }
    }

    /// Checks every relative link in the Markdown file at `relative`, a slash-separated path
    /// relative to the root; a file that cannot be read is one problem.
    pub fn check_file(&mut self, relative: &str) -> Vec<Problem> {
        match fs::read_to_string(self.root.join(relative)) {
            Ok(content) => self.check_text(relative, &content),
            Err(error) => vec![Problem::at(relative, format!("cannot be read: {error}"))],
        }
    }

    /// Checks `content` as if it were the file at `relative`, so a negative control can break a
    /// copy of a real page without touching it. Paths are resolved logically, against the file's
    /// own directory, and then followed through any symlink on disk.
    pub fn check_text(&mut self, relative: &str, content: &str) -> Vec<Problem> {
        let mut problems = Vec::new();
        for (index, line) in lines(content).iter().enumerate() {
            for target in targets(line) {
                if let Some(message) = self.check_target(relative, &target) {
                    problems.push(Problem::at(format!("{relative}:{}", index + 1), message));
                }
            }
        }
        problems
    }

    fn check_target(&mut self, relative: &str, target: &str) -> Option<String> {
        if URI_SCHEME.is_match(target) || target.starts_with("//") {
            return None;
        }
        let (file, fragment) = target.split_once('#').unwrap_or((target, ""));
        let quoted = quote(target);
        let mut resolved = relative.to_owned();
        if !file.is_empty() {
            resolved = match file.strip_prefix('/') {
                Some(absolute) => clean(absolute),
                None => clean(&format!("{}/{file}", dir(relative))),
            };
            if resolved == ".." || resolved.starts_with("../") {
                return Some(format!("link {quoted} leaves the repository"));
            }
            if fs::metadata(self.root.join(&resolved)).is_err() {
                return Some(format!("link {quoted}: {resolved} does not exist"));
            }
        }
        if fragment.is_empty() || !is_markdown(&resolved) {
            return None;
        }
        match self.anchors_of(&resolved) {
            Err(error) => Some(format!("link {quoted}: {error}")),
            Ok(anchors) if !anchors.contains(fragment) => Some(format!(
                "link {quoted}: {resolved} has no heading or anchor #{fragment}"
            )),
            Ok(_) => None,
        }
    }

    fn anchors_of(&mut self, relative: &str) -> Result<&HashSet<String>, String> {
        if !self.anchors.contains_key(relative) {
            let content = fs::read_to_string(self.root.join(relative))
                .map_err(|error| format!("read {relative}: {error}"))?;
            self.anchors.insert(relative.to_owned(), anchors(&content));
        }
        self.anchors
            .get(relative)
            .ok_or_else(|| format!("read {relative}: not cached"))
    }
}

/// Go's `path.Dir`: everything before the last slash, cleaned; `.` without one.
fn dir(path: &str) -> String {
    clean(path.rfind('/').map_or("", |slash| &path[..slash]))
}

#[cfg(test)]
mod tests;
