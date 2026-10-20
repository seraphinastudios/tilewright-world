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

/// The identifiers of a list's items, in order: `None` for an item that is
/// not an identifier or repeats one before it. Every such item is reported,
/// so one bad member does not hide the problems of the rest.
fn each_identifier<'a>(reader: &Reader<'a>, items: &'a [Value]) -> Vec<Option<String>> {
    let mut seen = BTreeSet::new();
    items
        .iter()
        .map(|item| {
            let id = reader.identifier(item)?;
            if seen.insert(id.clone()) {
                Some(id)
            } else {
                reader.report(
                    item.span(),
                    Code::Duplicate,
                    format!("\"{id}\" is listed twice"),
                );
                None
            }
        })
        .collect()
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
    Some(Pattern::new(variants))
}

/// A pattern key: a canonical decimal integer from 0 to 255.
fn pattern_key(key: &str) -> Option<u8> {
    let canonical = !key.is_empty()
        && key.len() <= 3
        && key.bytes().all(|byte| byte.is_ascii_digit())
        && (key == "0" || !key.starts_with('0'));
    if canonical { key.parse().ok() } else { None }
}

/// Whether `pattern` may be a key of a transition's `tiles`: one its model
/// produces for its draw. Where the draw or the model is invalid, and has
/// been reported, the key is checked against every value the other allows,
/// so a key that no transition could have is still reported.
fn possible(draw: Option<Draw>, model: Option<Model>, pattern: u8) -> bool {
    let draws = draw
        .as_ref()
        .map_or(&[Draw::Inside, Draw::Outside][..], std::slice::from_ref);
    let models = model.as_ref().map_or(
        &[Model::None, Model::Sides, Model::Corners, Model::Blob][..],
        std::slice::from_ref,
    );
    draws
        .iter()
        .any(|draw| models.iter().any(|model| model.produces(*draw, pattern)))
}

/// What a transition reads its `with` against.
struct Terrains<'t> {
    /// Terrain indexes by ID.
    ids: &'t BTreeMap<String, usize>,
    count: usize,
    /// The terrain layer, which no transition draws into; `None` when the
    /// document's is invalid and has been reported.
    terrain_layer: Option<usize>,
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
    let layer = reader.required(entry, "layer").and_then(|value| {
        let layer = ground_layer(reader, profile, value)?;
        if Some(layer) == terrains.terrain_layer {
            reader.report(
                value.span(),
                Code::Range,
                "a transition draws into a ground layer other than the terrain layer, which keeps the painted terrain",
            );
            return None;
        }
        Some(layer)
    });
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
                let mut blends = vec![false; terrains.count];
                let mut ok = true;
                for (name, item) in each_identifier(reader, items).into_iter().zip(items) {
                    let Some(name) = name else {
                        ok = false;
                        continue;
                    };
                    match terrains.ids.get(&name) {
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
            let pattern =
                pattern_key(&member.key).filter(|pattern| possible(draw, model, *pattern));
            // The tile is checked whatever its key, so every problem is
            // reported at once.
            let tile = read_pattern(reader, references, &member.value);
            let Some(pattern) = pattern else {
                let message = match (draw, model) {
                    (Some(draw), Some(model)) => format!(
                        "\"{}\" is not a pattern of the {} model drawn {}",
                        member.key,
                        model.as_str(),
                        draw.as_str()
                    ),
                    (Some(draw), None) => format!(
                        "\"{}\" is not a pattern of any model drawn {}",
                        member.key,
                        draw.as_str()
                    ),
                    (None, Some(model)) => format!(
                        "\"{}\" is not a pattern of the {} model",
                        member.key,
                        model.as_str()
                    ),
                    (None, None) => format!("\"{}\" is not a pattern", member.key),
                };
                reader.report(member.key_span, Code::Range, message);
                ok = false;
                continue;
            };
            match tile {
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
    ok.then(|| Transition::new((terrain, draw, model, layer), blends, seed, tiles))
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
        terrain_layer,
    };
    let mut terrains = Vec::with_capacity(items.len());
    let mut transitions = Vec::new();
    let mut by_proto: BTreeMap<String, usize> = BTreeMap::new();
    // Transitions are counted as they are declared, whether or not they
    // parse, so the document's cap bounds the work of reading them.
    let mut declared = 0_usize;
    let mut capped = false;
    for (index, item) in items.iter().enumerate() {
        // An entry that is not an object has been reported and has no
        // members. One whose ID is invalid or repeated is still checked, so
        // every problem is reported at once.
        if item.as_object().is_none() {
            ok = false;
            continue;
        }
        let id = ids.get(index).cloned().flatten();
        let protos = reader.required(item, "protos").and_then(|value| {
            let items = reader.array(value)?;
            if !count_within(
                &reader,
                value,
                items.len(),
                (1, MAXIMUM_TERRAIN_PROTOS),
                "protos",
            ) {
                return None;
            }
            let mut names = Vec::with_capacity(items.len());
            let mut ok = true;
            for (name, item) in each_identifier(&reader, items).into_iter().zip(items) {
                let Some(name) = name else {
                    ok = false;
                    continue;
                };
                // A prototype is claimed whether or not it resolves, so a
                // repeat is reported in the same load as a missing one.
                if proto(&reader, references, item).is_none() {
                    ok = false;
                }
                if let Some(&other) = by_proto.get(&name) {
                    let owner = ids
                        .get(other)
                        .and_then(Option::as_deref)
                        .map_or_else(|| format!("number {}", other + 1), |id| format!("\"{id}\""));
                    reader.report(
                        item.span(),
                        Code::Duplicate,
                        format!("prototype \"{name}\" already paints terrain {owner}"),
                    );
                    ok = false;
                } else {
                    by_proto.insert(name.clone(), index);
                }
                names.push(name);
            }
            ok.then_some(names)
        });
        let start = transitions.len();
        if let Some(value) = item.get("transitions") {
            match reader.array(value) {
                Some(list) => {
                    // Every list counts toward the document's cap, one that
                    // breaks the terrain's cap too.
                    declared = declared.saturating_add(list.len());
                    let over_document = declared > MAXIMUM_TRANSITIONS;
                    if over_document && !capped {
                        reader.report(
                            value.span(),
                            Code::Range,
                            format!("a document has at most {MAXIMUM_TRANSITIONS} transitions"),
                        );
                        capped = true;
                    }
                    let within_terrain = count_within(
                        &reader,
                        value,
                        list.len(),
                        (0, MAXIMUM_TERRAIN_TRANSITIONS),
                        "transitions",
                    );
                    if over_document || !within_terrain {
                        ok = false;
                    } else {
                        for entry in list {
                            match read_transition(
                                &reader, profile, references, &lookup, index, entry,
                            ) {
                                Some(transition) => transitions.push(transition),
                                None => ok = false,
                            }
                        }
                    }
                }
                None => ok = false,
            }
        }
        match (id, protos) {
            (Some(id), Some(protos)) => terrains.push(Terrain {
                id,
                protos,
                transitions: start..transitions.len(),
            }),
            _ => ok = false,
        }
    }
    let terrain_layer = terrain_layer?;
    if !ok {
        return None;
    }
    Some(Transitions {
        path: path.to_owned(),
        terrain_layer,
        ground_layers: profile.ground_layers.len(),
        terrains,
        list: transitions,
        by_proto,
    })
}
