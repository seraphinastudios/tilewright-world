// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Diagnostics at byte spans of a transaction document.

use tilewright_source::Diagnostic;

/// Bytes between character-count checkpoints in [`Lines`].
const STRIDE: usize = 256;

/// Line starts of a text, and character counts every [`STRIDE`] bytes, for
/// 1-based line and character-column lookup: each lookup is a binary
/// search and a scan of at most [`STRIDE`] bytes, however long the line.
pub(crate) struct Lines<'s> {
    source: &'s str,
    starts: Vec<usize>,
    /// Characters before byte `i * STRIDE`.
    characters: Vec<usize>,
}

impl<'s> Lines<'s> {
    pub(crate) fn new(source: &'s str) -> Self {
        let mut starts = vec![0];
        let mut characters = Vec::with_capacity(source.len() / STRIDE + 1);
        let mut count = 0;
        for (index, byte) in source.bytes().enumerate() {
            if index.is_multiple_of(STRIDE) {
                characters.push(count);
            }
            if byte == b'\n' {
                starts.push(index + 1);
            }
            if byte & 0xC0 != 0x80 {
                count += 1;
            }
        }
        if source.len().is_multiple_of(STRIDE) {
            characters.push(count);
        }
        Self {
            source,
            starts,
            characters,
        }
    }

    /// Characters before `offset`, a character boundary within the text.
    fn characters_before(&self, offset: usize) -> usize {
        let block = offset / STRIDE;
        self.characters[block]
            + self.source.as_bytes()[block * STRIDE..offset]
                .iter()
                .filter(|byte| **byte & 0xC0 != 0x80)
                .count()
    }

    fn locate(&self, offset: usize) -> (usize, usize) {
        let mut boundary = offset.min(self.source.len());
        while !self.source.is_char_boundary(boundary) {
            boundary -= 1;
        }
        let line = self.starts.partition_point(|start| *start <= boundary);
        let start = self.starts[line - 1];
        (
            line,
            self.characters_before(boundary) - self.characters_before(start) + 1,
        )
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
