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

//! The plain format of the Go programs these crates replace, a handler of their own beside
//! slog's:
//!
//! ```text
//! level=INFO hello world k=v foo=bar
//! ```
//!
//! with `time=<RFC3339Nano>` first when the time is logged. Two Go plain handlers exist, which
//! differ in how they write text, so the [`PlainStyle`] picks one; each is checked byte for byte
//! against its Go handler's goldens (`tests/testdata/{quoted,raw}/plain_*.log`).
//!
//! [`PlainStyle::Quoted`]: one record is always one line, and nothing in a message, key or value
//! can forge a field:
//!
//! - the message is escaped (`\n`, `\\`, `\xNN`, `\uNNNN`);
//! - a key, and a string, error, stringer or `[]string` value, is written raw when it is plainly
//!   safe (a key of `[A-Za-z0-9_.-]`; a value of printable characters other than space, `"`, `=`,
//!   `\`, `{` and `}`) and quoted with `strconv.Quote` otherwise, the empty string included;
//! - the empty `Attr{}` and a group without a key are dropped; any other attribute without a key
//!   is written with the key `""`.
//!
//! [`PlainStyle::Raw`]: the text is written as `fmt.Fprint` gives it:
//!
//! - the message and every key are written raw, a newline included;
//! - a string value is quoted only when it holds a space or a tab, and then only `"` is escaped;
//! - an error, a stringer, a `[]string` and nil are written raw;
//! - an attribute whose key is empty after qualification is dropped, a group included.
//!
//! Both keep the Go handlers' shared quirks: a level that is not exactly info, warn or error reads
//! `ERROR` above debug; attributes added by `with_attrs` are qualified with every group, those
//! opened after them included; and a group with no attributes, from `with_attrs`, is written
//! `key={}`.
//!
//! Extracted from swarm-scheduler-exporter/src/logger/plain_handler.rs at c9e79afa8810 (the
//! quoted style) and swarm-device-access/src/logger/plain_handler.rs at 8b4ab3a3e417 (the raw
//! style).

use go_compat::{duration, float, quote, time_layout};

use crate::Level;
use crate::value::{Attr, Time, Value};

/// How the plain format writes a message, a key and a text value: the two Go plain handlers.
/// There is no default: each program names the one its Go original had.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlainStyle {
    /// One record is one line: the message is escaped, and a key or a text value that is not
    /// plainly safe is quoted with `strconv.Quote`.
    Quoted,
    /// The message and the keys raw, newlines included; a string quoted only for a space or a
    /// tab, with only `"` escaped; errors, stringers and `[]string`s raw; attributes without a
    /// key dropped.
    Raw,
}

/// The plain handler: the style, the attributes from `with_attrs` and the groups from
/// `with_group`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PlainHandler {
    style: PlainStyle,
    prefix_attrs: Vec<Attr>,
    groups: Vec<String>,
}

impl PlainHandler {
    /// A handler in `style`, with no attributes and no groups.
    pub(crate) fn new(style: PlainStyle) -> Self {
        Self {
            style,
            prefix_attrs: Vec::new(),
            groups: Vec::new(),
        }
    }

    /// `WithAttrs`, copy on write; no attributes change nothing.
    pub(crate) fn with_attrs(&self, attrs: &[Attr]) -> Self {
        let mut next = self.clone();
        next.prefix_attrs.extend_from_slice(attrs);
        next
    }

    /// `WithGroup`: the name is trimmed, and a blank one changes nothing.
    pub(crate) fn with_group(&self, name: &str) -> Self {
        let name = name.trim();
        let mut next = self.clone();
        if !name.is_empty() {
            next.groups.push(name.to_owned());
        }
        next
    }

    /// `Handle`: one record, newline included. `time` is `None` when the time is not logged, or
    /// is Go's zero time.
    pub(crate) fn format(
        &self,
        time: Option<Time>,
        level: Level,
        message: &str,
        attrs: &[Attr],
    ) -> String {
        let mut buffer = String::with_capacity(128);
        if let Some(time) = time {
            buffer.push_str("time=");
            time_layout::push_rfc3339_nano(&mut buffer, time.timestamp, time.offset);
            buffer.push(' ');
        }
        buffer.push_str("level=");
        buffer.push_str(level_to_upper(level));
        if !message.is_empty() {
            buffer.push(' ');
            match self.style {
                PlainStyle::Quoted => push_escaped_unquoted(&mut buffer, message),
                PlainStyle::Raw => buffer.push_str(message),
            }
        }
        for attr in self.prefix_attrs.iter().chain(attrs) {
            let attr = qualify(&self.groups, attr);
            match self.style {
                PlainStyle::Quoted => quoted::write_attr_kv(&mut buffer, &attr),
                PlainStyle::Raw => raw::write_attr_kv(&mut buffer, &attr),
            }
        }
        buffer.push('\n');
        buffer
    }
}

