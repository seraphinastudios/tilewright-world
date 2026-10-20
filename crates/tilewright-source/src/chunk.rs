// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Chunk documents: terrain grids and objects.

use crate::diagnostic::Diagnostics;
use crate::{
    Chunk, Code, Grid, Object, Portal, Profile, Zone,
    files::Tree,
    read::Reader,
    schema::{References, Target, read_fields},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use tilewright_jsonc::Value;

/// Deepest inventory nesting accepted.
const MAXIMUM_INVENTORY_DEPTH: usize = 16;

/// Where a chunk file sits, from its path.
pub(crate) struct ChunkAddress<'a> {
    pub plane: &'a str,
    pub layer: i32,
    pub cx: i64,
    pub cy: i64,
    pub size: u16,
}

#[derive(Clone, Copy, PartialEq)]
enum GridKind {
    Ground,
    Zone,
}

#[allow(clippy::too_many_lines)]
pub(crate) fn read_chunk(
    tree: &mut Tree<'_>,
    profile: &Profile,
    zones: &BTreeMap<String, Zone>,
    address: &ChunkAddress<'_>,
    path: &str,
    references: &mut References,
    diagnostics: &mut Diagnostics,
) -> Option<Chunk> {
    let source = tree.document(path)?;
    let reader = Reader::new(&source, diagnostics);
    let root = reader.object(
        reader.root(),
        &["format", "plane", "layer", "chunk", "terrain", "objects"],
    )?;
    let mut ok = reader.format(root, "tilewright.chunk/1");
    let plane_value = reader.required(root, "plane")?;
    if reader.string(plane_value)? != address.plane {
        reader.report(
            plane_value.span(),
            Code::NameMismatch,
            format!("plane must be \"{}\" to match the path", address.plane),
        );
        ok = false;
    }
    let layer_value = reader.required(root, "layer")?;
    if reader.i32(layer_value)? != address.layer {
        reader.report(
            layer_value.span(),
            Code::NameMismatch,
            format!("layer must be {} to match the path", address.layer),
        );
        ok = false;
    }
    let chunk_value = reader.required(root, "chunk")?;
    if reader.coordinates(chunk_value)? != [address.cx, address.cy] {
        reader.report(
            chunk_value.span(),
            Code::NameMismatch,
            format!(
                "chunk must be [{}, {}] to match the file name",
                address.cx, address.cy
            ),
        );
        ok = false;
    }
    let cells = usize::from(address.size) * usize::from(address.size);
    let mut chunk = Chunk {
        path: path.to_owned(),
        ground: vec![
            Grid {
                palette: Vec::new(),
                cells: vec![0; cells],
            };
            profile.ground_layers.len()
        ],
        heights: vec![0; cells],
        zones: Grid {
            palette: Vec::new(),
            cells: vec![0; cells],
        },
        objects: Vec::new(),
    };
    if let Some(terrain) = root.get("terrain") {
        let mut allowed: Vec<&str> = profile.ground_layers.iter().map(String::as_str).collect();
        allowed.extend(["height", "zone"]);
        if reader.object(terrain, &allowed).is_none() {
            ok = false;
        }
        for (index, layer) in profile.ground_layers.iter().enumerate() {
            if let Some(value) = terrain.get(layer) {
                match read_grid(&reader, value, address.size, GridKind::Ground) {
                    Some(grid) => {
                        for value_name in &grid.palette {
                            reader.reference(
                                references,
                                &Target::Definition {
                                    kind: "proto".to_owned(),
                                    id: value_name.clone(),
                                },
                                value.span(),
                            );
                        }
                        chunk.ground[index] = grid;
                    }
                    None => ok = false,
                }
            }
        }
        if let Some(value) = terrain.get("height") {
            match read_heights(&reader, value, address.size) {
                Some(heights) => chunk.heights = heights,
                None => ok = false,
            }
        }
        if let Some(value) = terrain.get("zone") {
            match read_grid(&reader, value, address.size, GridKind::Zone) {
                Some(grid) => {
                    for zone in &grid.palette {
                        if !zones.contains_key(zone) {
                            reader.report(
                                value.span(),
                                Code::Reference,
                                format!("zone \"{zone}\" is not in this plane's zones.jsonc"),
                            );
                            ok = false;
                        }
                    }
                    chunk.zones = grid;
                }
                None => ok = false,
            }
        }
    }
    if let Some(objects) = root.get("objects") {
        let items = reader.array(objects)?;
        let mut context = ObjectContext {
            profile,
            plane: address.plane,
            path: Arc::from(path),
            size: address.size,
            references,
        };
        for item in items {
            match read_object(&reader, &mut context, item, 0) {
                Some(object) => chunk.objects.push(object),
                None => ok = false,
            }
        }
    }
    ok.then_some(chunk)
}

