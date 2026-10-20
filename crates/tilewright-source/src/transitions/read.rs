// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Reading a transitions document (`tilewright.transitions/1`).

use super::{
    Draw, MAXIMUM_TERRAIN_PROTOS, MAXIMUM_TERRAIN_TRANSITIONS, MAXIMUM_TERRAINS,
    MAXIMUM_TRANSITIONS, MAXIMUM_VARIANTS, MAXIMUM_WEIGHT, Model, Pattern, TRANSITIONS_FORMAT,
    Terrain, Transition, Transitions, Variant,
};
use crate::diagnostic::Diagnostics;
use crate::{
    Code, Profile,
    files::Tree,
    read::Reader,
    schema::{References, Target},
};
use std::collections::{BTreeMap, BTreeSet};
use tilewright_jsonc::Value;

/// The index of the ground layer `value` names.
fn ground_layer<'a>(reader: &Reader<'a>, profile: &Profile, value: &'a Value) -> Option<usize> {
    let name = reader.identifier(value)?;
    let index = profile
        .ground_layers
        .iter()
        .position(|layer| *layer == name);
    if index.is_none() {
        reader.report(
            value.span(),
            Code::Reference,
            format!("\"{name}\" is not a ground layer of the profile"),
        );
    }
    index
}

/// A `proto` definition named by `value`.
fn proto<'a>(reader: &Reader<'a>, references: &References, value: &'a Value) -> Option<String> {
    let id = reader.identifier(value)?;
    let target = Target::Definition {
        kind: "proto".to_owned(),
        id: id.clone(),
    };
    if references.resolves(&target) {
        Some(id)
    } else {
        reader.report(value.span(), Code::Reference, target.message());
        None
    }
}

/// Whether `count` items are within `minimum..=maximum`; reports a list
/// that is not.
fn count_within<'a>(
    reader: &Reader<'a>,
    value: &'a Value,
    count: usize,
    (minimum, maximum): (usize, usize),
    what: &str,
) -> bool {
    let within = (minimum..=maximum).contains(&count);
    if !within {
        reader.report(
            value.span(),
            Code::Range,
            format!("{what} must list {minimum} to {maximum} entries"),
        );
    }
    within
}

/// One variant: a prototype ID, or `{"proto": id, "weight": n}`.
fn read_variant<'a>(
    reader: &Reader<'a>,
    references: &References,
    value: &'a Value,
) -> Option<Variant> {
    match value {
        Value::String { .. } => Some(Variant {
            proto: proto(reader, references, value)?,
            weight: 1,
        }),
        Value::Object { .. } => {
            let entry = reader.object(value, &["proto", "weight"])?;
            let proto_value = reader.required(entry, "proto");
            let weight = match entry.get("weight") {
                Some(weight) => reader
                    .integer(weight, 1, i64::from(MAXIMUM_WEIGHT))
                    .and_then(|weight| u32::try_from(weight).ok()),
                None => Some(1),
            };
            Some(Variant {
                proto: proto(reader, references, proto_value?)?,
                weight: weight?,
            })
        }
        _ => {
            reader.report(
                value.span(),
                Code::Type,
                format!(
                    "expected a prototype ID or {{\"proto\", \"weight\"}}, found {}",
                    value.kind()
                ),
            );
            None
        }
    }
}

/// A pattern's tile: one variant, or a list of them.
fn read_pattern<'a>(
    reader: &Reader<'a>,
    references: &References,
    value: &'a Value,
) -> Option<Pattern> {
    let variants = match value {
        Value::Array { items, .. } => {
            if !count_within(
                reader,
                value,
                items.len(),
                (1, MAXIMUM_VARIANTS),
                "a pattern",
            ) {
                return None;
            }
            let mut variants = Vec::with_capacity(items.len());
            let mut ok = true;
            for item in items {
                match read_variant(reader, references, item) {
                    Some(variant) => variants.push(variant),
                    None => ok = false,
                }
            }
            ok.then_some(variants)?
        }
        _ => vec![read_variant(reader, references, value)?],
    };
    let total = variants
        .iter()
        .map(|variant| u64::from(variant.weight))
        .sum();
    Some(Pattern { variants, total })
}

/// A pattern key: a canonical decimal integer from 0 to 255.
fn pattern_key(key: &str) -> Option<u8> {
    let canonical = !key.is_empty()
        && key.len() <= 3
        && key.bytes().all(|byte| byte.is_ascii_digit())
        && (key == "0" || !key.starts_with('0'));
    if canonical { key.parse().ok() } else { None }
}

/// What a transition reads its `with` against.
struct Terrains<'t> {
    /// Terrain indexes by ID.
    ids: &'t BTreeMap<String, usize>,
    count: usize,
}