/// `levelToUpper`.
pub(crate) fn level_to_upper(level: Level) -> &'static str {
    if level <= Level::DEBUG {
        "DEBUG"
    } else if level == Level::INFO {
        "INFO"
    } else if level == Level::WARN {
        "WARN"
    } else {
        "ERROR"
    }
}

/// `qualify`: the key under the nested group prefixes (`g1.g2.key`).
pub(crate) fn qualify(groups: &[String], attr: &Attr) -> Attr {
    if groups.is_empty() {
        return attr.clone();
    }
    let mut key = groups.join(".");
    if !attr.key.is_empty() {
        key.push('.');
        key.push_str(&attr.key);
    }
    Attr {
        key,
        value: attr.value.clone(),
    }
}

/// The key of a group's child: `parent.child`, or the parent's for a child without a key.
fn child_key(parent_key: &str, child: &Attr) -> String {
    let mut key = parent_key.to_owned();
    if !child.key.is_empty() {
        key.push('.');
        key.push_str(&child.key);
    }
    key
}

/// The values both styles write the same way.
fn write_scalar(buffer: &mut String, value: &Value) {
    match value {
        Value::Int(number) => buffer.push_str(&number.to_string()),
        Value::Uint(number) => buffer.push_str(&number.to_string()),
        Value::Float(number) => float::push_g(buffer, *number),
        Value::Bool(flag) => buffer.push_str(if *flag { "true" } else { "false" }),
        Value::Time(time) => {
            if time_layout::is_go_zero_time(time.timestamp) {
                buffer.push('0');
            } else {
                time_layout::push_rfc3339_nano(buffer, time.timestamp, time.offset);
            }
        }
        Value::Duration(nanos) => duration::push(buffer, *nanos),
        // The text values and groups are each style's own.
        Value::Str(_)
        | Value::Strings(_)
        | Value::NilStrings
        | Value::Stringer(_)
        | Value::Error(_)
        | Value::Nil
        | Value::Group(_) => {}
    }
}

/// [`PlainStyle::Quoted`].
mod quoted {
    use go_compat::quote;

    use super::{child_key, write_scalar};
    use crate::value::{Attr, Value};

    /// `skipAttr`: the empty Attr, and a group without a key, are not written at all.
    fn skip_attr(attr: &Attr) -> bool {
        attr.is_empty() || (attr.key.is_empty() && matches!(attr.value, Value::Group(_)))
    }

    /// `writeAttrKV`: ` key=value`.
    pub(super) fn write_attr_kv(buffer: &mut String, attr: &Attr) {
        if skip_attr(attr) {
            return;
        }
        buffer.push(' ');
        write_kv(buffer, &attr.key, &attr.value);
    }

    /// `writeKV` without the leading space.
    fn write_kv(buffer: &mut String, key: &str, value: &Value) {
        if is_raw_key(key) {
            buffer.push_str(key);
        } else {
            quote::push_quoted(buffer, key);
        }
        buffer.push('=');
        match value {
            Value::Group(children) => write_group_braced(buffer, key, children),
            Value::Str(text) | Value::Error(text) | Value::Stringer(text) => {
                write_text_value(buffer, text);
            }
            // fmt.Sprint of a []string, of a nil one, and of nil.
            Value::Strings(items) => write_text_value(buffer, &format!("[{}]", items.join(" "))),
            Value::NilStrings => write_text_value(buffer, "[]"),
            Value::Nil => write_text_value(buffer, "<nil>"),
            Value::Int(_)
            | Value::Uint(_)
            | Value::Float(_)
            | Value::Bool(_)
            | Value::Time(_)
            | Value::Duration(_) => write_scalar(buffer, value),
        }
    }

    /// `writeGroupBraced`: `key={key.a=1 key.b="two"}`, the children's keys under the parent's.
    fn write_group_braced(buffer: &mut String, parent_key: &str, children: &[Attr]) {
        buffer.push('{');
        for (index, child) in children.iter().enumerate() {
            if index > 0 {
                buffer.push(' ');
            }
            let qualified = Attr {
                key: child_key(parent_key, child),
                value: child.value.clone(),
            };
            if !skip_attr(&qualified) {
                write_kv(buffer, &qualified.key, &qualified.value);
            }
        }
        buffer.push('}');
    }