/// A character grid: `{"palette": {...}, "rows": [...]}` or
/// `{"palette": {...}, "fill": "k"}`, with optional `"cell_width": 2`.
fn read_grid<'a>(
    reader: &Reader<'a>,
    value: &'a Value,
    chunk_size: u16,
    kind: GridKind,
) -> Option<Grid> {
    let grid = reader.object(value, &["palette", "rows", "fill", "cell_width"])?;
    let width = match grid.get("cell_width") {
        Some(width) => usize::try_from(reader.integer(width, 1, 2)?).ok()?,
        None => 1,
    };
    let (keys, palette) = read_palette(reader, reader.required(grid, "palette")?, width, kind)?;
    // Keys are indexed once, so decoding a cell is a lookup, not a scan.
    let indexes: BTreeMap<&str, u16> = keys
        .iter()
        .zip(1..)
        .map(|(key, index)| (*key, index))
        .collect();
    let lookup = |key: &str| -> Option<u16> {
        if key.bytes().all(|byte| byte == b'.') {
            Some(0)
        } else {
            indexes.get(key).copied()
        }
    };
    let side = usize::from(chunk_size);
    let cells = match (grid.get("rows"), grid.get("fill")) {
        (Some(rows), None) => {
            let items = reader.array(rows)?;
            if items.len() != side {
                reader.report(rows.span(), Code::Grid, format!("expected {side} rows"));
                return None;
            }
            let mut cells = Vec::with_capacity(side * side);
            for row in items {
                let text = reader.string(row)?;
                if !text.is_ascii() || text.len() != side * width {
                    reader.report(
                        row.span(),
                        Code::Grid,
                        format!("each row is {} ASCII characters", side * width),
                    );
                    return None;
                }
                for cell in text.as_bytes().chunks(width) {
                    let key = std::str::from_utf8(cell).unwrap_or_default();
                    let Some(index) = lookup(key) else {
                        reader.report(
                            row.span(),
                            Code::Grid,
                            format!("\"{key}\" is not in the palette"),
                        );
                        return None;
                    };
                    cells.push(index);
                }
            }
            cells
        }
        (None, Some(fill)) => {
            let key = reader.string(fill)?;
            let Some(index) = lookup(key).filter(|_| key.len() == width) else {
                reader.report(
                    fill.span(),
                    Code::Grid,
                    format!("\"{key}\" is not in the palette"),
                );
                return None;
            };
            vec![index; side * side]
        }
        _ => {
            reader.report(
                grid.span(),
                Code::Grid,
                "a grid has exactly one of rows or fill",
            );
            return None;
        }
    };
    Some(Grid { palette, cells })
}

/// A grid palette: keys of `width` printable ASCII characters, not all `.`,
/// mapping to identifiers. Returns the keys and values in source order.
fn read_palette<'a>(
    reader: &Reader<'a>,
    value: &'a Value,
    width: usize,
    kind: GridKind,
) -> Option<(Vec<&'a str>, Vec<String>)> {
    let members = reader.members(value)?;
    if members.len() > usize::from(u16::MAX - 1) {
        reader.report(value.span(), Code::Grid, "palette has too many entries");
        return None;
    }
    let mut keys: Vec<&str> = Vec::with_capacity(members.len());
    let mut palette = Vec::with_capacity(members.len());
    let mut seen = BTreeSet::new();
    for member in members {
        let key_ok = member.key.len() == width
            && member.key.bytes().all(|byte| byte.is_ascii_graphic())
            && member.key.bytes().any(|byte| byte != b'.');
        if !key_ok {
            reader.report(
                member.key_span,
                Code::Grid,
                format!("palette keys are {width} printable ASCII character(s) and not all \".\""),
            );
            return None;
        }
        let entry = reader.identifier(&member.value)?;
        if kind == GridKind::Zone && !seen.insert(entry.clone()) {
            reader.report(
                member.value.span(),
                Code::Duplicate,
                "zone listed twice in palette",
            );
            return None;
        }
        keys.push(member.key.as_str());
        palette.push(entry);
    }
    Some((keys, palette))
}

