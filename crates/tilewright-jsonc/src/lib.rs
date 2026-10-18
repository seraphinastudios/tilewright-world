// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Strict, lossless, bounded JSONC for Tilewright world sources.
//!
//! The dialect is UTF-8 JSON (RFC 8259) plus `//` and `/* */` comments, as
//! specified in `docs/spec/FORMAT.md` section 3. Trailing commas, duplicate
//! keys, byte-order marks, non-finite numbers and out-of-range integers are
//! errors. Every limit is enforced while reading.
//!
//! A [`Document`] keeps its source text unchanged. Every [`Value`] records the
//! byte [`Span`] it occupies, so tools can replace exactly one value and keep
//! comments, ordering and whitespace byte for byte.
//!
//! ```
//! use tilewright_jsonc::{Document, Limits};
//!
//! let source = "{\n  // terrain\n  \"fill\": 0\n}\n";
//! let document = Document::parse(source, &Limits::default()).unwrap();
//! let fill = document.root().get("fill").unwrap();
//! assert_eq!(fill.as_i64(), Some(0));
//! let edited = document.replace(fill.span(), "4", &Limits::default()).unwrap();
//! assert_eq!(edited.source(), "{\n  // terrain\n  \"fill\": 4\n}\n");
//! ```

use std::{collections::HashSet, fmt};

/// Largest integer magnitude accepted, `2^53 - 1`.
pub const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// Parser limits. [`Limits::default`] is the format's fixed contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub max_input_bytes: usize,
    pub max_depth: usize,
    pub max_values: usize,
    pub max_array_items: usize,
    pub max_object_keys: usize,
    pub max_string_bytes: usize,
    pub max_number_bytes: usize,
    pub max_comments: usize,
    pub max_comment_bytes: usize,
    pub max_lines: usize,
    pub max_line_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 64 * 1024 * 1024,
            max_depth: 64,
            max_values: 1_000_000,
            max_array_items: 250_000,
            max_object_keys: 256,
            max_string_bytes: 1024 * 1024,
            max_number_bytes: 128,
            max_comments: 100_000,
            max_comment_bytes: 4 * 1024 * 1024,
            max_lines: 250_000,
            max_line_bytes: 1024 * 1024,
        }
    }
}

/// A half-open byte range `[start, end)` in the document source.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    #[must_use]
    pub const fn len(self) -> usize {
        self.end - self.start
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.start == self.end
    }
}

/// A parsed value with its exact source span.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null { span: Span },
    Bool { value: bool, span: Span },
    Integer { value: i64, span: Span },
    Float { value: f64, span: Span },
    String { value: String, span: Span },
    Array { items: Vec<Value>, span: Span },
    Object { members: Vec<Member>, span: Span },
}

/// One object member. Members keep their source order.
#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    pub key: String,
    pub key_span: Span,
    pub value: Value,
}

/// A step in a [`Value::pointer`] path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Segment<'a> {
    Key(&'a str),
    Index(usize),
}

impl Value {
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::Null { span }
            | Self::Bool { span, .. }
            | Self::Integer { span, .. }
            | Self::Float { span, .. }
            | Self::String { span, .. }
            | Self::Array { span, .. }
            | Self::Object { span, .. } => *span,
        }
    }

    /// A short name for diagnostics: `null`, `boolean`, `integer`, `number`,
    /// `string`, `array` or `object`.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Null { .. } => "null",
            Self::Bool { .. } => "boolean",
            Self::Integer { .. } => "integer",
            Self::Float { .. } => "number",
            Self::String { .. } => "string",
            Self::Array { .. } => "array",
            Self::Object { .. } => "object",
        }
    }

    #[must_use]
    pub const fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool { value, .. } => Some(*value),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Integer { value, .. } => Some(*value),
            _ => None,
        }
    }

    /// The numeric value of an integer or float.
    #[must_use]
    #[allow(clippy::cast_precision_loss)]
    pub const fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Integer { value, .. } => Some(*value as f64),
            Self::Float { value, .. } => Some(*value),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String { value, .. } => Some(value),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Self::Array { items, .. } => Some(items),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_object(&self) -> Option<&[Member]> {
        match self {
            Self::Object { members, .. } => Some(members),
            _ => None,
        }
    }

    /// The value of an object member, if this is an object that has it.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_object()?
            .iter()
            .find(|member| member.key == key)
            .map(|member| &member.value)
    }

    /// The object member itself, including its key span.
    #[must_use]
    pub fn member(&self, key: &str) -> Option<&Member> {
        self.as_object()?.iter().find(|member| member.key == key)
    }

    /// Follows a path of keys and indexes.
    #[must_use]
    pub fn pointer(&self, path: &[Segment<'_>]) -> Option<&Value> {
        path.iter().try_fold(self, |value, segment| match segment {
            Segment::Key(key) => value.get(key),
            Segment::Index(index) => value.as_array()?.get(*index),
        })
    }
}

