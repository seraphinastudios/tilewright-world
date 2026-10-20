// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Reports: what changed between two snapshots, as text or as JSON in the
//! `tilewright.diff/1` format.

use crate::{Allowance, FORMAT, Kind, Side};
use std::fmt::Write as _;

/// The result of [`crate::diff`]. Every list is sorted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Report {
    pub old: Side,
    pub new: Side,
    /// IDs of the old world that the new one lacks and that no rename
    /// pairs, by kind and ID.
    pub removed: Vec<Removed>,
    /// Removed IDs paired with an ID of the new world, by kind, old ID and
    /// new ID.
    pub renamed: Vec<Renamed>,
    /// IDs of the new world that the old one lacks and that no rename
    /// pairs, by kind and ID.
    pub added: Vec<Added>,
    /// Allowlist entries that did not apply, by kind and old ID.
    pub unused: Vec<Unused>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Removed {
    pub kind: Kind,
    pub id: String,
    /// Whether the allowlist accepts the removal.
    pub allowed: bool,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Renamed {
    pub kind: Kind,
    pub from: String,
    pub to: String,
    pub basis: Basis,
    /// Whether the allowlist accepts the removal of `from`.
    pub allowed: bool,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Added {
    pub kind: Kind,
    pub id: String,
}

/// Why a removed ID and a new one are reported as a rename.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Basis {
    /// The allowlist declares it.
    Declared,
    /// They are the only removed and added IDs of their kind and scope with
    /// the same content.
    Content,
}

impl Basis {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Declared => "declared",
            Self::Content => "content",
        }
    }
}

/// An allowlist entry that did not apply.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Unused {
    pub allowance: Allowance,
    pub reason: Reason,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reason {
    /// The ID is not in the old world, or is still in the new one.
    NotRemoved,
    /// The rename's new ID is not in the new world, so the removal is not
    /// accepted.
    TargetMissing,
}

impl Reason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotRemoved => "not_removed",
            Self::TargetMissing => "target_missing",
        }
    }
}

impl Report {
    /// Whether an ID was removed or renamed without the allowlist accepting
    /// it.
    #[must_use]
    pub fn breaking(&self) -> bool {
        self.removed.iter().any(|removed| !removed.allowed)
            || self.renamed.iter().any(|renamed| !renamed.allowed)
    }

    /// The removed and renamed IDs the allowlist does not accept.
    #[must_use]
    pub fn not_allowed(&self) -> usize {
        self.removed
            .iter()
            .filter(|removed| !removed.allowed)
            .count()
            + self
                .renamed
                .iter()
                .filter(|renamed| !renamed.allowed)
                .count()
    }

