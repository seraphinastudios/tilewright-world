// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! `search_definitions` and `search_objects` (docs/MCP.md, "Searching
//! definitions and objects").
//!
//! A search walks its candidates in a fixed order and examines at most a
//! scan budget of them per call ([`budget::scan`]); a cursor resumes at the
//! first candidate not examined. Field values are the ones written on the
//! definition or object: an object's prototype's fields are not resolved.

use crate::budget::{self, Output, ScanPage, Work};
use std::fmt::Write as _;
use tilewright_jsonc::{Document, Limits, Value};
use tilewright_source::{Chunk, Definition, FieldValue, Object, World};

/// A scalar a field's value is compared with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scalar<'a> {
    Null,
    Bool(bool),
    Integer(i64),
    Float(f64),
    String(&'a str),
}

/// What a value must be, or hold.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Needle<'a> {
    /// The value is this scalar, or, for a list, object or JSON value,
    /// holds it at any depth.
    Equals(Scalar<'a>),
    /// The value is a string that contains this, or, for a list, object or
    /// JSON value, holds one at any depth.
    Contains(&'a str),
}

/// Whether two numbers are equal. Every integer the format holds is within
/// 2^53, where an `f64` is exact, and equality is meant exactly.
#[allow(clippy::cast_precision_loss, clippy::float_cmp)]
fn same_number(integer: i64, number: f64) -> bool {
    integer as f64 == number
}

impl Scalar<'_> {
    /// Whether a scalar of a parsed value is this one. Numbers are equal
    /// exactly, as the values a file spells.
    #[allow(clippy::float_cmp)]
    fn is(&self, value: &Value) -> bool {
        match (self, value) {
            (Self::Null, Value::Null { .. }) => true,
            (Self::Bool(expected), Value::Bool { value, .. }) => expected == value,
            (Self::Integer(expected), Value::Integer { value, .. }) => expected == value,
            (Self::Integer(expected), Value::Float { value, .. }) => same_number(*expected, *value),
            (Self::Float(expected), Value::Integer { value, .. }) => same_number(*value, *expected),
            (Self::Float(expected), Value::Float { value, .. }) => expected == value,
            (Self::String(expected), Value::String { value, .. }) => expected == value,
            _ => false,
        }
    }

    /// Whether a scalar field value is this one.
    #[allow(clippy::float_cmp)]
    fn is_field(&self, value: &FieldValue) -> bool {
        match (self, value) {
            (Self::Null, FieldValue::Null) => true,
            (Self::Bool(expected), FieldValue::Bool(value)) => expected == value,
            (Self::Integer(expected), FieldValue::Integer(value)) => expected == value,
            (Self::Integer(expected), FieldValue::Number(value)) => same_number(*expected, *value),
            (Self::Float(expected), FieldValue::Integer(value)) => same_number(*value, *expected),
            (Self::Float(expected), FieldValue::Number(value)) => expected == value,
            (Self::String(expected), FieldValue::String(value)) => expected == value,
            _ => false,
        }
    }
}

/// Limits for parsing a field's canonical JSON again: the format's, on one
/// line of any length the format allows.
fn json_limits() -> Limits {
    let format = Limits::default();
    Limits {
        max_lines: 1,
        max_line_bytes: format.max_input_bytes,
        max_comments: 0,
        ..format
    }
}

