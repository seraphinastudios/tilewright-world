// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! `inspect`: one item of the snapshot, as its file holds it now, with its
//! source span (docs/MCP.md, "Inspecting an item").
//!
//! The loaded world keeps no spans, so the item's one file is read again
//! through the toolkit's no-follow access layer and parsed with the strict
//! JSONC parser under the format's limits, as the loader parses it. The
//! library keeps no per-file digests either, so the item found there is
//! compared with the snapshot's own: the same canonical JSON for an item
//! the library keeps it for (a definition, plane, zone or stairwell), and
//! every value the library keeps for an anchor or an object. Anything else
//! is `revision.changed`: the tree changed on disk since the snapshot.

use crate::budget::{self, MAXIMUM_STRING_BYTES, Output, PAGE_BYTES};
use crate::json::{clip, json_string};
use std::fmt::Write as _;
use std::path::Path;
use tilewright_diff::Kind;
use tilewright_jsonc::{Document, Limits, Value};
use tilewright_source::{Anchor, FieldValue, Object, Portal, SourceRoot, World};

/// What `inspect` can fail with, besides the checks every tool makes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Failure {
    /// The ID is not of the form the kind's IDs have.
    MalformedId,
    /// The snapshot holds no such item.
    NotFound,
    /// The file no longer holds the snapshot's item.
    Changed,
}

/// An item of the snapshot to inspect.
enum Item<'w> {
    /// A whole document compared by its canonical JSON: a definition or a
    /// plane.
    Document {
        canonical: &'w str,
    },
    /// An entry of a plane's zones or stairwells, by ID, compared by its
    /// canonical JSON.
    Entry {
        list: &'static str,
        id: &'w str,
        canonical: &'w str,
    },
    Anchor {
        id: &'w str,
        anchor: &'w Anchor,
    },
    /// An object, by its place in its chunk.
    Object {
        position: &'w [usize],
        object: &'w Object,
    },
}

/// Whether `id` has the shape the IDs of `kind` have: `owner/name` with
/// both parts non-empty for a definition, zone, anchor or stairwell; any
/// string for a plane or object (render layers are not items).
pub fn well_formed(kind: Kind, id: &str) -> bool {
    match kind {
        Kind::Definition | Kind::Zone | Kind::Anchor | Kind::Stairwell => id
            .split_once('/')
            .is_some_and(|(owner, name)| !owner.is_empty() && !name.is_empty()),
        Kind::Plane | Kind::Object => true,
        Kind::RenderLayer => false,
    }
}

/// The item of `kind` named `id` in `world`, and the root-relative path of
/// its file.
fn find<'w>(world: &'w World, kind: Kind, id: &'w str) -> Result<(String, Item<'w>), Failure> {
    let owned = |id: &'w str| {
        id.split_once('/')
            .filter(|(owner, name)| !owner.is_empty() && !name.is_empty())
            .ok_or(Failure::MalformedId)
    };
    match kind {
        Kind::Definition => {
            let (kind, name) = owned(id)?;
            let definition = world
                .definitions()
                .get(&(kind.to_owned(), name.to_owned()))
                .ok_or(Failure::NotFound)?;
            Ok((
                format!("defs/{kind}/{name}.jsonc"),
                Item::Document {
                    canonical: &definition.canonical,
                },
            ))
        }
        Kind::Plane => {
            let plane = world.planes().get(id).ok_or(Failure::NotFound)?;
            Ok((
                format!("planes/{id}/plane.jsonc"),
                Item::Document {
                    canonical: &plane.canonical,
                },
            ))
        }
        Kind::Zone | Kind::Anchor | Kind::Stairwell => {
            let (plane_id, name) = owned(id)?;
            let plane = world.planes().get(plane_id).ok_or(Failure::NotFound)?;
            let (file, item) = match kind {
                Kind::Zone => {
                    let zone = plane.zones.get(name).ok_or(Failure::NotFound)?;
                    (
                        "zones",
                        Item::Entry {
                            list: "zones",
                            id: name,
                            canonical: &zone.canonical,
                        },
                    )
                }
                Kind::Stairwell => {
                    let stairwell = plane.stairwells.get(name).ok_or(Failure::NotFound)?;
                    (
                        "stairwells",
                        Item::Entry {
                            list: "stairwells",
                            id: name,
                            canonical: &stairwell.canonical,
                        },
                    )
                }
                _ => {
                    let anchor = plane.anchors.get(name).ok_or(Failure::NotFound)?;
                    ("anchors", Item::Anchor { id: name, anchor })
                }
            };
            Ok((format!("planes/{plane_id}/{file}.jsonc"), item))
        }
        Kind::Object => {
            let location = world.object_location(id).ok_or(Failure::NotFound)?;
            let chunk = world
                .planes()
                .get(&location.plane)
                .and_then(|plane| plane.chunks.get(&location.chunk))
                .ok_or(Failure::NotFound)?;
            let object = chunk.object(&location.path).ok_or(Failure::NotFound)?;
            Ok((
                chunk.path.clone(),
                Item::Object {
                    position: &location.path,
                    object,
                },
            ))
        }
        Kind::RenderLayer => Err(Failure::MalformedId),
    }
}