/// `{"fill": h}` or `{"rows": [[h, ...], ...]}`.
fn read_heights<'a>(reader: &Reader<'a>, value: &'a Value, chunk_size: u16) -> Option<Vec<i32>> {
    let grid = reader.object(value, &["rows", "fill"])?;
    let side = usize::from(chunk_size);
    match (grid.get("rows"), grid.get("fill")) {
        (Some(rows), None) => {
            let items = reader.array(rows)?;
            if items.len() != side {
                reader.report(rows.span(), Code::Grid, format!("expected {side} rows"));
                return None;
            }
            let mut heights = Vec::with_capacity(side * side);
            for row in items {
                let cells = reader.array(row)?;
                if cells.len() != side {
                    reader.report(
                        row.span(),
                        Code::Grid,
                        format!("each row has {side} heights"),
                    );
                    return None;
                }
                for cell in cells {
                    heights.push(reader.i32(cell)?);
                }
            }
            Some(heights)
        }
        (None, Some(fill)) => Some(vec![reader.i32(fill)?; side * side]),
        _ => {
            reader.report(
                grid.span(),
                Code::Grid,
                "a grid has exactly one of rows or fill",
            );
            None
        }
    }
}

struct ObjectContext<'p, 'r> {
    profile: &'p Profile,
    plane: &'p str,
    /// The chunk's path, kept with each uid its objects claim.
    path: Arc<str>,
    size: u16,
    references: &'r mut References,
}

/// The keys an object may have.
const OBJECT_KEYS: &[&str] = &[
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

/// Reads an object. Every part of it is read, and reports its own problems,
/// before a problem in one part fails the object, so a malformed value
/// never hides the problems of the object's other values or of its
/// inventory entries.
fn read_object<'a>(
    reader: &Reader<'a>,
    context: &mut ObjectContext<'_, '_>,
    value: &'a Value,
    depth: usize,
) -> Option<Object> {
    let entry = reader.object(value, OBJECT_KEYS)?;
    // The uid is claimed before the inventory is read, so of two objects
    // that share one, the first in document order keeps it.
    let uid = read_uid(reader, context, entry);
    let at = read_at(reader, context.size, entry, depth);
    let proto = reader.required(entry, "proto").and_then(|value| {
        let proto = reader.identifier(value)?;
        reader.reference(
            context.references,
            &Target::Definition {
                kind: "proto".to_owned(),
                id: proto.clone(),
            },
            value.span(),
        );
        Some(proto)
    });
    let layer = optional(entry.get("layer"), |value| {
        let layer = reader.identifier(value)?;
        reader.reference(
            context.references,
            &Target::RenderLayer(layer.clone()),
            value.span(),
        );
        Some(layer)
    });
    let sub_layer = optional(entry.get("sub_layer"), |value| {
        i16::try_from(reader.integer(value, i64::from(i16::MIN), i64::from(i16::MAX))?).ok()
    });
    let z = optional(entry.get("z"), |value| reader.i32(value));
    let fields = read_fields(
        reader,
        entry,
        entry.get("fields"),
        &context.profile.object_fields,
        context.references,
    );
    let portal = optional(entry.get("portal"), |value| {
        read_portal(reader, context, value)
    });
    let inventory = read_inventory(reader, context, entry, depth);
    let custom = reader.custom(entry);
    Some(Object {
        uid: uid.ok()?,
        at: at.ok()?,
        proto: proto?,
        layer: layer.ok()?,
        sub_layer: sub_layer.ok()?.unwrap_or(0),
        z: z.ok()?.unwrap_or(0),
        fields: fields?,
        portal: portal.ok()?,
        inventory: inventory?,
        custom,
    })
}

/// A value that did not read, whose problem has been reported.
struct Reported;

/// `Ok(None)` for a value that is absent, `Ok(Some(read))` for one that
/// reads, and [`Reported`] for one that does not.
fn optional<'a, T>(
    value: Option<&'a Value>,
    read: impl FnOnce(&'a Value) -> Option<T>,
) -> Result<Option<T>, Reported> {
    match value {
        Some(value) => read(value).map(Some).ok_or(Reported),
        None => Ok(None),
    }
}

