// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Planes, their zones and anchors, and the chunk tree.

use crate::diagnostic::Diagnostics;
use crate::{
    Anchor, CHUNK_SIZE, Code, Diagnostic, Plane, PlaneKind, Profile, Zone,
    chunk::{ChunkAddress, read_chunk},
    files::{EntryKind, Tree, join},
    read::Reader,
    schema::{References, read_fields},
    stairwell::read_stairwells,
};
use std::collections::{BTreeMap, BTreeSet};
use tilewright_jsonc::{MAX_SAFE_INTEGER, Span};

/// Parses a canonical decimal integer: no sign other than a leading `-`, no
/// leading zeros, and no `-0`.
pub(crate) fn canonical_integer(text: &str) -> Option<i64> {
    let digits = text.strip_prefix('-').unwrap_or(text);
    let canonical = !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_digit())
        && (digits == "0" || !digits.starts_with('0'))
        && text != "-0";
    if canonical { text.parse().ok() } else { None }
}

pub(crate) fn read_planes(
    tree: &mut Tree<'_>,
    profile: &Profile,
    plane_ids: &[String],
    references: &mut References,
    diagnostics: &mut Diagnostics,
) -> Vec<Plane> {
    let listed: BTreeSet<&str> = plane_ids.iter().map(String::as_str).collect();
    let present: BTreeSet<String> = if tree.container("planes") {
        tree.list("planes")
            .into_iter()
            .filter_map(|entry| {
                if entry.kind == EntryKind::Directory && listed.contains(entry.name.as_str()) {
                    Some(entry.name)
                } else {
                    diagnostics.push(Diagnostic::file(
                        &join("planes", &entry.name),
                        Code::UnexpectedFile,
                        "not a plane listed in world.jsonc",
                    ));
                    None
                }
            })
            .collect()
    } else {
        BTreeSet::new()
    };
    // Every plane's documents and anchors are read before any chunk, so
    // portal planes and anchors resolve as chunks are read.
    let mut planes = Vec::new();
    for id in plane_ids {
        let directory = join("planes", id);
        if !present.contains(id) {
            diagnostics.push(Diagnostic::file(
                &join(&directory, "plane.jsonc"),
                Code::MissingFile,
                format!("plane \"{id}\" is listed in world.jsonc but has no plane.jsonc"),
            ));
            continue;
        }
        if let Some((plane, has_layers)) =
            read_plane(tree, profile, id, &directory, references, diagnostics)
        {
            references.plane_read(&plane);
            planes.push((plane, has_layers, directory));
        }
    }
    planes
        .into_iter()
        .map(|(mut plane, has_layers, directory)| {
            if has_layers {
                read_layers(
                    tree,
                    profile,
                    &mut plane,
                    &directory,
                    references,
                    diagnostics,
                );
            }
            plane
        })
        .collect()
}

/// Reads a plane's own documents: the plane, its zones, its anchors and its
/// stairwells.
/// Returns the plane and whether it has a `layers` directory.
fn read_plane(
    tree: &mut Tree<'_>,
    profile: &Profile,
    id: &str,
    directory: &str,
    references: &mut References,
    diagnostics: &mut Diagnostics,
) -> Option<(Plane, bool)> {
    let mut has_layers = false;
    for entry in tree.list(directory) {
        match (entry.name.as_str(), entry.kind) {
            (
                "plane.jsonc" | "zones.jsonc" | "anchors.jsonc" | "stairwells.jsonc",
                EntryKind::File,
            ) => {}
            ("layers", EntryKind::Directory) => has_layers = true,
            _ => diagnostics.push(Diagnostic::file(
                &join(directory, &entry.name),
                Code::UnexpectedFile,
                "not part of the plane layout",
            )),
        }
    }
    let mut plane = read_plane_document(tree, id, directory, diagnostics)?;
    let zones_path = join(directory, "zones.jsonc");
    if tree.kind(&zones_path).is_some() {
        plane.zones = read_zones(tree, profile, &zones_path, references, diagnostics);
    }
    let anchors_path = join(directory, "anchors.jsonc");
    if tree.kind(&anchors_path).is_some() {
        plane.anchors = read_anchors(tree, &anchors_path, diagnostics);
    }
    let stairwells_path = join(directory, "stairwells.jsonc");
    if tree.kind(&stairwells_path).is_some() {
        plane.stairwells = read_stairwells(tree, &stairwells_path, references, diagnostics);
    }
    if let Some(zone) = &plane.default_zone
        && !plane.zones.contains_key(zone)
    {
        diagnostics.push(Diagnostic::file(
            &join(directory, "plane.jsonc"),
            Code::Reference,
            format!("default zone \"{zone}\" is not in zones.jsonc"),
        ));
    }
    Some((plane, has_layers))
}