/// The item's value in the re-read `document`, if it is still the
/// snapshot's item.
fn locate<'d>(document: &'d Document, item: &Item<'_>) -> Option<&'d Value> {
    let root = document.root();
    match item {
        Item::Document { canonical } => (document.canonical(root) == *canonical).then_some(root),
        Item::Entry {
            list,
            id,
            canonical,
        } => entry(root, list, id).filter(|value| document.canonical(value) == *canonical),
        Item::Anchor { id, anchor } => {
            entry(root, "anchors", id).filter(|value| same_anchor(value, anchor))
        }
        Item::Object { position, object } => {
            let (first, rest) = position.split_first()?;
            let value = rest.iter().try_fold(
                root.get("objects")?.as_array()?.get(*first)?,
                |value, index| value.get("inventory")?.as_array()?.get(*index),
            )?;
            same_object(document, value, object).then_some(value)
        }
    }
}

/// The first entry of the list `key` of `root` whose `id` is `id`: the one
/// the loader keeps when an ID repeats.
fn entry<'d>(root: &'d Value, key: &str, id: &str) -> Option<&'d Value> {
    root.get(key)?
        .as_array()?
        .iter()
        .find(|item| item.get("id").and_then(Value::as_str) == Some(id))
}

/// Whether `value`, an `[x, y]` pair, is `at`.
fn same_pair(value: &Value, at: [i64; 2]) -> bool {
    matches!(value.as_array(), Some([x, y]) if x.as_i64() == Some(at[0]) && y.as_i64() == Some(at[1]))
}

/// Whether `value` has only keys in `allowed`.
fn keys_within(value: &Value, allowed: &[&str]) -> bool {
    value.as_object().is_some_and(|members| {
        members
            .iter()
            .all(|member| allowed.contains(&member.key.as_str()))
    })
}

fn same_anchor(value: &Value, anchor: &Anchor) -> bool {
    keys_within(value, &["id", "layer", "at"])
        && value.get("layer").and_then(Value::as_i64) == Some(i64::from(anchor.layer))
        && value.get("at").is_some_and(|at| same_pair(at, anchor.at))
}

/// Whether the optional string member `key` of `value` is `expected`.
fn same_text(value: &Value, key: &str, expected: Option<&str>) -> bool {
    match value.get(key) {
        None => expected.is_none(),
        Some(found) => found.as_str().is_some() && found.as_str() == expected,
    }
}

/// Whether the optional integer member `key` of `value` is `expected`,
/// which an absent member gives as 0.
fn same_integer(value: &Value, key: &str, expected: i64) -> bool {
    value
        .get(key)
        .map_or(Some(0), Value::as_i64)
        .is_some_and(|found| found == expected)
}

/// Whether a field's value in the file is the value the library kept.
fn same_field(document: &Document, value: &Value, kept: &FieldValue) -> bool {
    match (kept, value) {
        (FieldValue::Null, Value::Null { .. }) => true,
        (FieldValue::Bool(kept), Value::Bool { value, .. }) => kept == value,
        (FieldValue::Integer(kept), Value::Integer { value, .. }) => kept == value,
        (FieldValue::String(kept), Value::String { value, .. }) => kept == value,
        (FieldValue::Number(kept), value) => value.as_f64() == Some(*kept),
        (FieldValue::Json(kept), Value::Array { .. } | Value::Object { .. }) => {
            document.canonical(value) == *kept
        }
        _ => false,
    }
}

