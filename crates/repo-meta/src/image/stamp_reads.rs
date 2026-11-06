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

//! The variables a version module reads at compile time, from its tokens: comments (nested block
//! comments included) are skipped and a string literal is one token, so neither a commented-out
//! read nor one quoted in a string counts.
//!
//! Generalises swarm-device-access/meta/tests/dockerfiles.rs's `stamp_reads` (staged on
//! 8b4ab3a3e417), which matched the text with regexes.

use std::collections::BTreeSet;

use super::BUILDINFO_FIELDS;

/// A token of Rust source, as far as the reads need one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Token<'text> {
    /// An identifier or keyword; a raw identifier `r#name` is `name`.
    Ident(&'text str),
    /// Any other byte outside a literal and a comment, such as `!`, `(` or one `:` of a `::`.
    Punct(u8),
    /// A string literal, plain or raw: its text between the quotes, as written.
    Str(&'text str),
    /// Any other literal: a number, a character, a byte, a byte string or a C string.
    Literal,
}

fn is_ident_start(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic() || !byte.is_ascii()
}

fn is_ident_continue(byte: u8) -> bool {
    is_ident_start(byte) || byte.is_ascii_digit()
}

/// The end of the word that starts at `start`.
fn word_end(bytes: &[u8], start: usize) -> usize {
    bytes[start..]
        .iter()
        .position(|byte| !is_ident_continue(*byte))
        .map_or(bytes.len(), |offset| start + offset)
}

/// The end of the block comment that opens at `start`, nested ones included (the end of the text
/// for an unterminated one).
fn block_comment_end(bytes: &[u8], start: usize) -> usize {
    let mut depth = 0_usize;
    let mut at = start;
    while at < bytes.len() {
        match (bytes[at], bytes.get(at + 1)) {
            (b'/', Some(b'*')) => {
                depth += 1;
                at += 2;
            }
            (b'*', Some(b'/')) => {
                depth -= 1;
                at += 2;
                if depth == 0 {
                    return at;
                }
            }
            _ => at += 1,
        }
    }
    bytes.len()
}

/// The text of the quoted literal whose contents start at `start`, and the index past its closing
/// quote; an escaped quote does not close it.
fn quoted(text: &str, start: usize) -> (&str, usize) {
    let bytes = text.as_bytes();
    let mut at = start;
    while at < bytes.len() {
        match bytes[at] {
            b'\\' => at += 2,
            b'"' => return (&text[start..at], at + 1),
            _ => at += 1,
        }
    }
    (&text[start.min(bytes.len())..], bytes.len())
}

/// The text of the raw string whose `hashes` hashes and opening quote end at `start`, and the
/// index past its closing quote and hashes.
fn raw_quoted(text: &str, start: usize, hashes: usize) -> (&str, usize) {
    let closing = format!("\x22{}", "#".repeat(hashes));
    text[start..].find(&closing).map_or_else(
        || (&text[start..], text.len()),
        |offset| {
            let close = start + offset;
            (&text[start..close], close + closing.len())
        },
    )
}

/// The index past the character literal whose opening quote is at `start`, or `None` when the
/// quote opens a lifetime or a label.
fn char_literal_end(bytes: &[u8], start: usize) -> Option<usize> {
    let first = *bytes.get(start + 1)?;
    if first == b'\\' {
        // The escaped character, then up to the closing quote (`'\''`, `'\u{1F600}'`).
        let after = start + 3;
        return Some(
            bytes
                .get(after..)?
                .iter()
                .position(|byte| *byte == b'\'')
                .map_or(bytes.len(), |offset| after + offset + 1),
        );
    }
    let width = match first {
        0xF0.. => 4,
        0xE0.. => 3,
        0xC0.. => 2,
        _ => 1,
    };
    let close = start + 1 + width;
    (bytes.get(close) == Some(&b'\'')).then_some(close + 1)
}

/// The tokens of `text`, comments skipped.
fn tokens(text: &str) -> Vec<Token<'_>> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        let next = bytes.get(at + 1).copied();
        match byte {
            b'/' if next == Some(b'/') => {
                at = bytes[at..]
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or(bytes.len(), |offset| at + offset);
            }
            b'/' if next == Some(b'*') => at = block_comment_end(bytes, at),
            b'"' => {
                let (content, end) = quoted(text, at + 1);
                tokens.push(Token::Str(content));
                at = end;
            }
            b'\'' => {
                if let Some(end) = char_literal_end(bytes, at) {
                    tokens.push(Token::Literal);
                    at = end;
                } else {
                    tokens.push(Token::Punct(byte));
                    at += 1;
                }
            }
            b'0'..=b'9' => {
                tokens.push(Token::Literal);
                at = word_end(bytes, at);
            }
            _ if is_ident_start(byte) => at = word(text, at, &mut tokens),
            _ if byte.is_ascii_whitespace() => at += 1,
            _ => {
                tokens.push(Token::Punct(byte));
                at += 1;
            }
        }
    }
    tokens
}

/// Pushes the token of the word that starts at `start` (an identifier, or the prefix of a raw
/// string, a byte or C string, a byte or a raw identifier) and returns the index past it.
fn word<'text>(text: &'text str, start: usize, tokens: &mut Vec<Token<'text>>) -> usize {
    let bytes = text.as_bytes();
    let end = word_end(bytes, start);
    let word = &text[start..end];
    let hashes = bytes[end..]
        .iter()
        .take_while(|byte| **byte == b'#')
        .count();
    let after_hashes = bytes.get(end + hashes).copied();
    match (word, after_hashes) {
        ("r" | "br" | "cr", Some(b'"')) => {
            let (content, past) = raw_quoted(text, end + hashes + 1, hashes);
            tokens.push(if word == "r" {
                Token::Str(content)
            } else {
                Token::Literal
            });
            past
        }
        ("r", Some(next)) if hashes == 1 && is_ident_start(next) => {
            let name_end = word_end(bytes, end + 1);
            tokens.push(Token::Ident(&text[end + 1..name_end]));
            name_end
        }
        ("b" | "c", Some(b'"')) if hashes == 0 => {
            tokens.push(Token::Literal);
            quoted(text, end + 1).1
        }
        ("b", Some(b'\'')) if hashes == 0 => {
            tokens.push(Token::Literal);
            char_literal_end(bytes, end).unwrap_or(end + 1)
        }
        _ => {
            tokens.push(Token::Ident(word));
            end
        }
    }
}