fn read_plane_document(
    tree: &mut Tree<'_>,
    id: &str,
    directory: &str,
    diagnostics: &mut Diagnostics,
) -> Option<Plane> {
    let source = tree.document(&join(directory, "plane.jsonc"))?;
    let reader = Reader::new(&source, diagnostics);
    let root = reader.object(
        reader.root(),
        &[
            "format",
            "id",
            "title",
            "kind",
            "chunk_size",
            "default_zone",
            "custom",
        ],
    )?;
    let format_ok = reader.format(root, "tilewright.plane/1");
    let id_value = reader.required(root, "id")?;
    let plane_id = reader.identifier(id_value)?;
    if plane_id != id {
        reader.report(
            id_value.span(),
            Code::NameMismatch,
            format!("plane id \"{plane_id}\" does not match its directory \"{id}\""),
        );
        return None;
    }
    let title = match root.get("title") {
        Some(value) => Some(reader.text(value, 256)?),
        None => None,
    };
    let kind_value = reader.required(root, "kind")?;
    let kind = match kind_value.as_str() {
        Some("shared") => PlaneKind::Shared,
        Some("instanced") => PlaneKind::Instanced,
        Some("per_player") => PlaneKind::PerPlayer,
        _ => {
            reader.report(
                kind_value.span(),
                Code::Range,
                "kind must be \"shared\", \"instanced\" or \"per_player\"",
            );
            return None;
        }
    };
    let size_value = reader.required(root, "chunk_size")?;
    let chunk_size = u16::try_from(reader.integer(size_value, 1, 255)?).ok()?;
    if chunk_size != CHUNK_SIZE {
        reader.report(
            size_value.span(),
            Code::Range,
            format!("chunk_size must be {CHUNK_SIZE} in format 1"),
        );
        return None;
    }
    let default_zone = match root.get("default_zone") {
        Some(value) => Some(reader.identifier(value)?),
        None => None,
    };
    reader.custom(root);
    let canonical = reader.canonical(root);
    format_ok.then_some(Plane {
        id: plane_id,
        title,
        kind,
        chunk_size,
        default_zone,
        canonical,
        zones: BTreeMap::new(),
        anchors: BTreeMap::new(),
        stairwells: BTreeMap::new(),
        chunks: BTreeMap::new(),
    })
}

fn read_zones(
    tree: &mut Tree<'_>,
    profile: &Profile,
    path: &str,
    references: &mut References,
    diagnostics: &mut Diagnostics,
) -> BTreeMap<String, Zone> {
    let mut zones = BTreeMap::new();
    let Some(source) = tree.document(path) else {
        return zones;
    };
    let reader = Reader::new(&source, diagnostics);
    let Some(root) = reader.object(reader.root(), &["format", "zones"]) else {
        return zones;
    };
    reader.format(root, "tilewright.zones/1");
    let Some(items) = reader
        .required(root, "zones")
        .and_then(|value| reader.array(value))
    else {
        return zones;
    };
    let mut parents = Vec::new();
    for item in items {
        let Some(entry) = reader.object(item, &["id", "parent", "name", "properties", "custom"])
        else {
            continue;
        };
        let Some(id_value) = reader.required(entry, "id") else {
            continue;
        };
        let Some(id) = reader.identifier(id_value) else {
            continue;
        };
        if zones.contains_key(&id) {
            reader.report(
                id_value.span(),
                Code::Duplicate,
                format!("zone \"{id}\" is defined twice"),
            );
            continue;
        }
        let parent = match entry.get("parent") {
            Some(value) => match reader.identifier(value) {
                Some(parent) => {
                    parents.push((id.clone(), parent.clone(), value.span()));
                    Some(parent)
                }
                None => continue,
            },
            None => None,
        };
        let name = match entry.get("name") {
            Some(value) => match reader.text(value, 256) {
                Some(name) => Some(name),
                None => continue,
            },
            None => None,
        };
        if read_fields(
            &reader,
            entry,
            entry.get("properties"),
            &profile.zone_properties,
            references,
        )
        .is_none()
        {
            continue;
        }
        reader.custom(entry);
        zones.insert(
            id.clone(),
            Zone {
                id,
                parent,
                name,
                canonical: reader.canonical(entry),
            },
        );
    }
    for (id, parent, span) in &parents {
        if !zones.contains_key(parent) {
            reader.report(
                *span,
                Code::Reference,
                format!("zone \"{id}\" has unknown parent \"{parent}\""),
            );
        }
    }
    report_cycles(&reader, &zones, &parents);
    zones
}

