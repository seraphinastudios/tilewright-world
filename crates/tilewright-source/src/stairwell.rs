// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Stairwells: `planes/<plane>/stairwells.jsonc`.

use crate::diagnostic::Diagnostics;
use crate::{
    Code, Stairwell, StairwellDirection, StairwellStep,
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
    // Every valid ID so far, and the entry, by position in `items`, whose
    // step holds each tile across the plane. IDs are checked for every
    // entry whose ID is valid, and tiles for every entry whose steps are,
    // so one problem never hides another.
    let mut ids = BTreeSet::new();
    let mut tiles: BTreeMap<(i32, i64, i64), usize> = BTreeMap::new();
    for (position, item) in items.iter().enumerate() {
        let Some(entry) = read_entry(&reader, item, references) else {
            continue;
        };
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
        let values = item.get("steps").and_then(Value::as_array).unwrap_or(&[]);
        for (step, value) in entry.steps.iter().flatten().zip(values) {
            let tile = (step.layer, step.at[0], step.at[1]);
            if let Some(other) = tiles.get(&tile) {
                let other = items
                    .get(*other)
                    .and_then(|entry| entry.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                reader.report(
                    value.span(),
                    Code::StairwellOverlap,
                    format!(
                        "layer {} tile [{}, {}] already holds a step of stairwell \"{other}\"",
                        tile.0, tile.1, tile.2
                    ),
                );
                ok = false;
            } else {
                tiles.insert(tile, position);
            }
        }
        if let (true, Some(id), Some(steps), Some((direction, hidden_on_other_layers, custom))) =
            (ok, entry.id, entry.steps, entry.rest)
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

/// The parts of a stairwell entry, each `None` when it is invalid: its ID,
/// its steps, and the rest (direction, flag and custom data).
struct Entry {
    id: Option<String>,
    steps: Option<Vec<StairwellStep>>,
    rest: Option<(StairwellDirection, bool, Option<String>)>,
}

/// Reads one entry, reporting every problem in it. `None` when it is not an
/// object.
fn read_entry<'a>(reader: &Reader<'a>, item: &'a Value, references: &References) -> Option<Entry> {
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
        .and_then(|value| read_steps(reader, value, references));
    // `custom` reports its own problems, which reject the tree.
    let custom = reader.custom(entry);
    let rest = direction
        .zip(hidden_on_other_layers)
        .map(|(direction, hidden)| (direction, hidden, custom));
    Some(Entry { id, steps, rest })
}

/// Reads at least two steps, each following the one before it.
fn read_steps<'a>(
    reader: &Reader<'a>,
    value: &'a Value,
    references: &References,
) -> Option<Vec<StairwellStep>> {
    let items = reader.array(value)?;
    // Too few steps is reported, and each step is still checked.
    let mut ok = items.len() >= 2;
    if !ok {
        reader.report(
            value.span(),
            Code::Range,
            "a stairwell has at least two steps",
        );
    }
    let mut steps: Vec<StairwellStep> = Vec::with_capacity(items.len());
    // Whether the steps go up; the first two adjacent steps set it.
    let mut up = None;
    // Whether the last step pushed is the one just before this item.
    let mut previous_read = false;
    for item in items {
        let Some(step) = read_step(reader, item, references) else {
            ok = false;
            previous_read = false;
            continue;
        };
        if let Some(previous) = steps.last().filter(|_| previous_read) {
            let going_up = up.unwrap_or(step.layer > previous.layer);
            if previous.leads_to(&step, going_up) {
                up = Some(going_up);
            } else {
                reader.report(
                    item.span(),
                    Code::StairwellStep,
                    "each step is on the layer next to the step before it, in the stairwell's direction, and at most one tile away on each axis",
                );
                ok = false;
            }
        }
        steps.push(step);
        previous_read = true;
    }
    ok.then_some(steps)
}

fn read_step<'a>(
    reader: &Reader<'a>,
    item: &'a Value,
    references: &References,
) -> Option<StairwellStep> {
    let step = reader.object(item, &["layer", "at", "proto"])?;
    let layer = reader.i32(reader.required(step, "layer")?);
    let at = reader.coordinates(reader.required(step, "at")?);
    let proto_value = reader.required(step, "proto")?;
    let proto = reader.identifier(proto_value)?;
    reader.reference(
        references,
        &Target::Definition {
            kind: "proto".to_owned(),
            id: proto.clone(),
        },
        proto_value.span(),
    );
    Some(StairwellStep {
        layer: layer?,
        at: at?,
        proto,
    })
}