/// An object's uid, claimed for the object: `Ok(None)` without one, and
/// [`Reported`] for one that is malformed or that an object read before it
/// has.
fn read_uid<'a>(
    reader: &Reader<'a>,
    context: &mut ObjectContext<'_, '_>,
    entry: &'a Value,
) -> Result<Option<String>, Reported> {
    optional(entry.get("uid"), |value| {
        let uid = reader.identifier(value)?;
        if let Some(first) = context.references.claim_uid(&uid, &context.path) {
            reader.report(
                value.span(),
                Code::Duplicate,
                format!("object uid \"{uid}\" is already used by an object in {first}"),
            );
            return None;
        }
        Some(uid)
    })
}

/// A top-level object's chunk-local tile, which it must have, or an
/// inventory entry's absent one.
fn read_at<'a>(
    reader: &Reader<'a>,
    size: u16,
    entry: &'a Value,
    depth: usize,
) -> Result<Option<[u16; 2]>, Reported> {
    if depth > 0 {
        if let Some(at_value) = entry.get("at") {
            reader.report(
                at_value.span(),
                Code::UnknownKey,
                "inventory entries have no \"at\"",
            );
            return Err(Reported);
        }
        return Ok(None);
    }
    let at_value = reader.required(entry, "at").ok_or(Reported)?;
    let [x, y] = reader.coordinates(at_value).ok_or(Reported)?;
    let limit = i64::from(size);
    match (u16::try_from(x), u16::try_from(y)) {
        (Ok(x), Ok(y)) if i64::from(x) < limit && i64::from(y) < limit => Ok(Some([x, y])),
        _ => {
            reader.report(
                at_value.span(),
                Code::Range,
                format!("at must be within 0..{limit} on both axes"),
            );
            Err(Reported)
        }
    }
}

/// An object's inventory. Every entry is read, so each reports its own
/// problems, before one that fails fails the inventory.
fn read_inventory<'a>(
    reader: &Reader<'a>,
    context: &mut ObjectContext<'_, '_>,
    entry: &'a Value,
    depth: usize,
) -> Option<Vec<Object>> {
    let Some(items_value) = entry.get("inventory") else {
        return Some(Vec::new());
    };
    if depth + 1 > MAXIMUM_INVENTORY_DEPTH {
        reader.report(
            items_value.span(),
            Code::Range,
            format!("inventories nest at most {MAXIMUM_INVENTORY_DEPTH} deep"),
        );
        return None;
    }
    let mut ok = true;
    let mut inventory = Vec::new();
    for item in reader.array(items_value)? {
        match read_object(reader, context, item, depth + 1) {
            Some(object) => inventory.push(object),
            None => ok = false,
        }
    }
    ok.then_some(inventory)
}

fn read_portal<'a>(
    reader: &Reader<'a>,
    context: &mut ObjectContext<'_, '_>,
    value: &'a Value,
) -> Option<Portal> {
    let anchor = value.get("anchor").is_some();
    let allowed: &[&str] = if anchor {
        &["plane", "anchor"]
    } else {
        &["plane", "layer", "at"]
    };
    let portal = reader.object(value, allowed)?;
    let plane = match portal.get("plane") {
        Some(plane_value) => {
            let plane = reader.identifier(plane_value)?;
            reader.reference(
                context.references,
                &Target::Plane(plane.clone()),
                plane_value.span(),
            );
            Some(plane)
        }
        None => None,
    };
    let target_plane = plane.clone().unwrap_or_else(|| context.plane.to_owned());
    if anchor {
        let anchor_value = reader.required(portal, "anchor")?;
        let anchor = reader.identifier(anchor_value)?;
        reader.reference(
            context.references,
            &Target::Anchor {
                plane: target_plane.clone(),
                id: anchor.clone(),
            },
            anchor_value.span(),
        );
        Some(Portal::Anchor { plane, anchor })
    } else {
        let layer = reader.i32(reader.required(portal, "layer")?)?;
        let at = reader.coordinates(reader.required(portal, "at")?)?;
        Some(Portal::Location { plane, layer, at })
    }
}