/// Reports every zone whose parent chain never reaches a root. Each zone is
/// walked once: a walk stops at the first zone whose outcome is known.
fn report_cycles(
    reader: &Reader<'_>,
    zones: &BTreeMap<String, Zone>,
    parents: &[(String, String, Span)],
) {
    let mut rooted: BTreeMap<&str, bool> = BTreeMap::new();
    for (id, _, span) in parents {
        let mut path = Vec::new();
        let mut on_path = BTreeSet::new();
        let mut current = Some(id.as_str());
        let reaches_root = loop {
            let Some(zone) = current else {
                break true;
            };
            if let Some(known) = rooted.get(zone) {
                break *known;
            }
            if !on_path.insert(zone) {
                break false;
            }
            path.push(zone);
            current = zones.get(zone).and_then(|zone| zone.parent.as_deref());
        };
        rooted.extend(path.into_iter().map(|zone| (zone, reaches_root)));
        if !reaches_root {
            reader.report(
                *span,
                Code::Cycle,
                format!("zone \"{id}\" is its own ancestor"),
            );
        }
    }
}

fn read_anchors(
    tree: &mut Tree<'_>,
    path: &str,
    diagnostics: &mut Diagnostics,
) -> BTreeMap<String, Anchor> {
    let mut anchors = BTreeMap::new();
    let Some(source) = tree.document(path) else {
        return anchors;
    };
    let reader = Reader::new(&source, diagnostics);
    let Some(root) = reader.object(reader.root(), &["format", "anchors"]) else {
        return anchors;
    };
    reader.format(root, "tilewright.anchors/1");
    let Some(items) = reader
        .required(root, "anchors")
        .and_then(|value| reader.array(value))
    else {
        return anchors;
    };
    for item in items {
        let Some(entry) = reader.object(item, &["id", "layer", "at"]) else {
            continue;
        };
        let (Some(id_value), Some(layer), Some(at)) = (
            reader.required(entry, "id"),
            reader.required(entry, "layer"),
            reader.required(entry, "at"),
        ) else {
            continue;
        };
        let (Some(id), Some(layer), Some(at)) = (
            reader.identifier(id_value),
            reader.i32(layer),
            reader.coordinates(at),
        ) else {
            continue;
        };
        if anchors.contains_key(&id) {
            reader.report(
                id_value.span(),
                Code::Duplicate,
                format!("anchor \"{id}\" is defined twice"),
            );
            continue;
        }
        anchors.insert(id, Anchor { layer, at });
    }
    anchors
}

fn read_layers(
    tree: &mut Tree<'_>,
    profile: &Profile,
    plane: &mut Plane,
    directory: &str,
    references: &mut References,
    diagnostics: &mut Diagnostics,
) {
    let layers = join(directory, "layers");
    // Chunk `c` covers tiles `c * size ..= c * size + size - 1`; both ends
    // must stay within the format's integer range.
    let size = i64::from(plane.chunk_size);
    let chunks = -(MAX_SAFE_INTEGER / size)..=(MAX_SAFE_INTEGER - (size - 1)) / size;
    for layer_entry in tree.list(&layers) {
        let layer_directory = join(&layers, &layer_entry.name);
        let layer = canonical_integer(&layer_entry.name)
            .and_then(|layer| i32::try_from(layer).ok())
            .filter(|_| layer_entry.kind == EntryKind::Directory);
        let Some(layer) = layer else {
            diagnostics.push(Diagnostic::file(
                &layer_directory,
                Code::UnexpectedFile,
                "layer directories are named by a canonical 32-bit integer",
            ));
            continue;
        };
        for chunk_entry in tree.list(&layer_directory) {
            let path = join(&layer_directory, &chunk_entry.name);
            let coordinates = chunk_entry
                .name
                .strip_suffix(".jsonc")
                .and_then(|stem| stem.split_once('_'))
                .and_then(|(x, y)| Some((canonical_integer(x)?, canonical_integer(y)?)))
                .filter(|(cx, cy)| chunks.contains(cx) && chunks.contains(cy))
                .filter(|_| chunk_entry.kind == EntryKind::File);
            let Some((cx, cy)) = coordinates else {
                diagnostics.push(Diagnostic::file(
                    &path,
                    Code::UnexpectedFile,
                    "chunk files are named <cx>_<cy>.jsonc with canonical integers in range",
                ));
                continue;
            };
            let address = ChunkAddress {
                plane: &plane.id,
                layer,
                cx,
                cy,
                size: plane.chunk_size,
            };
            if let Some(chunk) = read_chunk(
                tree,
                profile,
                &plane.zones,
                &address,
                &path,
                references,
                diagnostics,
            ) {
                plane.chunks.insert((layer, cx, cy), chunk);
            }
        }
    }
}
