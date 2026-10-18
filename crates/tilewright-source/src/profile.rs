// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! `world.jsonc` and the game profile.

use crate::{
    Code, Diagnostic, MAXIMUM_GROUND_LAYERS, Movement, Profile, Projection,
    files::{Tree, is_jsonc_path},
    read::Reader,
    schema::{FieldType, read_schema},
};
use std::collections::BTreeMap;

/// Reads `world.jsonc`: the profile path and the plane list.
pub(crate) fn read_manifest(
    tree: &mut Tree<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<(String, Vec<String>)> {
    let source = tree.document("world.jsonc")?;
    let reader = Reader::new(&source, diagnostics);
    let root = reader.object(reader.root(), &["format", "profile", "planes", "custom"])?;
    let format_ok = reader.format(root, "tilewright.world/1");
    let profile_value = reader.required(root, "profile")?;
    let profile = reader.string(profile_value)?;
    if !is_jsonc_path(profile) {
        reader.report(
            profile_value.span(),
            Code::Path,
            "profile must be a safe relative path to a .jsonc file",
        );
        return None;
    }
    let planes = reader.identifiers(reader.required(root, "planes")?)?;
    reader.custom(root);
    format_ok.then(|| (profile.to_owned(), planes))
}

/// What a schema document is for.
enum Role {
    Definition(String),
    ObjectFields,
    ZoneProperties,
}

/// The directory part of a relative path, without a trailing `/`.
fn directory(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(directory, _)| directory)
}

/// Resolves a path written in the profile relative to the profile's
/// directory.
fn beside(profile: &str, relative: &str) -> String {
    crate::files::join(directory(profile), relative)
}