/// Whether `value` holds the object the library kept: the same keys, and
/// every value it keeps equal, inventory entries included.
fn same_object(document: &Document, value: &Value, object: &Object) -> bool {
    const KEYS: &[&str] = &[
        "uid",
        "at",
        "proto",
        "layer",
        "sub_layer",
        "z",
        "fields",
        "portal",
        "inventory",
        "custom",
    ];
    if !keys_within(value, KEYS)
        || !same_text(value, "uid", object.uid.as_deref())
        || !same_text(value, "proto", Some(&object.proto))
        || !same_text(value, "layer", object.layer.as_deref())
        || !same_integer(value, "sub_layer", i64::from(object.sub_layer))
        || !same_integer(value, "z", i64::from(object.z))
    {
        return false;
    }
    let at = match (value.get("at"), object.at) {
        (None, None) => true,
        (Some(found), Some([x, y])) => same_pair(found, [i64::from(x), i64::from(y)]),
        _ => false,
    };
    let fields = match value.get("fields") {
        None => object.fields.is_empty(),
        Some(fields) => fields.as_object().is_some_and(|members| {
            members.len() == object.fields.len()
                && members
                    .iter()
                    .zip(&object.fields)
                    .all(|(member, (name, kept))| {
                        member.key == *name && same_field(document, &member.value, kept)
                    })
        }),
    };
    let portal = match (value.get("portal"), &object.portal) {
        (None, None) => true,
        (Some(found), Some(Portal::Location { plane, layer, at })) => {
            keys_within(found, &["plane", "layer", "at"])
                && same_text(found, "plane", plane.as_deref())
                && found.get("layer").and_then(Value::as_i64) == Some(i64::from(*layer))
                && found.get("at").is_some_and(|found| same_pair(found, *at))
        }
        (Some(found), Some(Portal::Anchor { plane, anchor })) => {
            keys_within(found, &["plane", "anchor"])
                && same_text(found, "plane", plane.as_deref())
                && same_text(found, "anchor", Some(anchor))
        }
        _ => false,
    };
    let inventory = match value.get("inventory") {
        None => object.inventory.is_empty(),
        Some(items) => items.as_array().is_some_and(|items| {
            items.len() == object.inventory.len()
                && items
                    .iter()
                    .zip(&object.inventory)
                    .all(|(item, kept)| same_object(document, item, kept))
        }),
    };
    let custom = match (value.get("custom"), &object.custom) {
        (None, None) => true,
        (Some(found), Some(kept)) => document.canonical(found) == *kept,
        _ => false,
    };
    at && fields && portal && inventory && custom
}

/// A 1-based line and column, in characters, as diagnostics give them.
fn position(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let start = before.rfind('\n').map_or(0, |newline| newline + 1);
    (line, before[start..].chars().count() + 1)
}

/// Writes a value as JSON within a budget of [`budget::cost`], strings cut
/// to [`MAXIMUM_STRING_BYTES`]. When the budget runs out the value is cut
/// short: every open array and object is closed, so the output stays one
/// JSON value.
struct Writer<'d> {
    document: &'d Document,
    output: Output,
    spent: usize,
    /// What closing the open arrays and objects will cost.
    closing: usize,
    budget: usize,
}

