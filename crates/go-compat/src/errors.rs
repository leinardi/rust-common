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

//! Go's `errors.Join`, as its `Error` reads: the messages of the errors that are not nil, joined
//! with newlines, and nil when there are none.
//!
//! Extracted from swarm-device-access/src/go_compat/errors.rs at 8b4ab3a3e417.

/// The text of `errors.Join(errs...)`, `None` standing for a nil error.
#[must_use]
pub fn join<I>(messages: I) -> Option<String>
where
    I: IntoIterator<Item = Option<String>>,
{
    let present: Vec<String> = messages.into_iter().flatten().collect();
    (!present.is_empty()).then(|| present.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Go: `errors.Join(errors.New("a"), nil, errors.New("b"))` reads `a\nb`, and
    /// `errors.Join(nil, nil)` is nil.
    #[test]
    fn join_drops_nil_and_joins_with_newlines() {
        assert_eq!(
            join([Some("a".to_owned()), None, Some("b".to_owned())]).as_deref(),
            Some("a\nb")
        );
        assert_eq!(join([None, None]), None);
        assert_eq!(join([Some("only".to_owned())]).as_deref(), Some("only"));
    }
}
