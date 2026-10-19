// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Stairwells: `planes/<plane>/stairwells.jsonc`.

use crate::diagnostic::Diagnostics;
use crate::{
    Code, Stairwell, StairwellDirection, StairwellStep, adjacent,
    files::Tree,
    read::Reader,
    schema::{References, Target},
};
use std::collections::{BTreeMap, BTreeSet};
use tilewright_jsonc::Value;

/// Reads a plane's stairwells. Steps resolve their prototypes as they are
/// read, so definitions must already be listed.
pub(crate) fn read_stairwells(
    tree: &mut Tree<'_>,
    path: &str,
    references: &References,
    diagnostics: &mut Diagnostics,
) -> BTreeMap<String, Stairwell> {
    let mut stairwells = BTreeMap::new();
    let Some(source) = tree.document(path) else {
        return stairwells;
    };
    let reader = Reader::new(&source, diagnostics);
    let Some(root) = reader.object(reader.root(), &["format", "stairwells"]) else {
        return stairwells;
    };
    reader.format(root, "tilewright.stairwells/1");
    let Some(items) = reader
        .required(root, "stairwells")
        .and_then(|value| reader.array(value))
    else {
        return stairwells;
    };
    // Every valid ID so far; each parsed entry's ID, if valid; and, for each
    // tile holding a parsed step, its entry's position in `names`.
    // Duplicates are checked for every valid ID and overlaps for every step
    // that parses, so one problem never hides another, and messages only
    // name a valid, bounded ID.
    let mut ids = BTreeSet::new();
    let mut names: Vec<Option<String>> = Vec::new();
    let mut tiles: BTreeMap<(i32, i64, i64), usize> = BTreeMap::new();
    for item in items {
        let Some(entry) = read_entry(&reader, item, references) else {
            continue;
        };
        let position = names.len();
        names.push(entry.id.clone());
        let mut ok = entry.rest.is_some();
        if let Some(id) = &entry.id
            && !ids.insert(id.clone())
        {
            let span = item.get("id").map_or(item.span(), Value::span);
            reader.report(
                span,
                Code::Duplicate,
                format!("stairwell \"{id}\" is defined twice"),
            );
            ok = false;
        }
        for (value, (layer, at)) in &entry.steps.positions {
            let tile = (*layer, at[0], at[1]);
            if let Some(other) = tiles.get(&tile) {
                let other = names.get(*other).and_then(Option::as_ref).map_or_else(
                    || "an earlier stairwell".to_owned(),
                    |id| format!("stairwell \"{id}\""),
                );
                reader.report(
                    value.span(),
                    Code::StairwellOverlap,
                    format!(
                        "layer {} tile [{}, {}] already holds a step of {other}",
                        tile.0, tile.1, tile.2
                    ),
                );
                ok = false;
            } else {
                tiles.insert(tile, position);
            }
        }
        if let (true, Some(id), Some(steps), Some((direction, hidden_on_other_layers, custom))) =
            (ok, entry.id, entry.steps.steps, entry.rest)
        {
            stairwells.insert(
                id.clone(),
                Stairwell {
                    id,
                    steps,
                    direction,
                    hidden_on_other_layers,
                    custom,
                    canonical: reader.canonical(item),
                },
            );
        }
    }
    stairwells
}

/// The parts of a stairwell entry: its ID and the rest (direction, flag and
/// custom data), each `None` when invalid, and its steps.
struct Entry<'a> {
    id: Option<String>,
    steps: Steps<'a>,
    rest: Option<(StairwellDirection, bool, Option<String>)>,
}

/// A step's layer and plane coordinates.
type Position = (i32, [i64; 2]);

/// The steps of an entry: the position of every step whose position
/// parses, with its value, for overlap checks; and the steps themselves
/// when they are complete and connected.
#[derive(Default)]
struct Steps<'a> {
    positions: Vec<(&'a Value, Position)>,
    steps: Option<Vec<StairwellStep>>,
}