/// How the macro name at an index is qualified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Path<'text> {
    /// Not at all: `name!`.
    Bare,
    /// By one crate or module, with or without a leading `::`: `krate::name!`, `::krate::name!`.
    In(&'text str),
    /// Any other way, such as `a::b::name!` or `::name!`.
    Other,
}

const COLON: Token<'static> = Token::Punct(b':');

/// How the name at `index` is qualified.
fn path_of<'text>(tokens: &[Token<'text>], index: usize) -> Path<'text> {
    let before = &tokens[..index];
    let Some(rest) = before.strip_suffix(&[COLON, COLON]) else {
        return Path::Bare;
    };
    let Some((Token::Ident(segment), rest)) = rest.split_last() else {
        return Path::Other;
    };
    match rest.strip_suffix(&[COLON, COLON]) {
        None => Path::In(segment),
        Some(rest) if !matches!(rest.last(), Some(Token::Ident(_))) => Path::In(segment),
        Some(_) => Path::Other,
    }
}

/// Whether `tokens` import service-kit's `buildinfo` under its own name: `use service_kit::
/// buildinfo;` or `buildinfo` in the top-level group of a `use service_kit::{..}`.
fn imports_buildinfo(tokens: &[Token<'_>]) -> bool {
    (0..tokens.len()).any(|index| {
        let Some(rest) = tokens[index..].strip_prefix(&[Token::Ident("use")]) else {
            return false;
        };
        match rest.strip_prefix(&[COLON, COLON]).unwrap_or(rest) {
            [Token::Ident("service_kit"), COLON, COLON, rest @ ..] => match rest {
                [Token::Ident("buildinfo"), Token::Punct(b';'), ..] => true,
                [Token::Punct(b'{'), group @ ..] => group_names_buildinfo(group),
                _ => false,
            },
            _ => false,
        }
    })
}

/// Whether the group whose tokens follow its `{` names `buildinfo` at its top level.
fn group_names_buildinfo(group: &[Token<'_>]) -> bool {
    let mut depth = 1_usize;
    let mut previous = Token::Punct(b'{');
    for (index, token) in group.iter().enumerate() {
        match token {
            Token::Punct(b'{') => depth += 1,
            Token::Punct(b'}') => {
                depth -= 1;
                if depth == 0 {
                    return false;
                }
            }
            Token::Ident("buildinfo")
                if depth == 1
                    && matches!(previous, Token::Punct(b'{' | b','))
                    && matches!(group.get(index + 1), Some(Token::Punct(b',' | b'}'))) =>
            {
                return true;
            }
            Token::Ident(_) | Token::Punct(_) | Token::Str(_) | Token::Literal => {}
        }
        previous = *token;
    }
    false
}

/// Whether `tokens` define a `macro_rules! buildinfo` of their own, which a bare `buildinfo!`
/// would call instead of service-kit's.
fn defines_buildinfo(tokens: &[Token<'_>]) -> bool {
    tokens.windows(3).any(|window| {
        window
            == [
                Token::Ident("macro_rules"),
                Token::Punct(b'!'),
                Token::Ident("buildinfo"),
            ]
    })
}

/// The variables the version module `text` reads at compile time: each `option_env!("<NAME>")`
/// (bare, or in `core` or `std`), and, for each service-kit `buildinfo!("<PREFIX>", ..)`,
/// `<PREFIX>_<field>` for each of [`BUILDINFO_FIELDS`]. A `buildinfo!` is service-kit's when it is
/// called as `service_kit::buildinfo!`, or bare where the module imports it from `service_kit` and
/// defines no `macro_rules! buildinfo` of its own. Comments and string literals never count.
pub(super) fn stamp_reads(text: &str) -> BTreeSet<String> {
    let tokens = tokens(text);
    let bare_is_service_kit = imports_buildinfo(&tokens) && !defines_buildinfo(&tokens);
    let mut reads = BTreeSet::new();
    for index in 0..tokens.len() {
        match &tokens[index..] {
            [
                Token::Ident("option_env"),
                Token::Punct(b'!'),
                Token::Punct(b'('),
                Token::Str(name),
                rest @ ..,
            ] if matches!(
                rest,
                [Token::Punct(b')'), ..] | [Token::Punct(b','), Token::Punct(b')'), ..]
            ) && matches!(
                path_of(&tokens, index),
                Path::Bare | Path::In("core" | "std")
            ) =>
            {
                reads.insert((*name).to_owned());
            }
            [
                Token::Ident("buildinfo"),
                Token::Punct(b'!'),
                Token::Punct(b'(' | b'[' | b'{'),
                Token::Str(prefix),
                Token::Punct(b','),
                ..,
            ] => {
                let service_kit = match path_of(&tokens, index) {
                    Path::Bare => bare_is_service_kit,
                    Path::In(segment) => segment == "service_kit",
                    Path::Other => false,
                };
                if service_kit {
                    reads.extend(BUILDINFO_FIELDS.map(|field| format!("{prefix}_{field}")));
                }
            }
            _ => {}
        }
    }
    reads
}

#[cfg(test)]
mod tests;
