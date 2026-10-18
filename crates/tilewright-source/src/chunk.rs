// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Chunk documents: terrain grids and objects.

use crate::{
    Chunk, Code, Diagnostic, Grid, Object, Portal, Profile, Zone,
    files::Tree,
    read::Reader,
    schema::{References, Target, read_fields},
};
use std::collections::BTreeMap;
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
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Chunk> {
    let source = tree.document(path)?;
    let reader = Reader::new(&source, diagnostics);
    let root = reader.object(
        reader.root(),
        &[
            "format", "plane", "layer", "chunk", "terrain", "objects", "custom",
        ],
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
                            references.add(
                                Target::Definition {
                                    kind: "proto".to_owned(),
                                    id: value_name.clone(),
                                },
                                reader.diagnostic(
                                    value.span(),
                                    Code::Reference,
                                    format!("no proto definition \"{value_name}\""),
                                ),
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
    reader.custom(root);
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
    let lookup = |key: &str| -> Option<u16> {
        if key.bytes().all(|byte| byte == b'.') {
            Some(0)
        } else {
            keys.iter()
                .position(|candidate| *candidate == key)
                .and_then(|index| u16::try_from(index + 1).ok())
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
        if kind == GridKind::Zone && palette.contains(&entry) {
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
    size: u16,
    references: &'r mut References,
}

#[allow(clippy::too_many_lines)]
fn read_object<'a>(
    reader: &Reader<'a>,
    context: &mut ObjectContext<'_, '_>,
    value: &'a Value,
    depth: usize,
) -> Option<Object> {
    let entry = reader.object(
        value,
        &[
            "at",
            "proto",
            "layer",
            "sub_layer",
            "z",
            "fields",
            "portal",
            "inventory",
            "custom",
        ],
    )?;
    let at = if depth == 0 {
        let at_value = reader.required(entry, "at")?;
        let [x, y] = reader.coordinates(at_value)?;
        let limit = i64::from(context.size);
        if !(0..limit).contains(&x) || !(0..limit).contains(&y) {
            reader.report(
                at_value.span(),
                Code::Range,
                format!("at must be within 0..{limit} on both axes"),
            );
            return None;
        }
        Some([u16::try_from(x).ok()?, u16::try_from(y).ok()?])
    } else {
        if let Some(at_value) = entry.get("at") {
            reader.report(
                at_value.span(),
                Code::UnknownKey,
                "inventory entries have no \"at\"",
            );
            return None;
        }
        None
    };
    let proto_value = reader.required(entry, "proto")?;
    let proto = reader.identifier(proto_value)?;
    context.references.add(
        Target::Definition {
            kind: "proto".to_owned(),
            id: proto.clone(),
        },
        reader.diagnostic(
            proto_value.span(),
            Code::Reference,
            format!("no proto definition \"{proto}\""),
        ),
    );
    let layer = match entry.get("layer") {
        Some(layer_value) => {
            let layer = reader.identifier(layer_value)?;
            context.references.add(
                Target::RenderLayer(layer.clone()),
                reader.diagnostic(
                    layer_value.span(),
                    Code::Reference,
                    format!("\"{layer}\" is not a render layer of the profile"),
                ),
            );
            Some(layer)
        }
        None => None,
    };
    let sub_layer = match entry.get("sub_layer") {
        Some(value) => {
            i16::try_from(reader.integer(value, i64::from(i16::MIN), i64::from(i16::MAX))?).ok()?
        }
        None => 0,
    };
    let z = match entry.get("z") {
        Some(value) => reader.i32(value)?,
        None => 0,
    };
    let fields = read_fields(
        reader,
        entry,
        entry.get("fields"),
        &context.profile.object_fields,
        context.references,
    )?;
    let portal = match entry.get("portal") {
        Some(value) => Some(read_portal(reader, context, value)?),
        None => None,
    };
    let mut inventory = Vec::new();
    if let Some(items_value) = entry.get("inventory") {
        if depth + 1 > MAXIMUM_INVENTORY_DEPTH {
            reader.report(
                items_value.span(),
                Code::Range,
                format!("inventories nest at most {MAXIMUM_INVENTORY_DEPTH} deep"),
            );
            return None;
        }
        for item in reader.array(items_value)? {
            inventory.push(read_object(reader, context, item, depth + 1)?);
        }
    }
    let custom = reader.custom(entry);
    Some(Object {
        at,
        proto,
        layer,
        sub_layer,
        z,
        fields,
        portal,
        inventory,
        custom,
    })
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
            context.references.add(
                Target::Plane(plane.clone()),
                reader.diagnostic(
                    plane_value.span(),
                    Code::Reference,
                    format!("no plane \"{plane}\""),
                ),
            );
            Some(plane)
        }
        None => None,
    };
    let target_plane = plane.clone().unwrap_or_else(|| context.plane.to_owned());
    if anchor {
        let anchor_value = reader.required(portal, "anchor")?;
        let anchor = reader.identifier(anchor_value)?;
        context.references.add(
            Target::Anchor {
                plane: target_plane.clone(),
                id: anchor.clone(),
            },
            reader.diagnostic(
                anchor_value.span(),
                Code::Reference,
                format!("no anchor \"{anchor}\" in plane \"{target_plane}\""),
            ),
        );
        Some(Portal::Anchor { plane, anchor })
    } else {
        let layer = reader.i32(reader.required(portal, "layer")?)?;
        let at = reader.coordinates(reader.required(portal, "at")?)?;
        Some(Portal::Location { plane, layer, at })
    }
}