    /// `isRawKey`: non-empty and made of `[A-Za-z0-9_.-]`.
    fn is_raw_key(key: &str) -> bool {
        !key.is_empty()
            && key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
    }

    /// `writeTextValue`: raw when unambiguous, quoted otherwise.
    fn write_text_value(buffer: &mut String, text: &str) {
        if is_raw_value(text) {
            buffer.push_str(text);
        } else {
            quote::push_quoted(buffer, text);
        }
    }

    /// `isRawValue`: non-empty, every character printable, and none of the delimiters (space,
    /// quote, equals, backslash, braces).
    fn is_raw_value(text: &str) -> bool {
        !text.is_empty()
            && text
                .chars()
                .all(|character| quote::is_print(character) && !" \"=\\{}".contains(character))
    }
}

/// [`PlainStyle::Raw`].
mod raw {
    use super::{child_key, write_scalar};
    use crate::value::{Attr, Value};

    /// `writeAttrKV`: ` key=value`, or nothing for an attribute without a key (the empty
    /// `Attr{}` included).
    pub(super) fn write_attr_kv(buffer: &mut String, attr: &Attr) {
        if attr.key.is_empty() {
            return;
        }
        buffer.push(' ');
        write_kv(buffer, &attr.key, &attr.value);
    }

    /// `writeKV` without the leading space: the key raw, `=`, then the value.
    fn write_kv(buffer: &mut String, key: &str, value: &Value) {
        buffer.push_str(key);
        buffer.push('=');
        match value {
            Value::Group(children) => write_group_braced(buffer, key, children),
            Value::Str(text) => write_string_value(buffer, text),
            // fmt.Fprint of an error, a stringer, a []string (nil or not) and nil.
            Value::Error(text) | Value::Stringer(text) => buffer.push_str(text),
            Value::Strings(items) => {
                buffer.push('[');
                buffer.push_str(&items.join(" "));
                buffer.push(']');
            }
            Value::NilStrings => buffer.push_str("[]"),
            Value::Nil => buffer.push_str("<nil>"),
            Value::Int(_)
            | Value::Uint(_)
            | Value::Float(_)
            | Value::Bool(_)
            | Value::Time(_)
            | Value::Duration(_) => write_scalar(buffer, value),
        }
    }

    /// `writeGroupBraced`: `key={key.a=1 key.b="two"}`, the children's keys under the parent's,
    /// and `key={}` for a group without attributes. A child's qualified key is never empty, so
    /// every child is written.
    fn write_group_braced(buffer: &mut String, parent_key: &str, children: &[Attr]) {
        buffer.push('{');
        for (index, child) in children.iter().enumerate() {
            if index > 0 {
                buffer.push(' ');
            }
            write_kv(buffer, &child_key(parent_key, child), &child.value);
        }
        buffer.push('}');
    }

    /// `writeScalarValue` for a string: quoted when it holds a space or a tab, with only `"`
    /// escaped; raw otherwise, the empty string included.
    fn write_string_value(buffer: &mut String, text: &str) {
        if text.contains([' ', '\t']) {
            buffer.push('"');
            buffer.push_str(&text.replace('"', "\\\""));
            buffer.push('"');
        } else {
            buffer.push_str(text);
        }
    }
}

/// `escapeUnquoted`, for the message of [`PlainStyle::Quoted`]: a backslash is doubled, a
/// printable character is written as is, `\n`, `\r` and `\t` are escaped by name, and any other
/// character as `\xNN` below 0x80, else `\uNNNN` or `\UNNNNNNNN`.
fn push_escaped_unquoted(buffer: &mut String, text: &str) {
    for character in text.chars() {
        match character {
            '\\' => buffer.push_str("\\\\"),
            _ if quote::is_print(character) => buffer.push(character),
            '\n' => buffer.push_str("\\n"),
            '\r' => buffer.push_str("\\r"),
            '\t' => buffer.push_str("\\t"),
            _ if character.is_ascii() => {
                quote::push_hex_escape(buffer, 'x', u32::from(character), 2);
            }
            _ if u32::from(character) <= 0xFFFF => {
                quote::push_hex_escape(buffer, 'u', u32::from(character), 4);
            }
            _ => quote::push_hex_escape(buffer, 'U', u32::from(character), 8),
        }
    }
}
