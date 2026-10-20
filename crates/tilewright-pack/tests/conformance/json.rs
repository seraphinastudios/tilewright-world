// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! A small JSON model for writing the manifest readably and stably.

use tilewright_jsonc::write_json_string;

#[derive(Clone, Debug, PartialEq)]
pub enum J {
    Null,
    Int(i64),
    Str(String),
    Array(Vec<J>),
    Object(Vec<(String, J)>),
}

impl From<&str> for J {
    fn from(value: &str) -> Self {
        Self::Str(value.to_owned())
    }
}

impl From<i64> for J {
    fn from(value: i64) -> Self {
        Self::Int(value)
    }
}

impl From<i32> for J {
    fn from(value: i32) -> Self {
        Self::Int(value.into())
    }
}

impl From<u32> for J {
    fn from(value: u32) -> Self {
        Self::Int(value.into())
    }
}

impl From<u64> for J {
    fn from(value: u64) -> Self {
        Self::Int(i64::try_from(value).expect("manifest numbers fit in i64"))
    }
}

/// Members longer than this when written on one line are written one per
/// line.
const INLINE_WIDTH: usize = 100;

impl J {
    pub fn object<K: Into<String>>(members: impl IntoIterator<Item = (K, J)>) -> Self {
        Self::Object(
            members
                .into_iter()
                .map(|(key, value)| (key.into(), value))
                .collect(),
        )
    }

    fn scalar(&self) -> bool {
        !matches!(self, Self::Array(_) | Self::Object(_))
    }

    /// The value as pretty-printed JSON with a final newline.
    pub fn pretty(&self) -> String {
        let mut output = String::new();
        self.write(0, &mut output);
        output.push('\n');
        output
    }

    fn inline(&self, output: &mut String) {
        match self {
            Self::Null => output.push_str("null"),
            Self::Int(value) => output.push_str(&value.to_string()),
            Self::Str(value) => write_json_string(value, output),
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

    fn write(&self, indent: usize, output: &mut String) {
        let flat = match self {
            Self::Array(items) => items.iter().all(Self::scalar),
            Self::Object(members) => members.iter().all(|(_, value)| value.scalar()),
            _ => true,
        };
        if flat {
            let mut line = String::new();
            self.inline(&mut line);
            if self.scalar() || line.len() <= INLINE_WIDTH {
                output.push_str(&line);
                return;
            }
        }
        let inner = " ".repeat(indent + 2);
        match self {
            Self::Array(items) => {
                output.push_str("[\n");
                for (index, item) in items.iter().enumerate() {
                    output.push_str(&inner);
                    item.write(indent + 2, output);
                    output.push_str(if index + 1 < items.len() { ",\n" } else { "\n" });
                }
                output.push_str(&" ".repeat(indent));
                output.push(']');
            }
            Self::Object(members) => {
                output.push_str("{\n");
                for (index, (key, value)) in members.iter().enumerate() {
                    output.push_str(&inner);
                    write_json_string(key, output);
                    output.push_str(": ");
                    value.write(indent + 2, output);
                    output.push_str(if index + 1 < members.len() {
                        ",\n"
                    } else {
                        "\n"
                    });
                }
                output.push_str(&" ".repeat(indent));
                output.push('}');
            }
            _ => self.inline(output),
        }
    }
}