/// A parsed document that owns its unchanged source text.
#[derive(Clone, Debug, PartialEq)]
pub struct Document {
    source: String,
    root: Value,
}

impl Document {
    /// Parses a complete document.
    ///
    /// # Errors
    /// Returns the first syntax or limit violation with its byte offset.
    pub fn parse(source: impl Into<String>, limits: &Limits) -> Result<Self, Error> {
        let source = source.into();
        let root = parse_value_tree(&source, limits)?;
        Ok(Self { source, root })
    }

    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    #[must_use]
    pub const fn root(&self) -> &Value {
        &self.root
    }

    /// The exact source text of a span.
    ///
    /// # Panics
    /// Panics if the span is not a span of this document.
    #[must_use]
    pub fn text(&self, span: Span) -> &str {
        &self.source[span.start..span.end]
    }

    /// Canonical minified JSON for a value of this document: no comments or
    /// whitespace, members in source order, numbers spelled exactly as in the
    /// source, and strings escaped minimally (`"`, `\\` and control
    /// characters only). Equal values from equal source text always give
    /// equal output.
    ///
    /// # Panics
    /// Panics if the value is not part of this document.
    #[must_use]
    pub fn canonical(&self, value: &Value) -> String {
        let mut output = String::new();
        self.write_canonical(value, &mut output);
        output
    }

    fn write_canonical(&self, value: &Value, output: &mut String) {
        match value {
            Value::Null { .. } => output.push_str("null"),
            Value::Bool { value, .. } => output.push_str(if *value { "true" } else { "false" }),
            Value::Integer { span, .. } | Value::Float { span, .. } => {
                output.push_str(self.text(*span));
            }
            Value::String { value, .. } => write_json_string(value, output),
            Value::Array { items, .. } => {
                output.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        output.push(',');
                    }
                    self.write_canonical(item, output);
                }
                output.push(']');
            }
            Value::Object { members, .. } => {
                output.push('{');
                for (index, member) in members.iter().enumerate() {
                    if index > 0 {
                        output.push(',');
                    }
                    write_json_string(&member.key, output);
                    output.push(':');
                    self.write_canonical(&member.value, output);
                }
                output.push('}');
            }
        }
    }

    /// Replaces one span with new text and parses the result. Every byte
    /// outside the span is preserved.
    ///
    /// # Errors
    /// Returns [`ErrorCode::InvalidEdit`] for a span outside the document or
    /// not on character boundaries, or any error of the edited document.
    pub fn replace(&self, span: Span, text: &str, limits: &Limits) -> Result<Self, Error> {
        if span.start > span.end
            || span.end > self.source.len()
            || !self.source.is_char_boundary(span.start)
            || !self.source.is_char_boundary(span.end)
        {
            return Err(Error {
                code: ErrorCode::InvalidEdit,
                offset: span.start.min(self.source.len()),
                line: 0,
                column: 0,
            });
        }
        let mut edited = String::with_capacity(self.source.len() - span.len() + text.len());
        edited.push_str(&self.source[..span.start]);
        edited.push_str(text);
        edited.push_str(&self.source[span.end..]);
        Self::parse(edited, limits)
    }
}

/// Stable error codes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ErrorCode {
    InputTooLarge,
    TooManyLines,
    LineTooLong,
    ByteOrderMark,
    UnexpectedEnd,
    UnexpectedCharacter,
    ExpectedValue,
    ExpectedKey,
    ExpectedColon,
    ExpectedCommaOrEnd,
    TrailingComma,
    TrailingContent,
    DuplicateKey,
    InvalidNumber,
    NumberTooLong,
    IntegerOutOfRange,
    NonFiniteNumber,
    UnterminatedString,
    ControlCharacterInString,
    InvalidEscape,
    InvalidUnicodeEscape,
    NulInString,
    StringTooLong,
    UnterminatedComment,
    TooManyComments,
    DepthLimit,
    ValueLimit,
    ArrayLimit,
    ObjectLimit,
    InvalidEdit,
}

