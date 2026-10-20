// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Canonical layout for new text: two-space indentation, `"key": value`
//! members, and a container on one line when it is short or holds only
//! scalars that are not strings.

use tilewright_jsonc::{Document, Value, write_json_string};

/// Longest container written on one line, unless it holds only non-string
/// scalars.
pub(crate) const INLINE_WIDTH: usize = 80;

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

    fn write(&self, indent: &str, expand: bool, output: &mut String) {
        let mut inline = String::new();
        self.inline(&mut inline);
        let scalars = match self {
            Self::Array(items) => items.iter().all(|item| matches!(item, Self::Scalar(_))),
            Self::Object(members) => members.is_empty(),
            Self::Scalar(_) | Self::String(_) => true,
        };
        let empty = matches!(self, Self::Array(items) if items.is_empty())
            || matches!(self, Self::Object(members) if members.is_empty());
        if empty || scalars || (!expand && inline.len() <= INLINE_WIDTH) {
            output.push_str(&inline);
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
                    output.push_str(": ");
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