impl Writer<'_> {
    fn fits(&self, piece: &str) -> bool {
        self.spent + budget::cost(piece) + self.closing <= self.budget
    }

    fn push(&mut self, piece: &str) {
        self.spent += budget::cost(piece);
        self.output.raw(piece);
    }

    /// Writes `value`, or as much of it as fits: `false` when it was cut,
    /// leaving either nothing written or an array or object cut short.
    fn value(&mut self, value: &Value) -> bool {
        let (members, items) = match value {
            Value::Object { members, .. } => (Some(members.as_slice()), None),
            Value::Array { items, .. } => (None, Some(items.as_slice())),
            scalar => {
                let mut text = String::new();
                let mut cut_strings = 0;
                match scalar {
                    Value::String { value, .. } => {
                        let (value, cut) = clip(value, MAXIMUM_STRING_BYTES);
                        json_string(value, &mut text);
                        cut_strings = usize::from(cut);
                    }
                    Value::Null { .. } => text.push_str("null"),
                    Value::Bool { value, .. } => {
                        text.push_str(if *value { "true" } else { "false" });
                    }
                    number => text.push_str(self.document.text(number.span())),
                }
                if !self.fits(&text) {
                    return false;
                }
                self.push(&text);
                self.output.cut_strings += cut_strings;
                return true;
            }
        };
        let (open, close) = if members.is_some() {
            ("{", "}")
        } else {
            ("[", "]")
        };
        if !self.fits(&format!("{open}{close}")) {
            return false;
        }
        self.push(open);
        self.closing += budget::cost(close);
        let count = members.map_or_else(|| items.map_or(0, <[Value]>::len), <[_]>::len);
        let mut complete = true;
        for index in 0..count {
            let mark = (self.output.text.len(), self.spent);
            let mut separator = String::new();
            if index > 0 {
                separator.push(',');
            }
            let child = match (members, items) {
                (Some(members), _) => {
                    let member = &members[index];
                    // A key is never cut: two cut keys could be the same.
                    if member.key.len() > MAXIMUM_STRING_BYTES {
                        complete = false;
                        break;
                    }
                    json_string(&member.key, &mut separator);
                    separator.push(':');
                    &member.value
                }
                (None, Some(items)) => &items[index],
                (None, None) => break,
            };
            if !self.fits(&separator) {
                complete = false;
                break;
            }
            self.push(&separator);
            let written = self.output.text.len();
            if !self.value(child) {
                complete = false;
                // Nothing of the child was written: take back its key or
                // comma too.
                if self.output.text.len() == written {
                    self.output.text.truncate(mark.0);
                    self.spent = mark.1;
                }
                break;
            }
        }
        self.closing -= budget::cost(close);
        self.push(close);
        complete
    }
}

/// The `inspect` result of `id` of `kind`, read again from `root`, after
/// `revision`.
pub fn inspect(
    world: &World,
    revision: &str,
    root: &Path,
    kind: Kind,
    id: &str,
) -> Result<String, Failure> {
    let (path, item) = find(world, kind, id)?;
    // The loader's limits, so that the file reads as it read.
    let limits = Limits::default();
    let limit = u64::try_from(limits.max_input_bytes).unwrap_or(u64::MAX);
    let bytes = SourceRoot::new(root)
        .read(&path, limit)
        .ok()
        .flatten()
        .ok_or(Failure::Changed)?;
    let text = String::from_utf8(bytes).map_err(|_| Failure::Changed)?;
    let document = Document::parse(text, &limits).map_err(|_| Failure::Changed)?;
    let value = locate(&document, &item).ok_or(Failure::Changed)?;
    let span = value.span();
    let source = document.source();
    let (start_line, start_column) = position(source, span.start);
    let (end_line, end_column) = position(source, span.end);

    let mut output = Output::default();
    output.raw("{\"revision\":");
    output.string(revision);
    let _ = write!(output.text, ",\"kind\":\"{}\",\"id\":", kind.as_str());
    output.string(id);
    output.raw(",\"path\":");
    output.string(&path);
    let _ = write!(
        output.text,
        ",\"span\":{{\"start\":{},\"end\":{},\"start_line\":{start_line},\"start_column\":{start_column},\"end_line\":{end_line},\"end_column\":{end_column}}}",
        span.start, span.end
    );
    let canonical_bytes = document.canonical(value).len();
    // The fields after the value are small: room for them is kept.
    let tail = 256;
    let mut writer = Writer {
        document: &document,
        output: Output::default(),
        spent: 0,
        closing: 0,
        budget: PAGE_BYTES.saturating_sub(budget::cost(&output.text) + tail),
    };
    let complete = writer.value(value);
    output.raw(",\"value\":");
    if writer.output.text.is_empty() {
        // Not even an empty array or object fitted, which the budget
        // makes impossible; say so rather than send nothing.
        output.raw("null");
    } else {
        output.raw(&writer.output.text);
    }
    let _ = write!(
        output.text,
        ",\"value_truncated\":{},\"canonical_bytes\":{canonical_bytes},\"truncated_strings\":{}}}",
        !complete || writer.output.text.is_empty(),
        output.cut_strings + writer.output.cut_strings
    );
    Ok(output.text)
}