#[allow(clippy::too_many_lines)]
pub(crate) fn read_profile(
    tree: &mut Tree<'_>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Profile> {
    let source = tree.document(path)?;
    let reader = Reader::new(&source, diagnostics);
    let root = reader.object(
        reader.root(),
        &[
            "format",
            "id",
            "version",
            "projection",
            "render_layers",
            "ground_layers",
            "definition_types",
            "object_fields",
            "zone_properties",
            "movement",
            "custom",
        ],
    )?;
    let mut ok = reader.format(root, "tilewright.profile/1");
    let id = reader.identifier(reader.required(root, "id")?)?;
    let version = reader.text(reader.required(root, "version")?, 64)?;
    let projection_value = reader.required(root, "projection")?;
    let projection = match projection_value.get("kind").and_then(|kind| kind.as_str()) {
        Some("isometric") => {
            reader.object(projection_value, &["kind", "tile_width", "tile_height"])?;
            Projection::Isometric {
                tile_width: reader.u32(reader.required(projection_value, "tile_width")?, 1)?,
                tile_height: reader.u32(reader.required(projection_value, "tile_height")?, 1)?,
            }
        }
        Some("top_down") => {
            reader.object(projection_value, &["kind", "tile_size"])?;
            Projection::TopDown {
                tile_size: reader.u32(reader.required(projection_value, "tile_size")?, 1)?,
            }
        }
        _ => {
            reader.report(
                projection_value.span(),
                Code::Range,
                "projection kind must be \"isometric\" or \"top_down\"",
            );
            return None;
        }
    };
    // List lengths are capped before their entries are checked.
    let render_value = reader.required(root, "render_layers")?;
    if render_value
        .as_array()
        .is_some_and(|items| items.is_empty() || items.len() > usize::from(u16::MAX - 1))
    {
        reader.report(
            render_value.span(),
            Code::Range,
            "render_layers must list 1 to 65534 layers",
        );
        return None;
    }
    let render_layers = reader.identifiers(render_value)?;
    let ground_value = reader.required(root, "ground_layers")?;
    if ground_value
        .as_array()
        .is_some_and(|items| items.len() > MAXIMUM_GROUND_LAYERS)
    {
        reader.report(
            ground_value.span(),
            Code::Range,
            format!("at most {MAXIMUM_GROUND_LAYERS} ground layers"),
        );
        return None;
    }
    let ground_layers = reader.identifiers(ground_value)?;
    for (index, layer) in ground_layers.iter().enumerate() {
        if !render_layers.contains(layer) || ["height", "zone"].contains(&layer.as_str()) {
            let span = ground_value
                .as_array()
                .and_then(|items| items.get(index))
                .map_or(ground_value.span(), tilewright_jsonc::Value::span);
            reader.report(
                span,
                Code::Reference,
                format!(
                    "ground layer \"{layer}\" must be a render layer other than height or zone"
                ),
            );
            ok = false;
        }
    }
    let movement = match root.get("movement") {
        Some(value) => {
            reader.object(value, &["max_step", "fall_unit"])?;
            Some(Movement {
                max_step: reader.u32(reader.required(value, "max_step")?, 0)?,
                fall_unit: reader.u32(reader.required(value, "fall_unit")?, 1)?,
            })
        }
        None => None,
    };
    reader.custom(root);
    let canonical = reader.canonical(root);

    // Schemas are separate documents beside the profile.
    let mut schema_paths = Vec::new();
    let types_value = reader.required(root, "definition_types")?;
    for member in reader.members(types_value)? {
        if !crate::read::is_identifier(&member.key) {
            reader.report(
                member.key_span,
                Code::Identifier,
                format!("\"{}\" is not a valid definition type", member.key),
            );
            ok = false;
            continue;
        }
        let Some(entry) = reader.object(&member.value, &["schema"]) else {
            ok = false;
            continue;
        };
        let Some(schema) = reader.required(entry, "schema").and_then(|value| {
            let text = reader.string(value)?;
            if is_jsonc_path(text) {
                Some(text)
            } else {
                reader.report(
                    value.span(),
                    Code::Path,
                    "schema must be a safe relative .jsonc path",
                );
                None
            }
        }) else {
            ok = false;
            continue;
        };
        schema_paths.push((Role::Definition(member.key.clone()), beside(path, schema)));
    }
    if !types_value
        .as_object()
        .is_some_and(|members| members.iter().any(|member| member.key == "proto"))
    {
        reader.report(
            types_value.span(),
            Code::MissingKey,
            "definition_types must declare \"proto\"",
        );
        ok = false;
    }
    for (key, role) in [
        ("object_fields", Role::ObjectFields),
        ("zone_properties", Role::ZoneProperties),
    ] {
        let Some(value) = reader.required(root, key) else {
            ok = false;
            continue;
        };
        match reader.string(value) {
            Some(text) if is_jsonc_path(text) => {
                schema_paths.push((role, beside(path, text)));
            }
            Some(_) => {
                reader.report(
                    value.span(),
                    Code::Path,
                    "must be a safe relative .jsonc path",
                );
                ok = false;
            }
            None => ok = false,
        }
    }
    let type_names: Vec<String> = schema_paths
        .iter()
        .filter_map(|(role, _)| match role {
            Role::Definition(kind) => Some(kind.clone()),
            _ => None,
        })
        .collect();

    let mut definition_types = BTreeMap::new();
    let mut object_fields = None;
    let mut zone_properties = None;
    for (role, schema_path) in schema_paths {
        let Some(schema) = read_schema(tree, &schema_path, diagnostics) else {
            ok = false;
            continue;
        };
        for (name, spec) in &schema.fields {
            if let FieldType::Reference { target } = &spec.field_type
                && !type_names.contains(target)
            {
                diagnostics.push(Diagnostic::file(
                    &schema.path,
                    Code::Reference,
                    format!("field \"{name}\" references unknown definition type \"{target}\""),
                ));
                ok = false;
            }
        }
        match role {
            Role::Definition(kind) => {
                definition_types.insert(kind, schema);
            }
            Role::ObjectFields => object_fields = Some(schema),
            Role::ZoneProperties => zone_properties = Some(schema),
        }
    }
    let (Some(object_fields), Some(zone_properties)) = (object_fields, zone_properties) else {
        return None;
    };
    ok.then_some(Profile {
        id,
        version,
        projection,
        render_layers,
        ground_layers,
        definition_types,
        object_fields,
        zone_properties,
        movement,
        canonical,
    })
}