impl Needle<'_> {
    /// Whether a parsed scalar matches.
    fn matches_scalar(&self, value: &Value) -> bool {
        match self {
            Self::Equals(scalar) => scalar.is(value),
            Self::Contains(text) => value.as_str().is_some_and(|value| value.contains(text)),
        }
    }

    /// Whether a field's value matches, parsing a list, object or JSON
    /// value again to look at the scalars inside it, and counting what it
    /// parses in `work`.
    fn matches(&self, value: &FieldValue, work: &mut Work) -> bool {
        match value {
            FieldValue::Json(canonical) => {
                work.json_bytes = work.json_bytes.saturating_add(canonical.len());
                // Canonical JSON from a document that parsed parses again;
                // if it did not, nothing in it would match.
                let Ok(document) = Document::parse(canonical.as_str(), &json_limits()) else {
                    return false;
                };
                // Depth first, without recursion.
                let mut stack = vec![document.root()];
                while let Some(value) = stack.pop() {
                    match value {
                        Value::Array { items, .. } => stack.extend(items.iter().rev()),
                        Value::Object { members, .. } => {
                            stack.extend(members.iter().rev().map(|member| &member.value));
                        }
                        scalar => {
                            if self.matches_scalar(scalar) {
                                return true;
                            }
                        }
                    }
                }
                false
            }
            scalar => match self {
                Self::Equals(expected) => expected.is_field(scalar),
                Self::Contains(text) => {
                    matches!(scalar, FieldValue::String(value) if value.contains(text))
                }
            },
        }
    }
}

/// The value of the field `name` in `fields`.
fn field<'f>(fields: &'f [(String, FieldValue)], name: &str) -> Option<&'f FieldValue> {
    fields
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value)
}

/// Writes `"field":…,"value_kind":…,"value":…` for a matched field, or
/// nulls for a match without one.
fn write_field(output: &mut Output, matched: Option<(&str, &FieldValue)>) {
    let Some((name, value)) = matched else {
        output.raw(",\"field\":null,\"value_kind\":null,\"value\":null");
        return;
    };
    output.raw(",\"field\":");
    output.string(name);
    let kind = match value {
        FieldValue::Null => "null",
        FieldValue::Bool(_) => "boolean",
        FieldValue::Integer(_) => "integer",
        FieldValue::Number(_) => "number",
        FieldValue::String(_) => "string",
        FieldValue::Json(_) => "json",
    };
    let _ = write!(output.text, ",\"value_kind\":\"{kind}\",\"value\":");
    match value {
        FieldValue::Bool(value) => output.raw(if *value { "true" } else { "false" }),
        FieldValue::Integer(value) => {
            let _ = write!(output.text, "{value}");
        }
        // Rust writes a finite f64 as JSON reads it: digits, an optional
        // fraction, no exponent.
        FieldValue::Number(value) if value.is_finite() => {
            let _ = write!(output.text, "{value}");
        }
        // JSON has no other numbers, so neither null nor this is seen.
        FieldValue::Null | FieldValue::Number(_) => output.raw("null"),
        // The canonical JSON, as a string, cut like any other.
        FieldValue::String(value) | FieldValue::Json(value) => {
            output.string(value);
        }
    }
}

/// What `search_definitions` looks for.
pub struct DefinitionQuery<'a> {
    pub kind: Option<&'a str>,
    pub field: Option<&'a str>,
    pub needle: Option<Needle<'a>>,
}

/// One page of `search_definitions`: definitions sorted by type, then ID,
/// both by their bytes.
pub fn definitions<'w>(
    world: &'w World,
    query: &DefinitionQuery<'_>,
    offset: usize,
    limit: usize,
    scan_limit: usize,
) -> ScanPage {
    // A type's definitions are found by a range, not by walking the others.
    let candidates: Box<dyn Iterator<Item = (&'w (String, String), &'w Definition)>> =
        match query.kind {
            Some(wanted) => Box::new(
                world
                    .definitions()
                    .range((wanted.to_owned(), String::new())..)
                    .take_while(move |((kind, _), _)| kind == wanted),
            ),
            None => Box::new(world.definitions().iter()),
        };
    let matches = |candidate: &(&'w (String, String), &'w Definition), work: &mut Work| {
        let ((_, id), definition) = *candidate;
        match query.field {
            Some(name) => {
                let value = field(&definition.fields, name)?;
                query
                    .needle
                    .is_none_or(|needle| needle.matches(value, work))
                    .then_some(Some((name, value)))
            }
            None => query
                .needle
                .is_none_or(|needle| match needle {
                    Needle::Equals(Scalar::String(text)) => id == text,
                    Needle::Contains(text) => id.contains(text),
                    // Arguments refuse other needles on an ID.
                    Needle::Equals(_) => false,
                })
                .then_some(None),
        }
    };
    budget::scan(
        candidates,
        offset,
        limit,
        scan_limit,
        matches,
        |output, ((kind, id), _), matched| {
            output.raw("{\"type\":");
            output.string(kind);
            output.raw(",\"id\":");
            output.string(id);
            output.raw(",\"path\":");
            output.string(&format!("defs/{kind}/{id}.jsonc"));
            write_field(output, matched);
            output.raw("}");
        },
    )
}

