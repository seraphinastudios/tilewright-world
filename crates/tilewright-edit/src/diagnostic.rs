// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Diagnostics at byte spans of a transaction document.

use tilewright_source::Diagnostic;

/// Line starts of a text, for 1-based line and character-column lookup.
pub(crate) struct Lines<'s> {
    source: &'s str,
    starts: Vec<usize>,
}

impl<'s> Lines<'s> {
    pub(crate) fn new(source: &'s str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            source
                .bytes()
                .enumerate()
                .filter(|(_, byte)| *byte == b'\n')
                .map(|(index, _)| index + 1),
        );
        Self { source, starts }
    }

    fn locate(&self, offset: usize) -> (usize, usize) {
        let mut boundary = offset.min(self.source.len());
        while !self.source.is_char_boundary(boundary) {
            boundary -= 1;
        }
        let line = self.starts.partition_point(|start| *start <= boundary);
        let start = self.starts[line - 1];
        (line, self.source[start..boundary].chars().count() + 1)
    }
}

/// A diagnostic at `start..end` of the text `lines` indexes.
pub(crate) fn at(
    path: &str,
    lines: &Lines<'_>,
    start: usize,
    end: usize,
    code: &'static str,
    message: impl Into<String>,
) -> Diagnostic {
    let (line, column) = lines.locate(start);
    Diagnostic {
        path: path.to_owned(),
        start,
        end,
        line,
        column,
        code,
        message: message.into(),
    }
}

/// A diagnostic without a position.
pub(crate) fn file(path: &str, code: &'static str, message: impl Into<String>) -> Diagnostic {
    Diagnostic {
        path: path.to_owned(),
        start: 0,
        end: 0,
        line: 0,
        column: 0,
        code,
        message: message.into(),
    }
}

/// A string for a message: quoted when short, described by its size when
/// long, so that a message costs little whatever the string.
pub(crate) fn quoted(text: &str) -> String {
    if text.len() <= 128 {
        let mut output = String::new();
        tilewright_jsonc::write_json_string(text, &mut output);
        output
    } else {
        format!("a string of {} bytes", text.len())
    }
}
