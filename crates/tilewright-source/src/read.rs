// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Shape checks shared by every document reader. Each helper reports its
//! own diagnostic and returns `None` when the value is unusable.

use crate::{Code, Diagnostic, FieldValue, diagnostic::Positions, files::Source};
use std::{
    cell::{OnceCell, RefCell},
    collections::BTreeSet,
};
use tilewright_jsonc::{MAX_SAFE_INTEGER, Member, Span, Value};

/// Whether `text` matches `^[a-z0-9][a-z0-9_.-]{0,127}$`.
pub(crate) fn is_identifier(text: &str) -> bool {
    let bytes = text.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 128
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_.-".contains(byte))
}

/// Whether `text` is a domain-qualified namespace for `custom` data, such
/// as `example.org` or `example.org/tool`.
pub(crate) fn is_namespace(text: &str) -> bool {
    let (domain, rest) = text.split_once('/').unwrap_or((text, ""));
    domain.contains('.')
        && is_identifier(domain)
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && (rest.is_empty() || is_identifier(rest))
}

/// Reads one document. Diagnostics go to a shared list through a
/// `RefCell`, so helpers can nest (`reader.i32(reader.required(..)?)`).
pub(crate) struct Reader<'a> {
    pub source: &'a Source,
    diagnostics: RefCell<&'a mut Vec<Diagnostic>>,
    /// Built at the first diagnostic.
    positions: OnceCell<Positions<'a>>,
}

impl<'a> Reader<'a> {
    pub fn new(source: &'a Source, diagnostics: &'a mut Vec<Diagnostic>) -> Self {
        Self {
            source,
            diagnostics: RefCell::new(diagnostics),
            positions: OnceCell::new(),
        }
    }