/// What `search_objects` looks for.
pub struct ObjectQuery<'a> {
    pub plane: Option<&'a str>,
    pub layer: Option<i32>,
    /// `[x0, y0, x1, y1]`, inclusive, in plane coordinates.
    pub bbox: Option<[i64; 4]>,
    pub proto: Option<&'a str>,
    pub uid_prefix: Option<&'a str>,
    pub field: Option<&'a str>,
    pub needle: Option<Needle<'a>>,
    /// Whether inventory entries are candidates.
    pub carried: bool,
}

/// An object with where it is.
pub struct Placed<'w> {
    plane: &'w str,
    chunk: &'w Chunk,
    layer: i32,
    /// The plane coordinates of the object's tile, or of the tile of the
    /// object that carries it.
    tile: [i128; 2],
    object: &'w Object,
    /// Whether an object carries it, in its inventory.
    carried: bool,
    /// The uid of the object that carries it, if it has one.
    carried_by: Option<&'w str>,
    /// Its index in the chunk's objects, then in each inventory down to it.
    position: Vec<usize>,
}

/// The objects of one chunk, depth first, each object before its
/// inventory entries; inventories only when `carried`.
struct Walk<'w> {
    plane: &'w str,
    chunk: &'w Chunk,
    layer: i32,
    origin: [i128; 2],
    carried: bool,
    /// At each depth, the objects left, their container, and the index of
    /// the next one.
    levels: Vec<(std::slice::Iter<'w, Object>, Option<&'w Object>, usize)>,
    /// The tile of the top-level object being walked.
    tile: [i128; 2],
}

impl<'w> Iterator for Walk<'w> {
    type Item = Placed<'w>;

    fn next(&mut self) -> Option<Placed<'w>> {
        loop {
            let (objects, container, index) = self.levels.last_mut()?;
            let Some(object) = objects.next() else {
                self.levels.pop();
                continue;
            };
            let container = *container;
            *index += 1;
            if container.is_none() {
                let [x, y] = object.at.unwrap_or_default();
                self.tile = [
                    self.origin[0] + i128::from(x),
                    self.origin[1] + i128::from(y),
                ];
            }
            let position = self.levels.iter().map(|(_, _, next)| next - 1).collect();
            if self.carried && !object.inventory.is_empty() {
                self.levels.push((object.inventory.iter(), Some(object), 0));
            }
            return Some(Placed {
                plane: self.plane,
                chunk: self.chunk,
                layer: self.layer,
                tile: self.tile,
                object,
                carried: container.is_some(),
                carried_by: container.and_then(|container| container.uid.as_deref()),
                position,
            });
        }
    }
}

/// Whether the tiles of the chunk at `origin` of `size` meet `bbox`.
fn chunk_meets(origin: [i128; 2], size: u16, bbox: [i64; 4]) -> bool {
    let size = i128::from(size);
    origin[0] <= i128::from(bbox[2])
        && origin[0] + size > i128::from(bbox[0])
        && origin[1] <= i128::from(bbox[3])
        && origin[1] + size > i128::from(bbox[1])
}