#[allow(clippy::too_many_lines)]
fn read_transition<'a>(
    reader: &Reader<'a>,
    profile: &Profile,
    references: &References,
    terrains: &Terrains<'_>,
    terrain: usize,
    value: &'a Value,
) -> Option<Transition> {
    let entry = reader.object(value, &["draw", "model", "layer", "with", "seed", "tiles"])?;
    let mut ok = true;
    let draw_value = reader.required(entry, "draw");
    let draw = draw_value.and_then(|value| {
        let draw = Draw::parse(reader.string(value)?);
        if draw.is_none() {
            reader.report(
                value.span(),
                Code::Range,
                "draw must be \"inside\" or \"outside\"",
            );
        }
        draw
    });
    let model_value = reader.required(entry, "model");
    let model = model_value.and_then(|value| {
        let model = Model::parse(reader.string(value)?);
        if model.is_none() {
            reader.report(
                value.span(),
                Code::Range,
                "model must be \"none\", \"sides\", \"corners\" or \"blob\"",
            );
        }
        model
    });
    if let (Some(Draw::Outside), Some(Model::None), Some(value)) = (draw, model, model_value) {
        reader.report(
            value.span(),
            Code::Range,
            "an outside transition reads its neighbours: its model must be \"sides\", \"corners\" or \"blob\"",
        );
        ok = false;
    }
    let layer = reader
        .required(entry, "layer")
        .and_then(|value| ground_layer(reader, profile, value));
    let seed = match entry.get("seed") {
        Some(value) => reader.u32(value, 0),
        None => Some(0),
    };
    let blends = match entry.get("with") {
        Some(value) => {
            let listed = reader.array(value).and_then(|items| {
                count_within(reader, value, items.len(), (1, MAXIMUM_TERRAINS), "with")
                    .then_some(items)
            });
            listed.and_then(|items| {
                let names = reader.identifiers(value)?;
                let mut blends = vec![false; terrains.count];
                let mut ok = true;
                for (name, item) in names.iter().zip(items) {
                    match terrains.ids.get(name) {
                        Some(&index) if index == terrain => {
                            reader.report(
                                item.span(),
                                Code::Range,
                                "a transition cannot blend with its own terrain",
                            );
                            ok = false;
                        }
                        Some(&index) => {
                            if let Some(slot) = blends.get_mut(index) {
                                *slot = true;
                            }
                        }
                        None => {
                            reader.report(
                                item.span(),
                                Code::Reference,
                                format!("no terrain \"{name}\""),
                            );
                            ok = false;
                        }
                    }
                }
                ok.then_some(blends)
            })
        }
        // Inside, a terrain blends with every other; outside, it draws
        // onto every terrain of lower precedence.
        None => draw.map(|draw| {
            (0..terrains.count)
                .map(|index| match draw {
                    Draw::Inside => index != terrain,
                    Draw::Outside => index < terrain,
                })
                .collect()
        }),
    };
    if draw == Some(Draw::Outside)
        && entry.get("with").is_none()
        && blends
            .as_ref()
            .is_some_and(|blends| !blends.contains(&true))
    {
        reader.report(
            entry.span(),
            Code::MissingKey,
            "an outside transition of the first terrain must list the terrains it draws onto in \"with\"",
        );
        ok = false;
    }
    let tiles = reader.required(entry, "tiles").and_then(|value| {
        let members = reader.members(value)?;
        if members.is_empty() {
            reader.report(
                value.span(),
                Code::Range,
                "tiles must map at least one pattern",
            );
            return None;
        }
        let mut tiles = BTreeMap::new();
        let mut ok = true;
        for member in members {
            let pattern = pattern_key(&member.key).filter(|pattern| match (draw, model) {
                (Some(draw), Some(model)) => model.produces(draw, *pattern),
                // Checked once the draw and model are known.
                _ => true,
            });
            let Some(pattern) = pattern else {
                let message = match (draw, model) {
                    (Some(draw), Some(model)) => format!(
                        "\"{}\" is not a pattern of the {} model drawn {}",
                        member.key,
                        model.as_str(),
                        draw.as_str()
                    ),
                    _ => format!("\"{}\" is not a pattern", member.key),
                };
                reader.report(member.key_span, Code::Range, message);
                ok = false;
                continue;
            };
            match read_pattern(reader, references, &member.value) {
                Some(tile) => {
                    tiles.insert(pattern, tile);
                }
                None => ok = false,
            }
        }
        ok.then_some(tiles)
    });
    let (Some(draw), Some(model), Some(layer), Some(seed), Some(blends), Some(tiles)) =
        (draw, model, layer, seed, blends, tiles)
    else {
        return None;
    };
    ok.then_some(Transition {
        terrain,
        draw,
        model,
        layer,
        blends,
        seed,
        tiles,
    })
}

/// Reads the terrain IDs of a document, by index; `None` where an entry has
/// no valid one. Every problem is reported.
fn read_ids<'a>(reader: &Reader<'a>, items: &'a [Value]) -> (Vec<Option<String>>, bool) {
    let mut ids = Vec::with_capacity(items.len());
    let mut seen = BTreeSet::new();
    let mut ok = true;
    for item in items {
        let id = reader
            .object(item, &["id", "protos", "transitions"])
            .and_then(|entry| reader.required(entry, "id"))
            .and_then(|value| {
                let id = reader.identifier(value)?;
                if !seen.insert(id.clone()) {
                    reader.report(
                        value.span(),
                        Code::Duplicate,
                        format!("terrain \"{id}\" is declared twice"),
                    );
                    return None;
                }
                Some(id)
            });
        ok &= id.is_some();
        ids.push(id);
    }
    (ids, ok)
}