    pub fn root(&self) -> &'a Value {
        self.source.document.root()
    }

    pub fn canonical(&self, value: &Value) -> String {
        self.source.document.canonical(value)
    }

    /// Builds a diagnostic at `span` without recording it.
    pub fn diagnostic(&self, span: Span, code: Code, message: impl Into<String>) -> Diagnostic {
        let positions = self
            .positions
            .get_or_init(|| Positions::new(self.source.document.source()));
        Diagnostic::at(
            &self.source.path,
            positions,
            span.start,
            span.end,
            code,
            message,
        )
    }

    pub fn report(&self, span: Span, code: Code, message: impl Into<String>) {
        let diagnostic = self.diagnostic(span, code, message);
        self.diagnostics.borrow_mut().push(diagnostic);
    }

    /// An object whose keys are all in `allowed`; unknown keys are reported.
    pub fn object(&self, value: &'a Value, allowed: &[&str]) -> Option<&'a Value> {
        let Some(members) = value.as_object() else {
            self.report(
                value.span(),
                Code::Type,
                format!("expected an object, found {}", value.kind()),
            );
            return None;
        };
        for member in members {
            if !allowed.contains(&member.key.as_str()) {
                self.report(
                    member.key_span,
                    Code::UnknownKey,
                    format!("unknown key \"{}\"", member.key),
                );
            }
        }
        Some(value)
    }

    /// The members of an open map: an object with any keys.
    pub fn members(&self, value: &'a Value) -> Option<&'a [Member]> {
        let members = value.as_object();
        if members.is_none() {
            self.report(
                value.span(),
                Code::Type,
                format!("expected an object, found {}", value.kind()),
            );
        }
        members
    }

    pub fn required(&self, object: &'a Value, key: &str) -> Option<&'a Value> {
        let value = object.get(key);
        if value.is_none() {
            self.report(
                object.span(),
                Code::MissingKey,
                format!("missing key \"{key}\""),
            );
        }
        value
    }

    /// Checks the document's `format` key.
    pub fn format(&self, root: &'a Value, expected: &str) -> bool {
        match self.required(root, "format") {
            Some(value) if value.as_str() == Some(expected) => true,
            Some(value) => {
                self.report(
                    value.span(),
                    Code::Format,
                    format!("format must be \"{expected}\""),
                );
                false
            }
            None => false,
        }
    }

    pub fn string(&self, value: &'a Value) -> Option<&'a str> {
        let text = value.as_str();
        if text.is_none() {
            self.report(
                value.span(),
                Code::Type,
                format!("expected a string, found {}", value.kind()),
            );
        }
        text
    }

    /// A string of at most `maximum` characters.
    pub fn text(&self, value: &'a Value, maximum: usize) -> Option<String> {
        let text = self.string(value)?;
        if text.chars().count() > maximum {
            self.report(
                value.span(),
                Code::Range,
                format!("longer than {maximum} characters"),
            );
            return None;
        }
        Some(text.to_owned())
    }

    pub fn identifier(&self, value: &'a Value) -> Option<String> {
        let text = self.string(value)?;
        if is_identifier(text) {
            Some(text.to_owned())
        } else {
            self.report(
                value.span(),
                Code::Identifier,
                format!("\"{text}\" is not a valid identifier"),
            );
            None
        }
    }

    pub fn boolean(&self, value: &'a Value) -> Option<bool> {
        let flag = value.as_bool();
        if flag.is_none() {
            self.report(
                value.span(),
                Code::Type,
                format!("expected a boolean, found {}", value.kind()),
            );
        }
        flag
    }

    /// An integer in `minimum..=maximum`.
    pub fn integer(&self, value: &'a Value, minimum: i64, maximum: i64) -> Option<i64> {
        let Some(number) = value.as_i64() else {
            self.report(
                value.span(),
                Code::Type,
                format!("expected an integer, found {}", value.kind()),
            );
            return None;
        };
        if (minimum..=maximum).contains(&number) {
            Some(number)
        } else {
            self.report(
                value.span(),
                Code::Range,
                format!("{number} is outside {minimum}..={maximum}"),
            );
            None
        }
    }

    pub fn i32(&self, value: &'a Value) -> Option<i32> {
        self.integer(value, i64::from(i32::MIN), i64::from(i32::MAX))
            .and_then(|number| i32::try_from(number).ok())
    }

    pub fn u32(&self, value: &'a Value, minimum: u32) -> Option<u32> {
        self.integer(value, i64::from(minimum), i64::from(u32::MAX))
            .and_then(|number| u32::try_from(number).ok())
    }

    pub fn array(&self, value: &'a Value) -> Option<&'a [Value]> {
        let items = value.as_array();
        if items.is_none() {
            self.report(
                value.span(),
                Code::Type,
                format!("expected an array, found {}", value.kind()),
            );
        }
        items
    }

    /// A pair of integers within the format's integer range.
    pub fn coordinates(&self, value: &'a Value) -> Option<[i64; 2]> {
        if let [x, y] = self.array(value)? {
            Some([
                self.integer(x, -MAX_SAFE_INTEGER, MAX_SAFE_INTEGER)?,
                self.integer(y, -MAX_SAFE_INTEGER, MAX_SAFE_INTEGER)?,
            ])
        } else {
            self.report(value.span(), Code::Type, "expected [x, y]");
            None
        }
    }

    /// A list of unique identifiers. Callers cap the list's length first.
    pub fn identifiers(&self, value: &'a Value) -> Option<Vec<String>> {
        let items = self.array(value)?;
        let mut seen = BTreeSet::new();
        let mut output = Vec::with_capacity(items.len());
        for item in items {
            let identifier = self.identifier(item)?;
            if !seen.insert(identifier.clone()) {
                self.report(
                    item.span(),
                    Code::Duplicate,
                    format!("\"{identifier}\" is listed twice"),
                );
                return None;
            }
            output.push(identifier);
        }
        Some(output)
    }

    /// A `custom` object whose keys are domain-qualified namespaces.
    pub fn custom(&self, object: &'a Value) -> Option<String> {
        let value = object.get("custom")?;
        let members = value.as_object();
        if members.is_none() {
            self.report(value.span(), Code::Type, "custom must be an object");
            return None;
        }
        for member in members.unwrap_or_default() {
            if !is_namespace(&member.key) {
                self.report(
                    member.key_span,
                    Code::Identifier,
                    format!(
                        "custom key \"{}\" must be a domain-qualified namespace",
                        member.key
                    ),
                );
            }
        }
        Some(self.canonical(value))
    }

    /// The typed form of a value as a field value.
    pub fn field_value(&self, value: &Value) -> FieldValue {
        match value {
            Value::Null { .. } => FieldValue::Null,
            Value::Bool { value, .. } => FieldValue::Bool(*value),
            Value::Integer { value, .. } => FieldValue::Integer(*value),
            Value::Float { value, .. } => FieldValue::Number(*value),
            Value::String { value, .. } => FieldValue::String(value.clone()),
            Value::Array { .. } | Value::Object { .. } => FieldValue::Json(self.canonical(value)),
        }
    }
}