    /// The report as one line of JSON in the `tilewright.diff/1` format,
    /// with a final newline. Every control character in a string is
    /// escaped, so the output is safe to print to a terminal.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut output = String::from("{\"format\":");
        json_string(FORMAT, &mut output);
        output.push_str(",\"old\":");
        side_json(&self.old, &mut output);
        output.push_str(",\"new\":");
        side_json(&self.new, &mut output);
        let _ = write!(output, ",\"breaking\":{}", self.breaking());
        output.push_str(",\"removed\":");
        list(&mut output, &self.removed, |removed, output| {
            entry(output, removed.kind, &[("id", &removed.id)]);
            let _ = write!(output, ",\"allowed\":{}}}", removed.allowed);
        });
        output.push_str(",\"renamed\":");
        list(&mut output, &self.renamed, |renamed, output| {
            entry(
                output,
                renamed.kind,
                &[
                    ("from", &renamed.from),
                    ("to", &renamed.to),
                    ("basis", renamed.basis.as_str()),
                ],
            );
            let _ = write!(output, ",\"allowed\":{}}}", renamed.allowed);
        });
        output.push_str(",\"added\":");
        list(&mut output, &self.added, |added, output| {
            entry(output, added.kind, &[("id", &added.id)]);
            output.push('}');
        });
        output.push_str(",\"unused\":");
        list(&mut output, &self.unused, |unused, output| {
            output.push_str("{\"allowance\":");
            match &unused.allowance {
                Allowance::Remove { kind, id } => {
                    json_string("remove", output);
                    output.push(',');
                    entry_members(output, *kind, &[("id", id)]);
                }
                Allowance::Rename { kind, from, to } => {
                    json_string("rename", output);
                    output.push(',');
                    entry_members(output, *kind, &[("from", from), ("to", to)]);
                }
            }
            output.push_str(",\"reason\":");
            json_string(unused.reason.as_str(), output);
            output.push('}');
        });
        output.push_str("}\n");
        output
    }

    /// The report as text: one line per removed, renamed and added ID, one
    /// per unused allowlist entry, and a summary line. IDs are identifiers,
    /// so no line carries control characters.
    #[must_use]
    pub fn to_text(&self) -> String {
        let allowed = |allowed: bool| if allowed { " (allowed)" } else { "" };
        let mut output = String::new();
        for removed in &self.removed {
            let _ = writeln!(
                output,
                "removed {} {}{}",
                removed.kind,
                removed.id,
                allowed(removed.allowed)
            );
        }
        for renamed in &self.renamed {
            let basis = match renamed.basis {
                crate::Basis::Declared => "declared",
                crate::Basis::Content => "same content",
            };
            let _ = writeln!(
                output,
                "renamed {} {} -> {} ({basis}{})",
                renamed.kind,
                renamed.from,
                renamed.to,
                if renamed.allowed { ", allowed" } else { "" }
            );
        }
        for added in &self.added {
            let _ = writeln!(output, "added {} {}", added.kind, added.id);
        }
        for unused in &self.unused {
            let reason = match unused.reason {
                Reason::NotRemoved => "not removed",
                Reason::TargetMissing => "target missing",
            };
            let _ = match &unused.allowance {
                Allowance::Remove { kind, id } => {
                    writeln!(output, "unused allowance: remove {kind} {id} ({reason})")
                }
                Allowance::Rename { kind, from, to } => writeln!(
                    output,
                    "unused allowance: rename {kind} {from} -> {to} ({reason})"
                ),
            };
        }
        let _ = writeln!(
            output,
            "summary: {} removed, {} renamed, {} added, {} not allowed",
            self.removed.len(),
            self.renamed.len(),
            self.added.len(),
            self.not_allowed()
        );
        output
    }
}

/// Writes `items` as a JSON array, each with `write`.
fn list<T>(output: &mut String, items: &[T], mut write: impl FnMut(&T, &mut String)) {
    output.push('[');
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            output.push(',');
        }
        write(item, output);
    }
    output.push(']');
}

/// Opens an entry object with its kind and string members, leaving it open.
fn entry(output: &mut String, kind: Kind, members: &[(&str, &str)]) {
    output.push('{');
    entry_members(output, kind, members);
}

fn entry_members(output: &mut String, kind: Kind, members: &[(&str, &str)]) {
    output.push_str("\"kind\":");
    json_string(kind.as_str(), output);
    for (key, value) in members {
        output.push(',');
        json_string(key, output);
        output.push(':');
        json_string(value, output);
    }
}

fn side_json(side: &Side, output: &mut String) {
    output.push_str("{\"input\":");
    json_string(side.input.as_str(), output);
    output.push_str(",\"profile\":");
    json_string(&side.profile, output);
    output.push_str(",\"profile_version\":");
    json_string(&side.profile_version, output);
    output.push_str(",\"source_digest\":");
    json_string(&side.source_digest, output);
    output.push('}');
}

/// Writes `text` as a JSON string with every control character escaped in
/// JSON's `\u00XX` form, DEL and C1 controls included.
fn json_string(text: &str, output: &mut String) {
    output.push('"');
    for character in text.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            character if character.is_control() => {
                let _ = write!(output, "\\u{:04x}", u32::from(character));
            }
            character => output.push(character),
        }
    }
    output.push('"');
}