/// Reads the transitions document at `path`, after the definitions are
/// listed, so that prototypes resolve as they are read.
#[allow(clippy::too_many_lines)]
pub(crate) fn read_transitions(
    tree: &mut Tree<'_>,
    path: &str,
    profile: &Profile,
    references: &References,
    diagnostics: &mut Diagnostics,
) -> Option<Transitions> {
    let source = tree.document(path)?;
    let reader = Reader::new(&source, diagnostics);
    let root = reader.object(reader.root(), &["format", "layer", "terrains"])?;
    let mut ok = reader.format(root, TRANSITIONS_FORMAT);
    let terrain_layer = reader
        .required(root, "layer")
        .and_then(|value| ground_layer(&reader, profile, value));
    ok &= terrain_layer.is_some();
    let terrains_value = reader.required(root, "terrains")?;
    let items = reader.array(terrains_value)?;
    // The list's length is capped before its entries are read.
    if !count_within(
        &reader,
        terrains_value,
        items.len(),
        (1, MAXIMUM_TERRAINS),
        "terrains",
    ) {
        return None;
    }
    // IDs are read first, so a transition may blend with a terrain listed
    // after its own.
    let (ids, ids_ok) = read_ids(&reader, items);
    ok &= ids_ok;
    let by_id: BTreeMap<String, usize> = ids
        .iter()
        .enumerate()
        .filter_map(|(index, id)| Some((id.clone()?, index)))
        .collect();
    let lookup = Terrains {
        ids: &by_id,
        count: items.len(),
    };
    let mut terrains = Vec::with_capacity(items.len());
    let mut transitions = Vec::new();
    let mut by_proto: BTreeMap<String, usize> = BTreeMap::new();
    for (index, (item, id)) in items.iter().zip(ids).enumerate() {
        let Some(id) = id else {
            continue;
        };
        let protos = reader.required(item, "protos").and_then(|value| {
            let count = reader.array(value)?.len();
            if !count_within(&reader, value, count, (1, MAXIMUM_TERRAIN_PROTOS), "protos") {
                return None;
            }
            let names = reader.identifiers(value)?;
            let mut ok = true;
            for (name, item) in names.iter().zip(value.as_array().unwrap_or_default()) {
                if proto(&reader, references, item).is_none() {
                    ok = false;
                } else if let Some(&other) = by_proto.get(name) {
                    let other = by_id
                        .iter()
                        .find_map(|(id, position)| (*position == other).then_some(id.as_str()))
                        .unwrap_or_default();
                    reader.report(
                        item.span(),
                        Code::Duplicate,
                        format!("prototype \"{name}\" already paints terrain \"{other}\""),
                    );
                    ok = false;
                } else {
                    by_proto.insert(name.clone(), index);
                }
            }
            ok.then_some(names)
        });
        let start = transitions.len();
        if let Some(value) = item.get("transitions") {
            match reader.array(value) {
                Some(list)
                    if !count_within(
                        &reader,
                        value,
                        list.len(),
                        (0, MAXIMUM_TERRAIN_TRANSITIONS),
                        "transitions",
                    ) =>
                {
                    ok = false;
                }
                Some(list) if start + list.len() > MAXIMUM_TRANSITIONS => {
                    reader.report(
                        value.span(),
                        Code::Range,
                        format!("a document has at most {MAXIMUM_TRANSITIONS} transitions"),
                    );
                    ok = false;
                }
                Some(list) => {
                    for entry in list {
                        match read_transition(&reader, profile, references, &lookup, index, entry) {
                            Some(transition) => transitions.push(transition),
                            None => ok = false,
                        }
                    }
                }
                None => ok = false,
            }
        }
        match protos {
            Some(protos) => terrains.push(Terrain {
                id,
                protos,
                transitions: start..transitions.len(),
            }),
            None => ok = false,
        }
    }
    let terrain_layer = terrain_layer?;
    if !ok {
        return None;
    }
    let mut drawn_on = vec![Vec::new(); terrains.len()];
    for (position, transition) in transitions.iter().enumerate() {
        match transition.draw {
            Draw::Inside => {
                if let Some(list) = drawn_on.get_mut(transition.terrain) {
                    list.push(position);
                }
            }
            Draw::Outside => {
                for (terrain, list) in drawn_on.iter_mut().enumerate() {
                    if transition.blends_with(terrain) {
                        list.push(position);
                    }
                }
            }
        }
    }
    Some(Transitions {
        path: path.to_owned(),
        terrain_layer,
        ground_layers: profile.ground_layers.len(),
        terrains,
        list: transitions,
        by_proto,
        drawn_on,
    })
}
