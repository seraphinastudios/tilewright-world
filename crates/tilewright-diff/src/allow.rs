// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Allowlists (`tilewright.diff-allow/1`): the removals and renames a change
//! intends, so that a comparison accepts them.

use crate::{ALLOWLIST_FORMAT, Kind};
use std::{collections::BTreeMap, fmt, io::Read as _, path::Path};
use tilewright_jsonc::{Document, ErrorCode, Limits, Span, Value};

/// Intended removals and renames, at most one for each old ID.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Allowlist {
    /// By kind and old ID: `None` for a removal, the new ID for a rename.
    entries: BTreeMap<Kind, BTreeMap<String, Option<String>>>,
}

/// One allowlist entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Allowance {
    /// `id` may be removed.
    Remove { kind: Kind, id: String },
    /// `from` may be removed when `to` exists in the new world.
    Rename {
        kind: Kind,
        from: String,
        to: String,
    },
}

impl Allowance {
    #[must_use]
    pub const fn kind(&self) -> Kind {
        match self {
            Self::Remove { kind, .. } | Self::Rename { kind, .. } => *kind,
        }
    }

    /// The ID the entry lets go: the removed ID, or the rename's old ID.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Remove { id, .. } => id,
            Self::Rename { from, .. } => from,
        }
    }
}

/// What an entry intends for its old ID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Intent<'a> {
    Remove,
    /// A rename to this new ID.
    Rename(&'a str),
}

/// Why an allowlist could not be read: the first problem found.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AllowlistError {
    /// The 1-based line and column, in characters; both 0 when the problem
    /// has no position, such as an unreadable file.
    pub line: usize,
    pub column: usize,
    /// A stable code, as source diagnostics use: `source.io`,
    /// `source.encoding`, `jsonc.*`, `shape.*` or `ref.duplicate`.
    pub code: &'static str,
    pub message: String,
}

impl AllowlistError {
    fn file(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            line: 0,
            column: 0,
            code,
            message: message.into(),
        }
    }

    /// An error at byte `offset` of `text`.
    fn at(text: &str, offset: usize, code: &'static str, message: impl Into<String>) -> Self {
        let mut boundary = offset.min(text.len());
        while !text.is_char_boundary(boundary) {
            boundary -= 1;
        }
        let before = &text[..boundary];
        let line_start = before.rfind('\n').map_or(0, |index| index + 1);
        Self {
            line: before.bytes().filter(|byte| *byte == b'\n').count() + 1,
            column: before[line_start..].chars().count() + 1,
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for AllowlistError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            write!(formatter, "{}: {}", self.code, self.message)
        } else {
            write!(
                formatter,
                "{}:{}: {}: {}",
                self.line, self.column, self.code, self.message
            )
        }
    }
}

impl std::error::Error for AllowlistError {}

impl Allowlist {
    /// Reads an allowlist file of at most the format's 64 MiB input limit.
    ///
    /// # Errors
    /// Returns the first problem: an unreadable or oversized file, text
    /// that is not UTF-8, or any problem [`Allowlist::parse`] reports.
    pub fn read(path: &Path) -> Result<Self, AllowlistError> {
        let limit = Limits::default().max_input_bytes;
        let io = |error: std::io::Error| AllowlistError::file("source.io", error.to_string());
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(io)?
            .take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(io)?;
        if bytes.len() > limit {
            return Err(AllowlistError::file(
                ErrorCode::InputTooLarge.as_str(),
                format!("an allowlist is at most {limit} bytes"),
            ));
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| AllowlistError::file("source.encoding", "an allowlist is UTF-8"))?;
        Self::parse(&text)
    }

