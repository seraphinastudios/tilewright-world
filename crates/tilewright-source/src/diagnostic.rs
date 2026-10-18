// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

use std::fmt;
use tilewright_jsonc::ErrorCode;

/// Anything with a stable diagnostic code string.
pub(crate) trait CodeName: Copy {
    fn name(self) -> &'static str;
}

impl CodeName for Code {
    fn name(self) -> &'static str {
        self.as_str()
    }
}

impl CodeName for ErrorCode {
    fn name(self) -> &'static str {
        self.as_str()
    }
}

/// Stable diagnostic codes. JSONC syntax errors use the parser's own codes.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Code {
    /// A file could not be read.
    Io,
    /// A file is not UTF-8.
    Encoding,
    /// A load exceeded its file or byte budget.
    Budget,
    /// A symbolic link where the format expects a file or directory.
    Symlink,
    /// A file or directory the format does not define.
    UnexpectedFile,
    /// A required file is missing.
    MissingFile,
    /// A path in a document is not a safe relative path.
    Path,
    /// A file name disagrees with the document's content.
    NameMismatch,
    /// A `format` value other than the one the document kind requires.
    Format,
    /// A key the format or schema does not define.
    UnknownKey,
    /// A required key is missing.
    MissingKey,
    /// A value of the wrong JSON type.
    Type,
    /// A value outside its allowed range or set.
    Range,
    /// An identifier that does not match the identifier pattern.
    Identifier,
    /// Two entries with the same identity.
    Duplicate,
    /// A reference that does not resolve.
    Reference,
    /// Zone parents that form a cycle.
    Cycle,
    /// A malformed terrain grid.
    Grid,
}

impl Code {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Io => "source.io",
            Self::Encoding => "source.encoding",
            Self::Budget => "source.budget",
            Self::Symlink => "source.symlink",
            Self::UnexpectedFile => "source.unexpected_file",
            Self::MissingFile => "source.missing_file",
            Self::Path => "source.path",
            Self::NameMismatch => "source.name_mismatch",
            Self::Format => "shape.format",
            Self::UnknownKey => "shape.unknown_key",
            Self::MissingKey => "shape.missing_key",
            Self::Type => "shape.type",
            Self::Range => "shape.range",
            Self::Identifier => "shape.identifier",
            Self::Duplicate => "ref.duplicate",
            Self::Reference => "ref.unresolved",
            Self::Cycle => "ref.cycle",
            Self::Grid => "grid.invalid",
        }
    }
}

/// One problem in a source tree.
///
/// `start..end` is the byte span in the file; `line` and `column` are
/// 1-based and both 0 when the problem has no position, such as a missing
/// file.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Diagnostic {
    pub path: String,
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub column: usize,
    pub code: &'static str,
    pub message: String,
}

impl Diagnostic {
    /// A diagnostic without a position.
    pub(crate) fn file(path: &str, code: impl CodeName, message: impl Into<String>) -> Self {
        Self {
            path: path.to_owned(),
            start: 0,
            end: 0,
            line: 0,
            column: 0,
            code: code.name(),
            message: message.into(),
        }
    }

    /// A diagnostic at a byte span of `source`.
    pub(crate) fn at(
        path: &str,
        source: &str,
        start: usize,
        end: usize,
        code: impl CodeName,
        message: impl Into<String>,
    ) -> Self {
        let mut boundary = start.min(source.len());
        while !source.is_char_boundary(boundary) {
            boundary -= 1;
        }
        let before = &source[..boundary];
        let line = before.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let line_start = before.rfind('\n').map_or(0, |index| index + 1);
        Self {
            path: path.to_owned(),
            start,
            end,
            line,
            column: source[line_start..boundary].chars().count() + 1,
            code: code.name(),
            message: message.into(),
        }
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            write!(formatter, "{}: {}: {}", self.path, self.code, self.message)
        } else {
            write!(
                formatter,
                "{}:{}:{}: {}: {}",
                self.path, self.line, self.column, self.code, self.message
            )
        }
    }
}
