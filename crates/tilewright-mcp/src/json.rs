// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! JSON output. Every message is written by hand, as the CLI writes its
//! JSON: strings escape every control character, so no value can break a
//! line of the transport or reach a terminal as a control sequence.

use std::fmt::Write as _;

/// Writes `text` as a JSON string with every control character escaped in
/// JSON's `\u00XX` form, DEL and C1 controls included, so JSON output needs
/// no further escaping to be safe for a terminal, and holds no raw newline.
/// The line and paragraph separators U+2028 and U+2029, which some line
/// splitters break lines at, are escaped too ([`escaped`]).
/// The CLI's `validate --json` writes its strings with this too.
pub fn json_string(text: &str, output: &mut String) {
    output.push('"');
    for character in text.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            character if escaped(character) => {
                let _ = write!(output, "\\u{:04x}", u32::from(character));
            }
            character => output.push(character),
        }
    }
    output.push('"');
}

/// Whether [`json_string`] writes `character` as `\uXXXX`: a control
/// character, or U+2028 or U+2029.
pub(crate) fn escaped(character: char) -> bool {
    character.is_control() || matches!(character, '\u{2028}' | '\u{2029}')
}

/// `text` cut to at most `maximum` bytes at a character boundary, and
/// whether it was cut.
pub(crate) fn clip(text: &str, maximum: usize) -> (&str, bool) {
    if text.len() <= maximum {
        return (text, false);
    }
    let mut end = maximum;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], true)
}

/// Lowercase hex of `bytes`.
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut output, byte| {
        let _ = write!(output, "{byte:02x}");
        output
    })
}

/// FNV-1a, 64 bits: a fingerprint for cursors, not a security measure. A
/// cursor only has to tell the arguments and tree it was issued for from
/// others a client sends by mistake.
pub(crate) fn fingerprint(parts: &[&[u8]]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for byte in part.iter().copied().chain(
            // Each part is followed by its length, so ["ab", "c"] and
            // ["a", "bc"] differ.
            (part.len() as u64).to_le_bytes(),
        ) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}