    /// Parses an allowlist document: strict JSONC with exactly the keys of
    /// `docs/spec/DIFF.md`, kinds and IDs written as the kinds require, and
    /// at most one entry for each old ID.
    ///
    /// # Errors
    /// Returns the first problem found.
    pub fn parse(text: &str) -> Result<Self, AllowlistError> {
        let document =
            Document::parse(text, &Limits::default()).map_err(|error| AllowlistError {
                line: error.line,
                column: error.column,
                code: error.code.as_str(),
                message: "invalid JSONC".into(),
            })?;
        let fail = |span: Span, code: &'static str, message: String| {
            AllowlistError::at(text, span.start, code, message)
        };
        let root = document.root();
        closed(root, &["format", "remove", "rename"], &fail)?;
        match root.get("format") {
            Some(value) if value.as_str() == Some(ALLOWLIST_FORMAT) => {}
            Some(value) => {
                return Err(fail(
                    value.span(),
                    "shape.format",
                    format!("format must be \"{ALLOWLIST_FORMAT}\""),
                ));
            }
            None => {
                return Err(fail(
                    root.span(),
                    "shape.missing_key",
                    "missing \"format\"".into(),
                ));
            }
        }
        let mut allowlist = Self::default();
        for (key, fields) in [
            ("remove", &["kind", "id"][..]),
            ("rename", &["kind", "from", "to"][..]),
        ] {
            let Some(list) = root.get(key) else {
                continue;
            };
            let items = list
                .as_array()
                .ok_or_else(|| fail(list.span(), "shape.type", format!("{key} is an array")))?;
            for item in items {
                closed(item, fields, &fail)?;
                let mut values = Vec::with_capacity(fields.len());
                for field in fields {
                    let value = item.get(field).ok_or_else(|| {
                        fail(
                            item.span(),
                            "shape.missing_key",
                            format!("missing \"{field}\""),
                        )
                    })?;
                    let text = value.as_str().ok_or_else(|| {
                        fail(value.span(), "shape.type", format!("{field} is a string"))
                    })?;
                    values.push((text, value.span()));
                }
                let (kind_text, kind_span) = values[0];
                let kind = Kind::parse(kind_text)
                    .ok_or_else(|| fail(kind_span, "shape.range", kinds_message()))?;
                for (id, span) in &values[1..] {
                    if !kind.is_id(id) {
                        return Err(fail(*span, "shape.identifier", id_form(kind)));
                    }
                }
                let (from, from_span) = values[1];
                let to = values.get(2).map(|(to, _)| (*to).to_owned());
                if to.as_deref() == Some(from) {
                    return Err(fail(
                        item.span(),
                        "shape.range",
                        "a rename's from and to differ".into(),
                    ));
                }
                let ids = allowlist.entries.entry(kind).or_default();
                if ids.contains_key(from) {
                    return Err(fail(
                        from_span,
                        "ref.duplicate",
                        format!("{kind} {from} has two entries"),
                    ));
                }
                ids.insert(from.to_owned(), to);
            }
        }
        Ok(allowlist)
    }

    /// Whether the allowlist has no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.values().all(BTreeMap::is_empty)
    }

    /// Every entry, sorted by kind and old ID.
    pub fn allowances(&self) -> impl Iterator<Item = Allowance> + '_ {
        self.entries.iter().flat_map(|(kind, ids)| {
            ids.iter().map(|(from, to)| match to {
                None => Allowance::Remove {
                    kind: *kind,
                    id: from.clone(),
                },
                Some(to) => Allowance::Rename {
                    kind: *kind,
                    from: from.clone(),
                    to: to.clone(),
                },
            })
        })
    }

    /// The entry for old ID `id`.
    pub(crate) fn get(&self, kind: Kind, id: &str) -> Option<Intent<'_>> {
        self.entries
            .get(&kind)?
            .get(id)
            .map(|to| to.as_deref().map_or(Intent::Remove, Intent::Rename))
    }
}

/// Checks that `value` is an object with no keys but `keys`.
fn closed(
    value: &Value,
    keys: &[&str],
    fail: &impl Fn(Span, &'static str, String) -> AllowlistError,
) -> Result<(), AllowlistError> {
    let members = value
        .as_object()
        .ok_or_else(|| fail(value.span(), "shape.type", "expected an object".into()))?;
    match members
        .iter()
        .find(|member| !keys.contains(&member.key.as_str()))
    {
        Some(member) => Err(fail(
            member.key_span,
            "shape.unknown_key",
            format!("unknown key; expected {}", keys.join(", ")),
        )),
        None => Ok(()),
    }
}

/// The message for a kind that is not one of [`Kind::ALL`], which it lists.
fn kinds_message() -> String {
    let names: Vec<&str> = Kind::ALL.iter().map(|kind| kind.as_str()).collect();
    match names.split_last() {
        Some((last, rest)) => format!("kind must be {} or {last}", rest.join(", ")),
        None => "kind must be a known kind".to_owned(),
    }
}

/// How IDs of `kind` are written.
fn id_form(kind: Kind) -> String {
    let form = match kind {
        Kind::Definition => "<type>/<id>",
        Kind::Object | Kind::Plane | Kind::RenderLayer => "an identifier",
        Kind::Anchor | Kind::Stairwell | Kind::Zone => "<plane>/<id>",
    };
    let article = if kind.as_str().starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
    };
    format!(
        "{article} {kind} ID is {form}, with identifiers matching ^[a-z0-9][a-z0-9_.-]{{0,127}}$"
    )
}