/// The candidates of a query: the objects of each plane by ID, each chunk
/// by layer, then x, then y, and in each chunk depth first. Planes, layers
/// and chunks outside `plane`, `layer` and `bbox` are passed over without
/// being walked.
fn candidates<'w>(
    world: &'w World,
    query: &'w ObjectQuery<'_>,
) -> impl Iterator<Item = Placed<'w>> + 'w {
    world
        .planes()
        .iter()
        .filter(|(id, _)| query.plane.is_none_or(|plane| plane == id.as_str()))
        .flat_map(move |(id, plane)| {
            let size = plane.chunk_size;
            let chunks: Box<dyn Iterator<Item = _>> = match query.layer {
                Some(layer) => Box::new(
                    plane
                        .chunks
                        .range((layer, i64::MIN, i64::MIN)..=(layer, i64::MAX, i64::MAX)),
                ),
                None => Box::new(plane.chunks.iter()),
            };
            chunks.filter_map(move |(&(layer, cx, cy), chunk)| {
                let origin = [
                    i128::from(cx) * i128::from(size),
                    i128::from(cy) * i128::from(size),
                ];
                query
                    .bbox
                    .is_none_or(|bbox| chunk_meets(origin, size, bbox))
                    .then(|| Walk {
                        plane: id,
                        chunk,
                        layer,
                        origin,
                        carried: query.carried,
                        levels: vec![(chunk.objects.iter(), None, 0)],
                        tile: origin,
                    })
            })
        })
        .flatten()
}

/// One page of `search_objects`.
pub fn objects<'w>(
    world: &'w World,
    query: &'w ObjectQuery<'_>,
    offset: usize,
    limit: usize,
    scan_limit: usize,
) -> ScanPage {
    let matches = |placed: &Placed<'w>, work: &mut Work| {
        let object: &'w Object = placed.object;
        let inside = query.bbox.is_none_or(|[x0, y0, x1, y1]| {
            (i128::from(x0)..=i128::from(x1)).contains(&placed.tile[0])
                && (i128::from(y0)..=i128::from(y1)).contains(&placed.tile[1])
        });
        if !inside
            || query.proto.is_some_and(|proto| object.proto != proto)
            || query.uid_prefix.is_some_and(|prefix| {
                !object
                    .uid
                    .as_deref()
                    .is_some_and(|uid| uid.starts_with(prefix))
            })
        {
            return None;
        }
        match query.field {
            Some(name) => {
                let value = field(&object.fields, name)?;
                query
                    .needle
                    .is_none_or(|needle| needle.matches(value, work))
                    .then_some(Some((name, value)))
            }
            None => Some(None),
        }
    };
    budget::scan(
        candidates(world, query),
        offset,
        limit,
        scan_limit,
        matches,
        |output, placed, matched| {
            let object = placed.object;
            output.raw("{\"uid\":");
            match &object.uid {
                Some(uid) => {
                    output.string(uid);
                }
                None => output.raw("null"),
            }
            output.raw(",\"plane\":");
            output.string(placed.plane);
            let _ = write!(
                output.text,
                ",\"layer\":{},\"tile\":[{},{}],\"proto\":",
                placed.layer, placed.tile[0], placed.tile[1]
            );
            output.string(&object.proto);
            let _ = write!(
                output.text,
                ",\"carried\":{},\"carried_by\":",
                placed.carried
            );
            match placed.carried_by {
                Some(uid) => {
                    output.string(uid);
                }
                None => output.raw("null"),
            }
            output.raw(",\"path\":");
            output.string(&placed.chunk.path);
            output.raw(",\"position\":[");
            for (index, step) in placed.position.iter().enumerate() {
                if index > 0 {
                    output.raw(",");
                }
                let _ = write!(output.text, "{step}");
            }
            output.raw("]");
            write_field(output, matched);
            output.raw("}");
        },
    )
}
