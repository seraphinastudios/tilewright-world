// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! `world.jsonc` and the game profile.

use crate::diagnostic::Diagnostics;
use crate::{
    Code, MAXIMUM_GROUND_LAYERS, MAXIMUM_LAYER_HEIGHT, Movement, Profile, Projection,
    files::{Tree, is_jsonc_path},
    index::{Holder, Role, Site, Target},
    read::Reader,
    schema::{References, Schema, SchemaTarget, read_schema},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// Reads `world.jsonc`: the profile path and the plane list.
pub(crate) fn read_manifest(
    tree: &mut Tree<'_>,
    references: &References,
    diagnostics: &mut Diagnostics,
) -> Option<(String, Vec<String>)> {
    let source = tree.document("world.jsonc")?;
    let reader = Reader::new(&source, diagnostics);
    let root = reader.object(reader.root(), &["format", "profile", "planes"])?;
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
    references.follow(&reader.site(
        Target::File(profile),
        (Role::ManifestProfile, Holder::Manifest),
        profile_value.span(),
    ));
    let planes_value = reader.required(root, "planes")?;
    let planes = reader.identifiers(planes_value)?;
    // Whether each plane's directory is there is the plane reading's to
    // report.
    for (plane, value) in planes
        .iter()
        .zip(planes_value.as_array().unwrap_or_default())
    {
        references.follow(&reader.site(
            Target::Plane(plane),
            (Role::ManifestPlane, Holder::Manifest),
            value.span(),
        ));
    }
    format_ok.then(|| (profile.to_owned(), planes))
}

/// Whether every reference field of `schema` targets a declared definition
/// type; reports each one that does not, on the schema as a whole.
fn references_resolve(
    schema: &Schema,
    targets: &[SchemaTarget],
    references: &References,
    diagnostics: &mut Diagnostics,
) -> bool {
    let mut ok = true;
    for target in targets {
        ok &= references.check_file(
            &Site {
                target: Target::DefinitionType(&target.target),
                role: Role::SchemaTarget {
                    field: &target.field,
                },
                holder: Holder::Schema { path: &schema.path },
                file: &schema.path,
                start: target.span.start,
                end: target.span.end,
            },
            diagnostics,
        );
    }
    ok
}

/// What a schema document is for.
enum Purpose {
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

/// Reads the profile and its schemas. Also returns the path of its
/// transitions document, if it names one, which is read once the
/// definitions are listed, since its prototypes refer to them.
#[allow(clippy::too_many_lines)]
pub(crate) fn read_profile(
    tree: &mut Tree<'_>,
    path: &str,
    references: &mut References,
    diagnostics: &mut Diagnostics,
) -> Option<(Profile, Option<String>)> {
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
            "layer_height",
            "movement",
            "transitions",
            "custom",
        ],
    )?;
    let mut ok = reader.format(root, "tilewright.profile/1");
    // The layer height is read first, so no other member's problem hides
    // one in it, and a bad one does not stop the profile's other checks.
    let layer_height = root.get("layer_height").and_then(|value| {
        let height = reader
            .integer(value, 1, i64::from(MAXIMUM_LAYER_HEIGHT))
            .and_then(|height| u32::try_from(height).ok());
        ok &= height.is_some();
        height
    });
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
    references.render_layers_declared(&render_layers);
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
        let span = ground_value
            .as_array()
            .and_then(|items| items.get(index))
            .map_or(ground_value.span(), tilewright_jsonc::Value::span);
        // A ground layer must be a render layer, and not one whose name a
        // chunk's terrain keeps for its other grids; one message says both.
        let resolves = references.check(&reader.site(
            Target::RenderLayer(layer),
            (Role::ProfileGroundLayer, Holder::Profile),
            span,
        ));
        if !resolves || ["height", "zone"].contains(&layer.as_str()) {
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
    let transitions = match root.get("transitions") {
        Some(value) => match reader.string(value) {
            Some(text) if is_jsonc_path(text) => {
                let resolved = beside(path, text);
                references.follow(&reader.site(
                    Target::File(&resolved),
                    (Role::ProfileTransitions, Holder::Profile),
                    value.span(),
                ));
                Some(resolved)
            }
            Some(_) => {
                reader.report(
                    value.span(),
                    Code::Path,
                    "transitions must be a safe relative .jsonc path",
                );
                ok = false;
                None
            }
            None => {
                ok = false;
                None
            }
        },
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
        let resolved = beside(path, schema);
        if let Some(value) = entry.get("schema") {
            references.follow(&reader.site(
                Target::File(&resolved),
                (Role::ProfileSchema, Holder::Profile),
                value.span(),
            ));
        }
        schema_paths.push((Purpose::Definition(member.key.clone()), resolved));
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
    for (key, purpose) in [
        ("object_fields", Purpose::ObjectFields),
        ("zone_properties", Purpose::ZoneProperties),
    ] {
        let Some(value) = reader.required(root, key) else {
            ok = false;
            continue;
        };
        match reader.string(value) {
            Some(text) if is_jsonc_path(text) => {
                let resolved = beside(path, text);
                references.follow(&reader.site(
                    Target::File(&resolved),
                    (Role::ProfileSchema, Holder::Profile),
                    value.span(),
                ));
                schema_paths.push((purpose, resolved));
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
    let type_names: BTreeSet<String> = schema_paths
        .iter()
        .filter_map(|(purpose, _)| match purpose {
            Purpose::Definition(kind) => Some(kind.clone()),
            _ => None,
        })
        .collect();
    references.profile_declared(&ground_layers, &type_names);

    // Each schema path is read and checked once; roles that name the same
    // path share one schema. `None` marks a schema that failed.
    let mut loaded: BTreeMap<String, Option<Arc<Schema>>> = BTreeMap::new();
    let mut definition_types = BTreeMap::new();
    let mut object_fields = None;
    let mut zone_properties = None;
    for (purpose, schema_path) in schema_paths {
        let schema = if let Some(schema) = loaded.get(&schema_path) {
            schema.clone()
        } else {
            let schema = read_schema(tree, &schema_path, diagnostics)
                .filter(|(schema, targets)| {
                    references_resolve(schema, targets, references, diagnostics)
                })
                .map(|(schema, _)| Arc::new(schema));
            loaded.insert(schema_path, schema.clone());
            schema
        };
        let Some(schema) = schema else {
            ok = false;
            continue;
        };
        match purpose {
            Purpose::Definition(kind) => {
                definition_types.insert(kind, schema);
            }
            Purpose::ObjectFields => object_fields = Some(schema),
            Purpose::ZoneProperties => zone_properties = Some(schema),
        }
    }
    let (Some(object_fields), Some(zone_properties)) = (object_fields, zone_properties) else {
        return None;
    };
    let profile = Profile {
        id,
        version,
        projection,
        render_layers,
        ground_layers,
        definition_types,
        object_fields,
        zone_properties,
        layer_height,
        movement,
        transitions: None,
        canonical,
    };
    ok.then_some((profile, transitions))
}