impl ErrorCode {
    /// The stable string form, for diagnostics and tests.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InputTooLarge => "jsonc.input_too_large",
            Self::TooManyLines => "jsonc.too_many_lines",
            Self::LineTooLong => "jsonc.line_too_long",
            Self::ByteOrderMark => "jsonc.byte_order_mark",
            Self::UnexpectedEnd => "jsonc.unexpected_end",
            Self::UnexpectedCharacter => "jsonc.unexpected_character",
            Self::ExpectedValue => "jsonc.expected_value",
            Self::ExpectedKey => "jsonc.expected_key",
            Self::ExpectedColon => "jsonc.expected_colon",
            Self::ExpectedCommaOrEnd => "jsonc.expected_comma_or_end",
            Self::TrailingComma => "jsonc.trailing_comma",
            Self::TrailingContent => "jsonc.trailing_content",
            Self::DuplicateKey => "jsonc.duplicate_key",
            Self::InvalidNumber => "jsonc.invalid_number",
            Self::NumberTooLong => "jsonc.number_too_long",
            Self::IntegerOutOfRange => "jsonc.integer_out_of_range",
            Self::NonFiniteNumber => "jsonc.non_finite_number",
            Self::UnterminatedString => "jsonc.unterminated_string",
            Self::ControlCharacterInString => "jsonc.control_character_in_string",
            Self::InvalidEscape => "jsonc.invalid_escape",
            Self::InvalidUnicodeEscape => "jsonc.invalid_unicode_escape",
            Self::NulInString => "jsonc.nul_in_string",
            Self::StringTooLong => "jsonc.string_too_long",
            Self::UnterminatedComment => "jsonc.unterminated_comment",
            Self::TooManyComments => "jsonc.too_many_comments",
            Self::DepthLimit => "jsonc.depth_limit",
            Self::ValueLimit => "jsonc.value_limit",
            Self::ArrayLimit => "jsonc.array_limit",
            Self::ObjectLimit => "jsonc.object_limit",
            Self::InvalidEdit => "jsonc.invalid_edit",
        }
    }
}

/// A parse error. `line` and `column` are 1-based (column counts characters);
/// both are 0 when no position applies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Error {
    pub code: ErrorCode,
    pub offset: usize,
    pub line: usize,
    pub column: usize,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            write!(formatter, "{} at byte {}", self.code.as_str(), self.offset)
        } else {
            write!(
                formatter,
                "{} at line {}, column {}",
                self.code.as_str(),
                self.line,
                self.column
            )
        }
    }
}

impl std::error::Error for Error {}

/// Appends `value` as a JSON string with minimal escaping.
pub fn write_json_string(value: &str, output: &mut String) {
    use std::fmt::Write as _;
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '\u{8}' => output.push_str("\\b"),
            '\u{c}' => output.push_str("\\f"),
            control if u32::from(control) < 0x20 => {
                let _ = write!(output, "\\u{:04x}", u32::from(control));
            }
            other => output.push(other),
        }
    }
    output.push('"');
}

fn parse_value_tree(source: &str, limits: &Limits) -> Result<Value, Error> {
    let error_at = |code, offset| Err(locate(source, code, offset));
    if source.len() > limits.max_input_bytes {
        return error_at(ErrorCode::InputTooLarge, 0);
    }
    if source.starts_with('\u{feff}') {
        return error_at(ErrorCode::ByteOrderMark, 0);
    }
    let mut lines = 1_usize;
    let mut line_start = 0_usize;
    for (offset, byte) in source.bytes().enumerate() {
        if byte == b'\n' {
            if offset - line_start > limits.max_line_bytes {
                return error_at(ErrorCode::LineTooLong, line_start);
            }
            lines += 1;
            line_start = offset + 1;
            if lines > limits.max_lines {
                return error_at(ErrorCode::TooManyLines, offset);
            }
        }
    }
    if source.len() - line_start > limits.max_line_bytes {
        return error_at(ErrorCode::LineTooLong, line_start);
    }
    let mut parser = Parser {
        source,
        bytes: source.as_bytes(),
        position: 0,
        limits,
        values: 0,
        comments: 0,
        comment_bytes: 0,
    };
    parser
        .document()
        .map_err(|(code, offset)| locate(source, code, offset))
}

