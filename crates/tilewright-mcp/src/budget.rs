// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Budgets and cursors that every paged tool shares (docs/MCP.md,
//! "Limits").
//!
//! A tool's result is sent twice: as `structuredContent`, and serialized
//! as its text content. A page holds at most `limit` items and stops early,
//! before the item that would take the two together past
//! [`MAXIMUM_RESULT_BYTES`]; every string value is cut to
//! [`MAXIMUM_STRING_BYTES`], and the result counts the strings it cut. A
//! cursor names the tool, the arguments and the snapshot it was issued
//! for, and the offset of the next item.

use crate::json::{clip, json_string};

/// The `limit` of a call that gives none.
pub const DEFAULT_LIMIT: usize = 50;
/// The largest `limit` a call may give.
pub const MAXIMUM_LIMIT: usize = 500;
/// Most bytes of a tool call's `result` as sent: its structured result and
/// the text copy of it together, with the fields around them.
pub const MAXIMUM_RESULT_BYTES: usize = 256 * 1024;
/// Most bytes of one string value, before escaping; a longer one is cut at
/// a character boundary.
pub const MAXIMUM_STRING_BYTES: usize = 4096;
/// An upper bound on what one item of a page costs, both copies counted: a
/// few strings of at most [`MAXIMUM_STRING_BYTES`] each, escaped to at most
/// six bytes per byte, then escaped again in the text copy. It is well
/// under the budget of a page, so a page always holds its first item.
pub const MAXIMUM_ITEM_BYTES: usize = 128 * 1024;
/// Most bytes of a cursor a client sends back; a longer one is invalid.
pub const MAXIMUM_CURSOR_BYTES: usize = 256;

/// Room kept for the fields around a page's items, both copies counted.
pub const ENVELOPE_BYTES: usize = 8192;

/// The bytes the items of a page may cost.
pub const PAGE_BYTES: usize = MAXIMUM_RESULT_BYTES - ENVELOPE_BYTES;

/// What `json` costs once sent: as written, and again as a JSON string in
/// the text copy, where `"` and `\\` take two bytes and a control
/// character six.
pub fn cost(json: &str) -> usize {
    json.len()
        + json
            .chars()
            .map(|character| match character {
                '"' | '\\' => 2,
                character if character.is_control() => 6,
                character => character.len_utf8(),
            })
            .sum::<usize>()
}

/// Structured output being written, with a count of the strings it cut.
#[derive(Debug, Default)]
pub struct Output {
    pub text: String,
    pub cut_strings: usize,
}

impl Output {
    /// Appends a string value, cut to [`MAXIMUM_STRING_BYTES`].
    pub fn string(&mut self, value: &str) -> bool {
        let (value, cut) = clip(value, MAXIMUM_STRING_BYTES);
        self.cut_strings += usize::from(cut);
        json_string(value, &mut self.text);
        cut
    }

    pub fn raw(&mut self, text: &str) {
        self.text.push_str(text);
    }
}

/// One page of items, written as a JSON array.
#[derive(Debug)]
pub struct Page {
    /// The items, as a JSON array.
    pub items: String,
    /// What the items cost once sent ([`cost`]).
    pub cost: usize,
    pub returned: usize,
    /// Strings cut in these items.
    pub cut_strings: usize,
    /// The offset of the next item, when one remains.
    pub next: Option<usize>,
    /// Whether the page stopped before `limit` items to stay within its
    /// byte budget.
    pub stopped_by_bytes: bool,
}

/// Writes the page of `items` (all items, the page starting at `offset`)
/// with at most `limit` of them, within [`PAGE_BYTES`].
pub fn page<T>(
    items: impl Iterator<Item = T>,
    offset: usize,
    limit: usize,
    write: impl FnMut(&mut Output, T),
) -> Page {
    page_within(items, offset, limit, PAGE_BYTES, write)
}

/// [`page`] within `budget` bytes of [`cost`], for a result of several
/// lists. The page never costs more than `budget`: an item that does not
/// fit ends it.
pub fn page_within<T>(
    items: impl Iterator<Item = T>,
    offset: usize,
    limit: usize,
    budget: usize,
    mut write: impl FnMut(&mut Output, T),
) -> Page {
    let mut page = Page {
        items: String::from("["),
        cost: cost("[]"),
        returned: 0,
        cut_strings: 0,
        next: None,
        stopped_by_bytes: false,
    };
    for (index, item) in items.enumerate().skip(offset) {
        if page.returned == limit {
            page.next = Some(index);
            break;
        }
        let mut output = Output::default();
        write(&mut output, item);
        let item_cost = cost(&output.text) + usize::from(page.returned > 0) * cost(",");
        if page.cost + item_cost > budget {
            page.next = Some(index);
            page.stopped_by_bytes = true;
            break;
        }
        if page.returned > 0 {
            page.items.push(',');
        }
        page.items.push_str(&output.text);
        page.cost += item_cost;
        page.cut_strings += output.cut_strings;
        page.returned += 1;
    }
    page.items.push(']');
    page
}

