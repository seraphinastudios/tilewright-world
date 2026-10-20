// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Canonical layout for new text: two-space indentation, `"key": value`
//! members, and a container on one line when it is short or holds only
//! scalars that are not strings.

use tilewright_jsonc::{Document, Value, write_json_string};

/// Longest container written on one line, unless it holds only non-string
/// scalars.
pub(crate) const INLINE_WIDTH: usize = 80;

/// Longest array of non-string scalars written on one line.
pub(crate) const SCALAR_LINE_WIDTH: usize = 4_096;

/// A value to write in canonical layout.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Node {
    /// A number, `true`, `false` or `null`, spelled as given.
    Scalar(String),
    String(String),
    Array(Vec<Node>),
    Object(Vec<(String, Node)>),
}

impl Node {
    /// A document value. Numbers keep their spelling.
    pub fn from_value(document: &Document, value: &Value) -> Self {
        match value {
            Value::Null { .. } => Self::Scalar("null".to_owned()),
            Value::Bool { value, .. } => Self::Scalar(value.to_string()),
            Value::Integer { span, .. } | Value::Float { span, .. } => {
                Self::Scalar(document.text(*span).to_owned())
            }
            Value::String { value, .. } => Self::String(value.clone()),
            Value::Array { items, .. } => Self::Array(
                items
                    .iter()
                    .map(|item| Self::from_value(document, item))
                    .collect(),
            ),
            Value::Object { members, .. } => Self::Object(
                members
                    .iter()
                    .map(|member| {
                        (
                            member.key.clone(),
                            Self::from_value(document, &member.value),
                        )
                    })
                    .collect(),
            ),
        }
    }

    pub fn string(text: &str) -> Self {
        Self::String(text.to_owned())
    }

    pub fn integer(value: i64) -> Self {
        Self::Scalar(value.to_string())
    }

    fn inline(&self, output: &mut String) {
        match self {
            Self::Scalar(text) => output.push_str(text),
            Self::String(text) => write_json_string(text, output),
            Self::Array(items) => {
                output.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        output.push_str(", ");
                    }
                    item.inline(output);
                }
                output.push(']');
            }
            Self::Object(members) => {
                output.push('{');
                for (index, (key, value)) in members.iter().enumerate() {
                    if index > 0 {
                        output.push_str(", ");
                    }
                    write_json_string(key, output);
                    output.push_str(": ");
                    value.inline(output);
                }
                output.push('}');
            }
        }
    }

    /// The value in canonical layout; continuation lines start with
    /// `indent`.
    pub fn layout(&self, indent: &str) -> String {
        let mut output = String::new();
        self.write(indent, false, &mut output);
        output
    }

    /// A whole file: the root on several lines and a final newline.
    pub fn file(&self) -> String {
        let mut output = String::new();
        self.write("", true, &mut output);
        output.push('\n');
        output
    }

    /// Whether the one-line form takes at most `room` bytes, which it
    /// reduces; counting stops as soon as it does not, so checking costs
    /// at most `room` whatever the value's size.
    fn fits(&self, room: &mut usize) -> bool {
        fn take(bytes: usize, room: &mut usize) -> bool {
            *room = match room.checked_sub(bytes) {
                Some(left) => left,
                None => return false,
            };
            true
        }
        fn string(text: &str, room: &mut usize) -> bool {
            take(2, room)
                && text.chars().all(|character| {
                    let mut escaped = String::new();
                    write_json_string(&character.to_string(), &mut escaped);
                    take(escaped.len() - 2, room)
                })
        }
        match self {
            Self::Scalar(text) => take(text.len(), room),
            Self::String(text) => string(text, room),
            Self::Array(items) => {
                take(2, room)
                    && items
                        .iter()
                        .enumerate()
                        .all(|(index, item)| (index == 0 || take(2, room)) && item.fits(room))
            }
            Self::Object(members) => {
                take(2, room)
                    && members.iter().enumerate().all(|(index, (key, value))| {
                        (index == 0 || take(2, room))
                            && string(key, room)
                            && take(2, room)
                            && value.fits(room)
                    })
            }
        }
    }

    fn write(&self, indent: &str, expand: bool, output: &mut String) {
        // An array of numbers, booleans and nulls stays on one line while
        // that line is not long; any other container while it is short.
        let one_line = match self {
            Self::Array(items) if items.iter().all(|item| matches!(item, Self::Scalar(_))) => {
                self.fits(&mut SCALAR_LINE_WIDTH.clone())
            }
            Self::Object(members) if members.is_empty() => true,
            Self::Scalar(_) | Self::String(_) => true,
            Self::Array(_) | Self::Object(_) => !expand && self.fits(&mut INLINE_WIDTH.clone()),
        };
        if one_line {
            self.inline(output);
            return;
        }
        let inner = format!("{indent}  ");
        match self {
            Self::Array(items) => {
                output.push_str("[\n");
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        output.push_str(",\n");
                    }
                    output.push_str(&inner);
                    item.write(&inner, false, output);
                }
                output.push('\n');
                output.push_str(indent);
                output.push(']');
            }
            Self::Object(members) => {
                output.push_str("{\n");
                for (index, (key, value)) in members.iter().enumerate() {
                    if index > 0 {
                        output.push_str(",\n");
                    }
                    output.push_str(&inner);
                    write_json_string(key, output);
                    // A long string goes on a line of its own.
                    if matches!(value, Self::String(_))
                        && !value.fits(&mut SCALAR_LINE_WIDTH.clone())
                    {
                        output.push_str(":\n");
                        output.push_str(&inner);
                        output.push_str("  ");
                    } else {
                        output.push_str(": ");
                    }
                    value.write(&inner, false, output);
                }
                output.push('\n');
                output.push_str(indent);
                output.push('}');
            }
            Self::Scalar(_) | Self::String(_) => {}
        }
    }
}

/// `text` as a JSON string.
pub(crate) fn json_string(text: &str) -> String {
    let mut output = String::new();
    write_json_string(text, &mut output);
    output
}