fn locate(source: &str, code: ErrorCode, offset: usize) -> Error {
    let offset = offset.min(source.len());
    let mut boundary = offset;
    while !source.is_char_boundary(boundary) {
        boundary -= 1;
    }
    let before = &source[..boundary];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let column = source[line_start..boundary].chars().count() + 1;
    Error {
        code,
        offset,
        line,
        column,
    }
}

type Failure = (ErrorCode, usize);

struct Parser<'a> {
    source: &'a str,
    bytes: &'a [u8],
    position: usize,
    limits: &'a Limits,
    values: usize,
    comments: usize,
    comment_bytes: usize,
}

impl Parser<'_> {
    fn document(&mut self) -> Result<Value, Failure> {
        self.trivia()?;
        if self.position == self.bytes.len() {
            return Err((ErrorCode::ExpectedValue, self.position));
        }
        let root = self.value(0)?;
        self.trivia()?;
        if self.position != self.bytes.len() {
            return Err((ErrorCode::TrailingContent, self.position));
        }
        Ok(root)
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn trivia(&mut self) -> Result<(), Failure> {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\n' | b'\r') => self.position += 1,
                Some(b'/') => self.comment()?,
                _ => return Ok(()),
            }
        }
    }

    fn comment(&mut self) -> Result<(), Failure> {
        let start = self.position;
        match self.bytes.get(start + 1) {
            Some(b'/') => {
                let end = self.bytes[start..]
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or(self.bytes.len(), |index| start + index);
                self.position = end;
            }
            Some(b'*') => {
                let body = start + 2;
                let close = self.source[body..]
                    .find("*/")
                    .ok_or((ErrorCode::UnterminatedComment, start))?;
                self.position = body + close + 2;
            }
            _ => return Err((ErrorCode::UnexpectedCharacter, start)),
        }
        self.comments += 1;
        self.comment_bytes += self.position - start;
        if self.comments > self.limits.max_comments
            || self.comment_bytes > self.limits.max_comment_bytes
        {
            return Err((ErrorCode::TooManyComments, start));
        }
        Ok(())
    }

    fn count_value(&mut self, offset: usize) -> Result<(), Failure> {
        self.values += 1;
        if self.values > self.limits.max_values {
            return Err((ErrorCode::ValueLimit, offset));
        }
        Ok(())
    }

    fn value(&mut self, depth: usize) -> Result<Value, Failure> {
        let start = self.position;
        self.count_value(start)?;
        match self.peek() {
            None => Err((ErrorCode::UnexpectedEnd, start)),
            Some(b'{') => self.object(depth + 1),
            Some(b'[') => self.array(depth + 1),
            Some(b'"') => {
                let value = self.string()?;
                Ok(Value::String {
                    value,
                    span: Span {
                        start,
                        end: self.position,
                    },
                })
            }
            Some(b't') => self.literal(
                "true",
                Value::Bool {
                    value: true,
                    span: Span {
                        start,
                        end: start + 4,
                    },
                },
            ),
            Some(b'f') => self.literal(
                "false",
                Value::Bool {
                    value: false,
                    span: Span {
                        start,
                        end: start + 5,
                    },
                },
            ),
            Some(b'n') => self.literal(
                "null",
                Value::Null {
                    span: Span {
                        start,
                        end: start + 4,
                    },
                },
            ),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err((ErrorCode::ExpectedValue, start)),
        }
    }

    fn literal(&mut self, word: &str, value: Value) -> Result<Value, Failure> {
        if self.bytes[self.position..].starts_with(word.as_bytes()) {
            self.position += word.len();
            Ok(value)
        } else {
            Err((ErrorCode::ExpectedValue, self.position))
        }
    }

    fn number(&mut self) -> Result<Value, Failure> {
        let start = self.position;
        let mut cursor = start;
        let digit = |byte: Option<&u8>| matches!(byte, Some(b'0'..=b'9'));
        if self.bytes.get(cursor) == Some(&b'-') {
            cursor += 1;
        }
        match self.bytes.get(cursor) {
            Some(b'0') => cursor += 1,
            Some(b'1'..=b'9') => {
                while digit(self.bytes.get(cursor)) {
                    cursor += 1;
                }
            }
            _ => return Err((ErrorCode::InvalidNumber, start)),
        }
        let mut integer = true;
        if self.bytes.get(cursor) == Some(&b'.') {
            integer = false;
            cursor += 1;
            if !digit(self.bytes.get(cursor)) {
                return Err((ErrorCode::InvalidNumber, start));
            }
            while digit(self.bytes.get(cursor)) {
                cursor += 1;
            }
        }
        if matches!(self.bytes.get(cursor), Some(b'e' | b'E')) {
            integer = false;
            cursor += 1;
            if matches!(self.bytes.get(cursor), Some(b'+' | b'-')) {
                cursor += 1;
            }
            if !digit(self.bytes.get(cursor)) {
                return Err((ErrorCode::InvalidNumber, start));
            }
            while digit(self.bytes.get(cursor)) {
                cursor += 1;
            }
        }
        if matches!(
            self.bytes.get(cursor),
            Some(b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-' | b'x' | b'X')
        ) {
            return Err((ErrorCode::InvalidNumber, start));
        }
        if cursor - start > self.limits.max_number_bytes {
            return Err((ErrorCode::NumberTooLong, start));
        }
        let text = &self.source[start..cursor];
        let span = Span { start, end: cursor };
        self.position = cursor;
        if integer {
            let value = text
                .parse::<i64>()
                .map_err(|_| (ErrorCode::IntegerOutOfRange, start))?;
            if !(-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&value) {
                return Err((ErrorCode::IntegerOutOfRange, start));
            }
            Ok(Value::Integer { value, span })
        } else {
            let value = text
                .parse::<f64>()
                .map_err(|_| (ErrorCode::InvalidNumber, start))?;
            if !value.is_finite() {
                return Err((ErrorCode::NonFiniteNumber, start));
            }
            Ok(Value::Float { value, span })
        }
    }

    fn string(&mut self) -> Result<String, Failure> {
        let start = self.position;
        self.position += 1;
        let mut value = String::new();
        loop {
            let run_start = self.position;
            while let Some(byte) = self.peek() {
                if byte == b'"' || byte == b'\\' || byte < 0x20 {
                    break;
                }
                self.position += 1;
            }
            value.push_str(&self.source[run_start..self.position]);
            if value.len() > self.limits.max_string_bytes {
                return Err((ErrorCode::StringTooLong, start));
            }
            match self.peek() {
                None => return Err((ErrorCode::UnterminatedString, start)),
                Some(b'"') => {
                    self.position += 1;
                    return Ok(value);
                }
                Some(b'\\') => {
                    let escaped = self.escape()?;
                    if escaped == '\0' {
                        return Err((ErrorCode::NulInString, self.position));
                    }
                    value.push(escaped);
                    if value.len() > self.limits.max_string_bytes {
                        return Err((ErrorCode::StringTooLong, start));
                    }
                }
                Some(_) => return Err((ErrorCode::ControlCharacterInString, self.position)),
            }
        }
    }

    fn escape(&mut self) -> Result<char, Failure> {
        let start = self.position;
        let kind = self
            .bytes
            .get(start + 1)
            .copied()
            .ok_or((ErrorCode::UnterminatedString, start))?;
        self.position += 2;
        Ok(match kind {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => {
                let first = self.hex4(start)?;
                match first {
                    0xd800..=0xdbff => {
                        if self.bytes.get(self.position) != Some(&b'\\')
                            || self.bytes.get(self.position + 1) != Some(&b'u')
                        {
                            return Err((ErrorCode::InvalidUnicodeEscape, start));
                        }
                        self.position += 2;
                        let second = self.hex4(start)?;
                        if !(0xdc00..=0xdfff).contains(&second) {
                            return Err((ErrorCode::InvalidUnicodeEscape, start));
                        }
                        let scalar = 0x1_0000 + ((first - 0xd800) << 10) + (second - 0xdc00);
                        char::from_u32(scalar).ok_or((ErrorCode::InvalidUnicodeEscape, start))?
                    }
                    0xdc00..=0xdfff => return Err((ErrorCode::InvalidUnicodeEscape, start)),
                    scalar => {
                        char::from_u32(scalar).ok_or((ErrorCode::InvalidUnicodeEscape, start))?
                    }
                }
            }
            _ => return Err((ErrorCode::InvalidEscape, start)),
        })
    }

    fn hex4(&mut self, escape_start: usize) -> Result<u32, Failure> {
        let digits = self
            .bytes
            .get(self.position..self.position + 4)
            .ok_or((ErrorCode::InvalidUnicodeEscape, escape_start))?;
        let mut value = 0_u32;
        for digit in digits {
            let nibble = char::from(*digit)
                .to_digit(16)
                .ok_or((ErrorCode::InvalidUnicodeEscape, escape_start))?;
            value = value * 16 + nibble;
        }
        self.position += 4;
        Ok(value)
    }

    fn array(&mut self, depth: usize) -> Result<Value, Failure> {
        let start = self.position;
        if depth > self.limits.max_depth {
            return Err((ErrorCode::DepthLimit, start));
        }
        self.position += 1;
        let mut items = Vec::new();
        self.trivia()?;
        if self.peek() == Some(b']') {
            self.position += 1;
            return Ok(Value::Array {
                items,
                span: Span {
                    start,
                    end: self.position,
                },
            });
        }
        loop {
            self.trivia()?;
            if items.len() == self.limits.max_array_items {
                return Err((ErrorCode::ArrayLimit, self.position));
            }
            items.push(self.value(depth)?);
            self.trivia()?;
            match self.peek() {
                Some(b',') => {
                    let comma = self.position;
                    self.position += 1;
                    self.trivia()?;
                    if self.peek() == Some(b']') {
                        return Err((ErrorCode::TrailingComma, comma));
                    }
                }
                Some(b']') => {
                    self.position += 1;
                    return Ok(Value::Array {
                        items,
                        span: Span {
                            start,
                            end: self.position,
                        },
                    });
                }
                None => return Err((ErrorCode::UnexpectedEnd, self.position)),
                Some(_) => return Err((ErrorCode::ExpectedCommaOrEnd, self.position)),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, Failure> {
        let start = self.position;
        if depth > self.limits.max_depth {
            return Err((ErrorCode::DepthLimit, start));
        }
        self.position += 1;
        let mut members: Vec<Member> = Vec::new();
        let mut keys = HashSet::new();
        self.trivia()?;
        if self.peek() == Some(b'}') {
            self.position += 1;
            return Ok(Value::Object {
                members,
                span: Span {
                    start,
                    end: self.position,
                },
            });
        }
        loop {
            self.trivia()?;
            let key_start = self.position;
            match self.peek() {
                Some(b'"') => {}
                None => return Err((ErrorCode::UnexpectedEnd, key_start)),
                Some(_) => return Err((ErrorCode::ExpectedKey, key_start)),
            }
            if members.len() == self.limits.max_object_keys {
                return Err((ErrorCode::ObjectLimit, key_start));
            }
            let key = self.string()?;
            let key_span = Span {
                start: key_start,
                end: self.position,
            };
            if !keys.insert(key.clone()) {
                return Err((ErrorCode::DuplicateKey, key_start));
            }
            self.trivia()?;
            if self.peek() != Some(b':') {
                return Err((ErrorCode::ExpectedColon, self.position));
            }
            self.position += 1;
            self.trivia()?;
            let value = self.value(depth)?;
            members.push(Member {
                key,
                key_span,
                value,
            });
            self.trivia()?;
            match self.peek() {
                Some(b',') => {
                    let comma = self.position;
                    self.position += 1;
                    self.trivia()?;
                    if self.peek() == Some(b'}') {
                        return Err((ErrorCode::TrailingComma, comma));
                    }
                }
                Some(b'}') => {
                    self.position += 1;
                    return Ok(Value::Object {
                        members,
                        span: Span {
                            start,
                            end: self.position,
                        },
                    });
                }
                None => return Err((ErrorCode::UnexpectedEnd, self.position)),
                Some(_) => return Err((ErrorCode::ExpectedCommaOrEnd, self.position)),
            }
        }
    }
}

#[cfg(test)]
mod tests;