/// The `scan_limit` of a search that gives none, and the largest it may
/// give: the most candidates one call examines.
pub const MAXIMUM_SCAN: usize = 10_000;
/// Most bytes of `list`, `object` and `json` field values a search parses
/// again in one call to match scalars inside them. A call stops after the
/// candidate that reaches it, so it parses at most this plus one value.
pub const MAXIMUM_SCAN_JSON_BYTES: usize = 8 * 1024 * 1024;

/// The work a search has done in one call.
#[derive(Debug, Default)]
pub struct Work {
    /// Bytes of field values parsed again.
    pub json_bytes: usize,
}

/// One page of a search: like a [`Page`], and the candidates examined.
#[derive(Debug)]
pub struct ScanPage {
    pub page: Page,
    /// Candidates examined in this call.
    pub scanned: usize,
    /// Whether the call stopped at its scan budget, before `limit` matches
    /// and before the byte budget.
    pub stopped_by_scan: bool,
    /// Whether the candidates ended before `offset`: a cursor past the end.
    pub past_end: bool,
}

/// Examines `candidates` (all of them, the call resuming at `offset`) one
/// by one with `matches`, writing each match, until `limit` matches, the
/// byte budget ([`PAGE_BYTES`]), `scan_limit` candidates or
/// [`MAXIMUM_SCAN_JSON_BYTES`] of parsed field values. `next` is then the
/// offset of the first candidate not examined, or of the match that did not
/// fit, so paging on returns every match exactly once.
pub fn scan<T, M>(
    candidates: impl Iterator<Item = T>,
    offset: usize,
    limit: usize,
    scan_limit: usize,
    mut matches: impl FnMut(&T, &mut Work) -> Option<M>,
    mut write: impl FnMut(&mut Output, &T, M),
) -> ScanPage {
    let mut result = ScanPage {
        page: Page {
            items: String::from("["),
            cost: cost("[]"),
            returned: 0,
            cut_strings: 0,
            next: None,
            stopped_by_bytes: false,
        },
        scanned: 0,
        stopped_by_scan: false,
        past_end: offset > 0,
    };
    let mut work = Work::default();
    let page = &mut result.page;
    for (index, candidate) in candidates.enumerate().skip(offset) {
        result.past_end = false;
        if page.returned == limit {
            page.next = Some(index);
            break;
        }
        if result.scanned == scan_limit || work.json_bytes >= MAXIMUM_SCAN_JSON_BYTES {
            page.next = Some(index);
            result.stopped_by_scan = true;
            break;
        }
        result.scanned += 1;
        let Some(found) = matches(&candidate, &mut work) else {
            continue;
        };
        let mut output = Output::default();
        write(&mut output, &candidate, found);
        let item_cost = cost(&output.text) + usize::from(page.returned > 0) * cost(",");
        if page.cost + item_cost > PAGE_BYTES {
            page.next = Some(index);
            page.stopped_by_bytes = true;
            break;
        }
        if page.returned > 0 {
            page.items.push(',');
        }
        page.items.push_str(&output.text);
        page.cost += item_cost;
        page.cut_strings += output.cut_strings;
        page.returned += 1;
    }
    result.page.items.push(']');
    result
}

/// Why a cursor was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CursorError {
    /// Not a cursor this server issued for this tool and these arguments.
    Invalid,
    /// Issued for another snapshot of the tree: the tree was reloaded
    /// since, and changed.
    Stale,
}

impl CursorError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Invalid => "cursor.invalid",
            Self::Stale => "cursor.stale",
        }
    }

    pub const fn message(self) -> &'static str {
        match self {
            Self::Invalid => {
                "the cursor was not issued by this server for this tool and these arguments; call again without a cursor"
            }
            Self::Stale => {
                "the cursor was issued for another revision of the tree; call again without a cursor"
            }
        }
    }
}

/// The prefix of this server's cursors, with the cursor format's version.
const CURSOR_PREFIX: &str = "tw1";

/// A cursor for the item at `offset` of `tool` with the arguments whose
/// fingerprint is `arguments`, on the snapshot `binding`.
pub fn cursor(tool: &str, binding: &str, arguments: u64, offset: usize) -> String {
    format!("{CURSOR_PREFIX}.{tool}.{binding}.{arguments:016x}.{offset}")
}

