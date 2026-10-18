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
use std::collections::BTreeMap;
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
    // The entry, by position in `items`, whose step holds each tile, across
    // the plane.
    let mut tiles: BTreeMap<(i32, i64, i64), usize> = BTreeMap::new();
    for (position, item) in items.iter().enumerate() {
        let Some(stairwell) = read_stairwell(&reader, item, references) else {
            continue;
        };
        if stairwells.contains_key(&stairwell.id) {
            let span = item.get("id").map_or(item.span(), Value::span);
            reader.report(
                span,
                Code::Duplicate,
                format!("stairwell \"{}\" is defined twice", stairwell.id),
            );
            continue;
        }
        let mut ok = true;
        let steps = item.get("steps").and_then(Value::as_array).unwrap_or(&[]);
        for (step, value) in stairwell.steps.iter().zip(steps) {
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
        if ok {
            stairwells.insert(stairwell.id.clone(), stairwell);
        }
    }
    stairwells
}

fn read_stairwell<'a>(
    reader: &Reader<'a>,
    item: &'a Value,
    references: &References,
) -> Option<Stairwell> {
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
    let id = reader.identifier(reader.required(entry, "id")?);
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
    let steps = read_steps(reader, reader.required(entry, "steps")?, references);
    let custom = reader.custom(entry);
    Some(Stairwell {
        id: id?,
        steps: steps?,
        direction: direction?,
        hidden_on_other_layers: hidden_on_other_layers?,
        custom,
        canonical: reader.canonical(entry),
    })
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
