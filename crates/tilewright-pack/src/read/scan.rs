// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Bounded scans of stored documents that build nothing: whether a text
//! can be canonical JSON at all, and how many items one array of its root
//! holds. They run before a document is parsed, so a document a read would
//! refuse costs no parse.

/// Whether `text` has none of the bytes canonical JSON never has outside
/// its strings: whitespace, and the `/` that starts a comment. A stored
/// document must be canonical, so one with them is refused unparsed.
pub(super) fn compact(text: &str) -> bool {
    let mut in_string = false;
    let mut escaped = false;
    for &byte in text.as_bytes() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
        } else if byte == b'"' {
            in_string = true;
        } else if matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | b'/') {
            return false;
        }
    }
    true
}

/// The index of the quote that closes the string opening at `open`.
fn string_end(bytes: &[u8], open: usize) -> Option<usize> {
    let mut index = open + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'"' => return Some(index),
            _ => index += 1,
        }
    }
    None
}

/// The items of the array opening at `open`, or `None` if it does not
/// close.
fn array_items(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth: usize = 0;
    let mut commas: usize = 0;
    let mut empty = true;
    let mut index = open;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                if depth == 1 {
                    empty = false;
                }
                index = string_end(bytes, index)?;
            }
            b'[' | b'{' => {
                if depth == 1 {
                    empty = false;
                }
                depth += 1;
            }
            b']' | b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(if empty { 0 } else { commas + 1 });
                }
            }
            b',' if depth == 1 => commas += 1,
            _ => {
                if depth == 1 {
                    empty = false;
                }
            }
        }
        index += 1;
    }
    None
}

/// The items of the array that is the root object's member `key`, in a
/// text that is [`compact`]: exact for canonical JSON, and for other text
/// `None` or some count, which the parse that follows judges.
pub(super) fn root_array_items(text: &str, key: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth: usize = 0;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                let end = string_end(bytes, index)?;
                let name = &bytes[index + 1..end];
                index = end + 1;
                // A member name of the root, not a string value, is
                // followed by a colon.
                if depth == 1 && bytes.get(index) == Some(&b':') && name == key.as_bytes() {
                    return match bytes.get(index + 1) {
                        Some(b'[') => array_items(bytes, index + 1),
                        _ => None,
                    };
                }
                continue;
            }
            b'{' | b'[' => depth += 1,
            b'}' | b']' => depth = depth.checked_sub(1)?,
            _ => {}
        }
        index += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{compact, root_array_items};

    #[test]
    fn canonical_text_is_compact_and_other_text_is_not() {
        assert!(compact(r#"{"a":"x y/z","b":[1,{"c":"\" /"}]}"#));
        for text in [
            r#"{"a": 1}"#,
            "{\"a\":1}\n",
            r#"{"a":1}//"#,
            r#"{"a":/*x*/1}"#,
            "\t{}",
        ] {
            assert!(!compact(text), "{text}");
        }
    }

    #[test]
    fn root_arrays_are_counted_without_parsing() {
        let steps = r#"{"id":"a","steps":[{"layer":0,"at":[0,0],"proto":"x,]"},{"layer":1,"at":[1,0],"proto":"y\"]"}],"direction":"up"}"#;
        assert_eq!(root_array_items(steps, "steps"), Some(2));
        assert_eq!(root_array_items(r#"{"steps":[]}"#, "steps"), Some(0));
        assert_eq!(
            root_array_items(r#"{"steps":[1,[2,3],"4,5"]}"#, "steps"),
            Some(3)
        );
        // A member of a nested object, a string value, or a member that is
        // not an array, is not the root's array.
        assert_eq!(
            root_array_items(r#"{"custom":{"x.org":{"steps":[1,2]}}}"#, "steps"),
            None
        );
        assert_eq!(root_array_items(r#"{"name":"steps","n":1}"#, "steps"), None);
        assert_eq!(root_array_items(r#"{"steps":3}"#, "steps"), None);
        assert_eq!(root_array_items(r#"{"steps":[1,2"#, "steps"), None);
        assert_eq!(root_array_items(r#"{"id":"a"}"#, "steps"), None);
    }
}