/// Reads one entry, reporting every problem in it. `None` when it is not an
/// object.
fn read_entry<'a>(
    reader: &Reader<'a>,
    item: &'a Value,
    references: &References,
) -> Option<Entry<'a>> {
    let entry = reader.object(
        item,
        &[
            "id",
            "steps",
            "direction",
            "hidden_on_other_layers",
            "custom",
        ],
    )?;
    let id = reader
        .required(entry, "id")
        .and_then(|value| reader.identifier(value));
    let direction = match entry.get("direction") {
        None => Some(StairwellDirection::Both),
        Some(value) => {
            let direction = reader.string(value).and_then(StairwellDirection::parse);
            if direction.is_none() && value.as_str().is_some() {
                reader.report(
                    value.span(),
                    Code::Range,
                    "direction must be \"both\", \"down\" or \"up\"",
                );
            }
            direction
        }
    };
    let hidden_on_other_layers = match entry.get("hidden_on_other_layers") {
        None => Some(true),
        Some(value) => reader.boolean(value),
    };
    let steps = reader
        .required(entry, "steps")
        .map(|value| read_steps(reader, value, references))
        .unwrap_or_default();
    // `custom` reports its own problems, which reject the tree.
    let custom = reader.custom(entry);
    let rest = direction
        .zip(hidden_on_other_layers)
        .map(|(direction, hidden)| (direction, hidden, custom));
    Some(Entry { id, steps, rest })
}

/// Reads every step. Each step's position (`layer` and `at`) is checked
/// against the position before it whenever both parse, and its prototype on
/// its own. The steps are complete when there are at least two, every one
/// parses, and each follows the one before it.
fn read_steps<'a>(reader: &Reader<'a>, value: &'a Value, references: &References) -> Steps<'a> {
    let Some(items) = reader.array(value) else {
        return Steps::default();
    };
    let mut complete = items.len() >= 2;
    if !complete {
        reader.report(
            value.span(),
            Code::Range,
            "a stairwell has at least two steps",
        );
    }
    let mut positions: Vec<(&Value, Position)> = Vec::with_capacity(items.len());
    let mut steps = Vec::with_capacity(items.len());
    // Whether the steps go up; the first two adjacent positions set it.
    let mut up = None;
    // The position of the item just before this one, if it parsed.
    let mut previous: Option<Position> = None;
    for item in items {
        let (position, proto) = read_step(reader, item, references);
        let Some(position) = position else {
            complete = false;
            previous = None;
            continue;
        };
        if let Some(before) = previous {
            let going_up = up.unwrap_or(position.0 > before.0);
            if adjacent(before, position, going_up) {
                up = Some(going_up);
            } else {
                reader.report(
                    item.span(),
                    Code::StairwellStep,
                    "each step is on the layer next to the step before it, in the stairwell's direction, and at most one tile away on each axis",
                );
                complete = false;
            }
        }
        previous = Some(position);
        positions.push((item, position));
        match proto {
            Some(proto) => steps.push(StairwellStep {
                layer: position.0,
                at: position.1,
                proto,
            }),
            None => complete = false,
        }
    }
    Steps {
        positions,
        steps: complete.then_some(steps),
    }
}

/// Reads one step, checking each of its members: its position (`layer` and
/// `at`) and its prototype, each `None` when invalid.
fn read_step<'a>(
    reader: &Reader<'a>,
    item: &'a Value,
    references: &References,
) -> (Option<Position>, Option<String>) {
    let Some(step) = reader.object(item, &["layer", "at", "proto"]) else {
        return (None, None);
    };
    let layer = reader
        .required(step, "layer")
        .and_then(|value| reader.i32(value));
    let at = reader
        .required(step, "at")
        .and_then(|value| reader.coordinates(value));
    let proto = reader.required(step, "proto").and_then(|value| {
        let proto = reader.identifier(value)?;
        reader.reference(
            references,
            &Target::Definition {
                kind: "proto".to_owned(),
                id: proto.clone(),
            },
            value.span(),
        );
        Some(proto)
    });
    (layer.zip(at), proto)
}