/// The offset a cursor names, checked against the tool, snapshot and
/// arguments of the call it came back with, and the call's item count.
pub fn resume(
    text: &str,
    tool: &str,
    binding: &str,
    arguments: u64,
    total: usize,
) -> Result<usize, CursorError> {
    if text.len() > MAXIMUM_CURSOR_BYTES {
        return Err(CursorError::Invalid);
    }
    let parts: Vec<&str> = text.split('.').collect();
    let [prefix, named_tool, named_binding, named_arguments, offset] = parts.as_slice() else {
        return Err(CursorError::Invalid);
    };
    if *prefix != CURSOR_PREFIX
        || *named_tool != tool
        || !is_binding(named_binding)
        || named_arguments.len() != 16
        || u64::from_str_radix(named_arguments, 16).ok() != Some(arguments)
    {
        return Err(CursorError::Invalid);
    }
    // Digits only, without a leading zero, so one offset has one spelling.
    let offset = (!offset.is_empty()
        && offset.bytes().all(|byte| byte.is_ascii_digit())
        && (offset.len() == 1 || !offset.starts_with('0')))
    .then(|| offset.parse::<usize>().ok())
    .flatten()
    .ok_or(CursorError::Invalid)?;
    if *named_binding != binding {
        return Err(CursorError::Stale);
    }
    // A cursor is only issued for an item that exists.
    if offset == 0 || offset >= total {
        return Err(CursorError::Invalid);
    }
    Ok(offset)
}

/// Whether `text` is a snapshot binding: 64 hex digits for a revision, or
/// `x` and 16 hex digits for a tree that does not load.
pub fn is_binding(text: &str) -> bool {
    let hex = |text: &str| {
        text.bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };
    match text.strip_prefix('x') {
        Some(rest) => rest.len() == 16 && hex(rest),
        None => text.len() == 64 && hex(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_resume_only_where_issued() {
        let binding = "a".repeat(64);
        let text = cursor("validate", &binding, 7, 50);
        assert_eq!(resume(&text, "validate", &binding, 7, 51), Ok(50));
        assert_eq!(
            resume(&text, "validate", &"b".repeat(64), 7, 51),
            Err(CursorError::Stale)
        );
        for (tool, arguments, total) in [("search_ids", 7, 51), ("validate", 8, 51)] {
            assert_eq!(
                resume(&text, tool, &binding, arguments, total),
                Err(CursorError::Invalid)
            );
        }
        // Past the end, after the tree shrank without changing revision,
        // which cannot happen, or crafted.
        assert_eq!(
            resume(&text, "validate", &binding, 7, 50),
            Err(CursorError::Invalid)
        );
        for text in [
            "",
            "tw1",
            "tw2.validate.x0000000000000000.0000000000000007.1",
            "tw1.validate.x0000000000000000.0000000000000007.01",
            "tw1.validate.x0000000000000000.0000000000000007.+1",
            "tw1.validate.x0000000000000000.0000000000000007.0",
            "tw1.validate.x0000000000000000.0000000000000007.99999999999999999999999",
            "tw1.validate.x000000000000000.0000000000000007.1",
            "tw1.validate.X0000000000000000.0000000000000007.1",
            "tw1.validate.x0000000000000000.000000000000007.1",
            "tw1.validate.x0000000000000000.0000000000000007.1.1",
        ] {
            assert_eq!(
                resume(text, "validate", "x0000000000000000", 7, 100),
                Err(CursorError::Invalid),
                "{text}"
            );
        }
        assert_eq!(
            resume(
                &cursor("validate", "x0000000000000000", 7, 1),
                "validate",
                "x0000000000000000",
                7,
                100
            ),
            Ok(1)
        );
        assert_eq!(
            resume(&"a".repeat(300), "validate", &binding, 7, 100),
            Err(CursorError::Invalid)
        );
    }

    #[test]
    fn pages_keep_to_the_limit_and_the_byte_budget() {
        let page = page(0..10, 3, 4, |output, item| {
            output.raw(&item.to_string());
        });
        assert_eq!(page.items, "[3,4,5,6]");
        assert_eq!(page.next, Some(7));
        assert!(!page.stopped_by_bytes);

        let long = "é".repeat(MAXIMUM_STRING_BYTES);
        let page = super::page(0..1000, 0, MAXIMUM_LIMIT, |output, _| {
            output.string(&long);
        });
        assert!(page.stopped_by_bytes);
        assert_eq!(page.cut_strings, page.returned);
        assert!(page.cost <= PAGE_BYTES && cost(&page.items) == page.cost);
        assert_eq!(page.next, Some(page.returned));
        // Cut at a character boundary.
        assert!(
            page.items
                .starts_with(&format!("[\"{}\"", "é".repeat(2048)))
        );

        let page = super::page(0..2, 2, 5, |output, item: i32| {
            output.raw(&item.to_string());
        });
        assert_eq!((page.items.as_str(), page.next), ("[]", None));
    }
}
