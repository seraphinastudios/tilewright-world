// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! The reference index: what each load records, where, and how it is
//! looked up.

use super::{copy_tree, edit, fixture, workspace};
use crate::{
    LoadOptions, Overlay, ReferenceIndex, SourceRoot, World,
    index::{Holder, Reference, Role, Target},
    load_edited_in, load_indexed_in,
};
use std::{
    collections::BTreeSet,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

fn terrain_fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/terrain")
}

/// Every file of `from` copied to `to` with its line endings rewritten:
/// each `\r\n` and each lone `\n` becomes `ending`, so a checkout's
/// endings never reach a golden.
fn copy_with_endings(from: &Path, to: &Path, ending: &[u8]) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_with_endings(&entry.path(), &target, ending);
            continue;
        }
        let bytes = fs::read(entry.path()).unwrap();
        let mut output = Vec::with_capacity(bytes.len() * 2);
        let mut position = 0;
        while position < bytes.len() {
            if bytes[position..].starts_with(b"\r\n") {
                output.extend_from_slice(ending);
                position += 2;
            } else if bytes[position] == b'\n' {
                output.extend_from_slice(ending);
                position += 1;
            } else {
                output.push(bytes[position]);
                position += 1;
            }
        }
        fs::write(target, output).unwrap();
    }
}

/// A copy of `from` with `\n` line endings.
fn normalised(from: &Path) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    copy_with_endings(from, directory.path(), b"\n");
    directory
}

fn indexed(root: &Path) -> (World, ReferenceIndex) {
    indexed_with(root, &Overlay::new())
}

fn indexed_with(root: &Path, overlay: &Overlay) -> (World, ReferenceIndex) {
    load_indexed_in(&SourceRoot::new(root), &LoadOptions::default(), overlay)
        .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"))
}

/// The bytes of `reference`'s span, as the file on disk holds them.
fn text_at(root: &Path, reference: &Reference<'_>) -> String {
    let bytes = fs::read(root.join(reference.file())).unwrap();
    String::from_utf8(bytes[reference.span()].to_vec()).unwrap()
}

/// The identity a reference writes: the last part of its target.
fn written(target: Target<'_>) -> &str {
    match target {
        Target::Definition { id, .. }
        | Target::Anchor { id, .. }
        | Target::Zone { id, .. }
        | Target::DefinitionType(id)
        | Target::Plane(id)
        | Target::RenderLayer(id)
        | Target::GroundLayer(id)
        | Target::Terrain(id)
        | Target::File(id) => id,
    }
}

/// The index as text: one line per reference, in index order, each with
/// the source text at its span.
fn render(root: &Path, index: &ReferenceIndex) -> String {
    let mut text = String::new();
    for reference in index.iter() {
        let span = reference.span();
        let _ = writeln!(
            text,
            "{:?} <- {:?} in {:?} at {}:{}..{} {}",
            reference.target(),
            reference.role(),
            reference.holder(),
            reference.file(),
            span.start,
            span.end,
            text_at(root, &reference),
        );
    }
    text
}

/// Compares `actual` with `tests/golden/{name}.txt`, or writes it there
/// with `TILEWRIGHT_SOURCE_BLESS=1`.
fn golden(name: &str, actual: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/golden/{name}.txt"));
    if std::env::var_os("TILEWRIGHT_SOURCE_BLESS").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, actual).unwrap();
        return;
    }
    let expected = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        .replace("\r\n", "\n");
    assert_eq!(
        actual, expected,
        "{name}; rerun with TILEWRIGHT_SOURCE_BLESS=1 if intended"
    );
}

#[test]
fn the_fixtures_index_every_reference_golden() {
    for (name, from) in [("basic", fixture()), ("terrain", terrain_fixture())] {
        let root = normalised(&from);
        let (_, index) = indexed(root.path());
        golden(&format!("references-{name}"), &render(root.path(), &index));
    }
}

/// Checks what holds for every reference of an index: its span holds the
/// identity it names, as a JSON string, and its file is one the load read.
fn check_spans(root: &Path, world: &World, index: &ReferenceIndex) {
    for reference in index.iter() {
        assert!(
            world.files().contains_key(reference.file()),
            "{reference:?}: its file was not read"
        );
        let text = text_at(root, &reference);
        let name = written(reference.target());
        match reference.target() {
            // A path is written relative to the file that names it.
            Target::File(path) => {
                let written: String = serde_free_string(&text);
                assert!(
                    path.ends_with(&written),
                    "{reference:?}: {text} does not name {path}"
                );
            }
            _ => assert_eq!(text, format!("\"{name}\""), "{reference:?}"),
        }
    }
}

/// The value of a JSON string without escapes.
fn serde_free_string(text: &str) -> String {
    let inner = text
        .strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .unwrap_or_else(|| panic!("{text} is not a string"));
    assert!(!inner.contains('\\'));
    inner.to_owned()
}

#[test]
fn every_span_holds_the_identity_it_names() {
    for from in [fixture(), terrain_fixture()] {
        let root = normalised(&from);
        let (world, index) = indexed(root.path());
        check_spans(root.path(), &world, &index);
    }
}

#[test]
fn spans_are_byte_offsets_of_the_file_as_read_whatever_its_line_endings() {
    for from in [fixture(), terrain_fixture()] {
        let lf = normalised(&from);
        let crlf = tempfile::tempdir().unwrap();
        copy_with_endings(&from, crlf.path(), b"\r\n");
        let (lf_world, lf_index) = indexed(lf.path());
        let (crlf_world, crlf_index) = indexed(crlf.path());
        check_spans(crlf.path(), &crlf_world, &crlf_index);
        // The same references, at other offsets wherever a line ends
        // before them.
        let parts = |index: &ReferenceIndex| -> Vec<String> {
            index
                .iter()
                .map(|reference| {
                    format!(
                        "{:?} {:?} {:?} {}",
                        reference.target(),
                        reference.role(),
                        reference.holder(),
                        reference.file()
                    )
                })
                .collect()
        };
        assert_eq!(parts(&lf_index), parts(&crlf_index));
        let moved = lf_index
            .iter()
            .zip(crlf_index.iter())
            .filter(|(lf, crlf)| lf.span() != crlf.span())
            .count();
        assert!(moved * 2 > lf_index.len(), "{moved} of {}", lf_index.len());
        assert_eq!(lf_world.planes(), crlf_world.planes());
    }
}

#[test]
fn an_object_holder_is_its_depth_first_index_in_the_chunk() {
    let root = workspace();
    // Inventories two deep, before and after other objects.
    edit(
        root.path(),
        super::CHUNK,
        "\"objects\": [",
        "\"objects\": [\n    {\"at\": [0, 0], \"proto\": \"chest\", \"inventory\": [{\"proto\": \"coin\", \"inventory\": [{\"proto\": \"tree\"}]}, {\"proto\": \"wall\"}]},",
    );
    let (world, index) = indexed(root.path());
    let mut objects = 0;
    for (plane_id, plane) in world.planes() {
        for (key, chunk) in &plane.chunks {
            for (ordinal, object) in chunk.depth_first().enumerate() {
                let protos: Vec<Target<'_>> = index
                    .from(Holder::Object {
                        plane: plane_id,
                        chunk: *key,
                        ordinal,
                    })
                    .filter(|reference| reference.role() == Role::Proto)
                    .map(|reference| reference.target())
                    .collect();
                assert_eq!(
                    protos,
                    [Target::Definition {
                        kind: "proto",
                        id: &object.proto
                    }],
                    "{plane_id} {key:?} object {ordinal}"
                );
                objects += 1;
            }
            // Nothing past the last object.
            assert_eq!(
                index
                    .from(Holder::Object {
                        plane: plane_id,
                        chunk: *key,
                        ordinal: chunk.depth_first().count(),
                    })
                    .len(),
                0
            );
        }
    }
    let protos = index
        .iter()
        .filter(|reference| reference.role() == Role::Proto)
        .count();
    assert_eq!(objects, protos);
    let chest = Holder::Object {
        plane: "overworld",
        chunk: (0, 0, 0),
        ordinal: 0,
    };
    let tree = Holder::Object {
        plane: "overworld",
        chunk: (0, 0, 0),
        ordinal: 2,
    };
    assert_eq!(index.from(chest).len(), 1);
    assert_eq!(
        index.from(tree).next().map(|reference| reference.target()),
        Some(Target::Definition {
            kind: "proto",
            id: "tree"
        })
    );
}

/// Every role the loader records, with one tree that holds each.
#[test]
fn every_role_is_recorded_somewhere() {
    let root = workspace();
    // An anchor portal that names its plane, which the fixture lacks.
    edit(
        root.path(),
        super::CHUNK,
        "\"portal\": {\"anchor\": \"cave.entrance\"}",
        "\"portal\": {\"plane\": \"overworld\", \"anchor\": \"cave.entrance\"}",
    );
    let mut roles = BTreeSet::new();
    for from in [root.path().to_path_buf(), terrain_fixture()] {
        let (_, index) = indexed(&from);
        roles.extend(
            index
                .iter()
                .map(|reference| format!("{:?}", reference.role())),
        );
    }
    let expected = [
        "ManifestProfile",
        "ManifestPlane",
        "ProfileGroundLayer",
        "ProfileSchema",
        "ProfileTransitions",
        "SchemaTarget { field: \"entries[].item\" }",
        "DefinitionType",
        "Field { field: \"entries[].item\" }",
        "ZoneParent",
        "DefaultZone",
        "GroundPalette { layer: \"floor\" }",
        "ZonePalette",
        "Proto",
        "Layer",
        "PortalPlane",
        "PortalAnchor { implicit_plane: true }",
        "PortalAnchor { implicit_plane: false }",
        "StairwellStep",
        "TerrainLayer",
        "TerrainProto",
        "TransitionLayer",
        "TransitionBlend",
        "TransitionTile",
    ];
    for role in expected {
        assert!(roles.contains(role), "{role} is not recorded: {roles:#?}");
    }
}

#[test]
fn an_explicit_portal_plane_is_recorded_with_its_anchor() {
    let root = workspace();
    edit(
        root.path(),
        super::CHUNK,
        "\"portal\": {\"anchor\": \"cave.entrance\"}",
        "\"portal\": {\"plane\": \"overworld\", \"anchor\": \"cave.entrance\"}",
    );
    let (world, index) = indexed(root.path());
    check_spans(root.path(), &world, &index);
    let anchor: Vec<(Role<'_>, Holder<'_>)> = index
        .to(Target::Anchor {
            plane: "overworld",
            id: "cave.entrance",
        })
        .map(|reference| (reference.role(), reference.holder()))
        .collect();
    let [(role, holder)] = anchor[..] else {
        panic!("{anchor:?}");
    };
    assert_eq!(
        role,
        Role::PortalAnchor {
            implicit_plane: false
        }
    );
    let planes: Vec<Role<'_>> = index
        .from(holder)
        .map(|reference| reference.role())
        .collect();
    assert_eq!(planes, [Role::Proto, Role::PortalPlane, anchor[0].0]);
}

#[test]
fn an_implicit_portal_plane_is_the_portals_own() {
    let root = normalised(&fixture());
    let (_, index) = indexed(root.path());
    let to: Vec<(Role<'_>, &str, String)> = index
        .to(Target::Anchor {
            plane: "overworld",
            id: "cave.entrance",
        })
        .map(|reference| {
            (
                reference.role(),
                reference.file(),
                text_at(root.path(), &reference),
            )
        })
        .collect();
    assert_eq!(
        to,
        [(
            Role::PortalAnchor {
                implicit_plane: true
            },
            super::CHUNK,
            "\"cave.entrance\"".to_owned()
        )]
    );
    // No other plane's anchor of that name is named.
    assert_eq!(
        index
            .to(Target::Anchor {
                plane: "vault",
                id: "cave.entrance"
            })
            .len(),
        0
    );
}

#[test]
fn nested_reference_fields_carry_their_schema_path() {
    let root = super::with_loot(
        r#"{"entries": [{"item": "coin", "weight": 1}, {"item": "tree", "weight": 2}]}"#,
    );
    let (world, index) = indexed(root.path());
    check_spans(root.path(), &world, &index);
    let loot = Holder::Definition {
        kind: "loot",
        id: "chest_common",
    };
    let fields: Vec<(Role<'_>, Target<'_>, String)> = index
        .from(loot)
        .map(|reference| {
            (
                reference.role(),
                reference.target(),
                text_at(root.path(), &reference),
            )
        })
        .collect();
    let item = Role::Field {
        field: "entries[].item",
    };
    assert_eq!(
        fields,
        [
            (
                Role::DefinitionType,
                Target::DefinitionType("loot"),
                "\"loot\"".to_owned()
            ),
            (
                item,
                Target::Definition {
                    kind: "proto",
                    id: "coin"
                },
                "\"coin\"".to_owned()
            ),
            (
                item,
                Target::Definition {
                    kind: "proto",
                    id: "tree"
                },
                "\"tree\"".to_owned()
            ),
        ]
    );
    // The schema's own `reference` types, at their `target` values.
    let schema: Vec<Role<'_>> = index
        .from(Holder::Schema {
            path: "profile/schema/loot.jsonc",
        })
        .map(|reference| reference.role())
        .collect();
    assert_eq!(
        schema,
        [Role::SchemaTarget {
            field: "entries[].item"
        }]
    );
}

#[test]
fn reference_fields_in_objects_and_lists_of_objects_are_recorded() {
    let root = workspace();
    // A schema with a reference in an object in a list in an object.
    fs::write(
        root.path().join("profile/schema/loot.jsonc"),
        r#"{"format": "tilewright.schema/1", "fields": {
          "table": {"type": "object", "fields": {
            "rows": {"type": "list", "items": {"type": "object", "fields": {
              "pick": {"type": "list", "items": {"type": "reference", "target": "proto"}}
            }}},
            "fallback": {"type": "reference", "target": "loot"}
          }}
        }}"#,
    )
    .unwrap();
    fs::write(
        root.path().join(super::LOOT),
        r#"{"format": "tilewright.def/1", "type": "loot", "id": "chest_common", "fields": {"table": {"rows": [{"pick": ["coin", "tree"]}, {"pick": ["wall"]}], "fallback": "chest_common"}}}"#,
    )
    .unwrap();
    let (world, index) = indexed(root.path());
    check_spans(root.path(), &world, &index);
    let fields: Vec<(String, String)> = index
        .from(Holder::Definition {
            kind: "loot",
            id: "chest_common",
        })
        .filter_map(|reference| match reference.role() {
            Role::Field { field } => Some((field.to_owned(), text_at(root.path(), &reference))),
            _ => None,
        })
        .collect();
    let expected = [
        ("table.rows[].pick[]", "\"coin\""),
        ("table.rows[].pick[]", "\"tree\""),
        ("table.rows[].pick[]", "\"wall\""),
        ("table.fallback", "\"chest_common\""),
    ];
    assert_eq!(
        fields,
        expected.map(|(field, text)| (field.to_owned(), text.to_owned()))
    );
    let schema: BTreeSet<String> = index
        .from(Holder::Schema {
            path: "profile/schema/loot.jsonc",
        })
        .map(|reference| format!("{:?} {:?}", reference.role(), reference.target()))
        .collect();
    assert_eq!(
        schema,
        BTreeSet::from([
            "SchemaTarget { field: \"table.rows[].pick[]\" } DefinitionType(\"proto\")".to_owned(),
            "SchemaTarget { field: \"table.fallback\" } DefinitionType(\"loot\")".to_owned(),
        ])
    );
}

#[test]
fn palette_entries_are_recorded_at_their_values() {
    let root = normalised(&fixture());
    let (world, index) = indexed(root.path());
    let chunk = Holder::Chunk {
        plane: "overworld",
        chunk: (0, 0, 0),
    };
    let palettes: Vec<(Role<'_>, Target<'_>)> = index
        .from(chunk)
        .map(|reference| (reference.role(), reference.target()))
        .collect();
    let plane = &world.planes()["overworld"].chunks[&(0, 0, 0)];
    let mut expected = Vec::new();
    for (layer, grid) in world.profile().ground_layers.iter().zip(&plane.ground) {
        for proto in &grid.palette {
            expected.push((
                Role::GroundPalette { layer },
                Target::Definition {
                    kind: "proto",
                    id: proto,
                },
            ));
        }
    }
    for zone in &plane.zones.palette {
        expected.push((
            Role::ZonePalette,
            Target::Zone {
                plane: "overworld",
                id: zone,
            },
        ));
    }
    // Source order is ground layers first, as the fixture writes them.
    expected.sort_by_key(|(role, _)| matches!(role, Role::ZonePalette));
    assert_eq!(palettes, expected);
    assert!(palettes.len() >= 3, "{palettes:?}");
}

#[test]
fn zone_parents_and_default_zones_are_recorded() {
    let root = normalised(&fixture());
    let (_, index) = indexed(root.path());
    let island = Target::Zone {
        plane: "overworld",
        id: "island",
    };
    let to: Vec<(Role<'_>, Holder<'_>)> = index
        .to(island)
        .filter(|reference| reference.role() != Role::ZonePalette)
        .map(|reference| (reference.role(), reference.holder()))
        .collect();
    assert_eq!(
        to,
        [(
            Role::ZoneParent,
            Holder::Zone {
                plane: "overworld",
                zone: "village"
            }
        )]
    );
    let wilderness: Vec<(Role<'_>, Holder<'_>, &str)> = index
        .to(Target::Zone {
            plane: "overworld",
            id: "wilderness",
        })
        .filter(|reference| reference.role() != Role::ZonePalette)
        .map(|reference| (reference.role(), reference.holder(), reference.file()))
        .collect();
    assert_eq!(
        wilderness,
        [
            (
                Role::DefaultZone,
                Holder::Plane { plane: "overworld" },
                "planes/overworld/plane.jsonc"
            ),
            (
                Role::ZoneParent,
                Holder::Zone {
                    plane: "overworld",
                    zone: "island"
                },
                "planes/overworld/zones.jsonc"
            ),
        ]
    );
}

#[test]
fn lookups_are_ordered_and_absent_targets_have_none() {
    let root = normalised(&fixture());
    let (world, index) = indexed(root.path());
    assert_eq!(index.len(), index.iter().count());
    assert!(!index.is_empty());
    // Grouped by target, and in source order within one.
    let all: Vec<Reference<'_>> = index.iter().collect();
    for pair in all.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        if a.target() == b.target() {
            assert!(
                (a.file(), a.span().start) < (b.file(), b.span().start),
                "{a:?} then {b:?}"
            );
        }
    }
    let mut seen = BTreeSet::new();
    let mut total = 0;
    for reference in index.iter() {
        let target = reference.target();
        if seen.insert(format!("{target:?}")) {
            let to: Vec<Reference<'_>> = index.to(target).collect();
            assert!(to.iter().all(|found| found.target() == target));
            total += to.len();
        }
    }
    assert_eq!(total, index.len());
    // Every reference is found from its holder exactly once.
    let mut holders = BTreeSet::new();
    let mut from = 0;
    for reference in index.iter() {
        if holders.insert(format!("{:?}", reference.holder())) {
            let held: Vec<Reference<'_>> = index.from(reference.holder()).collect();
            assert!(
                held.iter()
                    .all(|found| found.holder() == reference.holder())
            );
            for pair in held.windows(2) {
                assert!(pair[0].span().start < pair[1].span().start);
            }
            from += held.len();
        }
    }
    assert_eq!(from, index.len());
    for absent in [
        Target::Definition {
            kind: "proto",
            id: "missing",
        },
        Target::Definition {
            kind: "missing",
            id: "tree",
        },
        Target::Plane("nowhere"),
        Target::Zone {
            plane: "vault",
            id: "island",
        },
        Target::File(""),
    ] {
        assert_eq!(index.to(absent).len(), 0, "{absent:?}");
    }
    assert_eq!(
        index
            .from(Holder::Definition {
                kind: "proto",
                id: "missing"
            })
            .len(),
        0
    );
    // A definition nothing names is not a target.
    assert!(world.definitions().len() > 1);
    assert_eq!(
        index
            .to(Target::Definition {
                kind: "recipe",
                id: "chest"
            })
            .len(),
        0
    );
    assert!(
        index
            .to(Target::Definition {
                kind: "proto",
                id: "tree"
            })
            .len()
            >= 1
    );
}

#[test]
fn an_indexed_load_gives_the_same_world_as_any_other() {
    for from in [fixture(), terrain_fixture()] {
        let root = SourceRoot::new(&from);
        let plain = load_edited_in(&root, &LoadOptions::default(), &Overlay::new()).unwrap();
        let (world, _) = load_indexed_in(&root, &LoadOptions::default(), &Overlay::new()).unwrap();
        assert_eq!(plain, world);
        assert_eq!(plain.source_digest(), world.source_digest());
    }
}

/// Trees that fail with each kind of reference, by name: one edit of a
/// fixture each, with `\n` line endings.
#[allow(clippy::too_many_lines)]
fn broken_trees() -> Vec<(String, tempfile::TempDir)> {
    let chunk = super::CHUNK;
    let transitions = "profile/transitions.jsonc";
    let cases: &[(&str, &str, &str, &str)] = &[
        (
            "basic",
            chunk,
            "\"proto\": \"stairs_down\"",
            "\"proto\": \"nope\"",
        ),
        (
            "basic",
            chunk,
            "\"layer\": \"wall\", \"fields\"",
            "\"layer\": \"roof\", \"fields\"",
        ),
        (
            "basic",
            chunk,
            "\"loot\": \"chest_common\"",
            "\"loot\": \"nope\"",
        ),
        (
            "basic",
            chunk,
            "\"anchor\": \"cave.entrance\"",
            "\"anchor\": \"nope\"",
        ),
        (
            "basic",
            chunk,
            "\"plane\": \"vault\"",
            "\"plane\": \"nope\"",
        ),
        ("basic", chunk, "\"v\": \"village\"", "\"v\": \"nope\""),
        ("basic", chunk, "\"g\": \"grass\"", "\"g\": \"nope\""),
        (
            "basic",
            "planes/overworld/plane.jsonc",
            "\"wilderness\"",
            "\"nope\"",
        ),
        (
            "basic",
            "planes/overworld/zones.jsonc",
            "\"parent\": \"island\"",
            "\"parent\": \"nope\"",
        ),
        (
            "basic",
            "planes/overworld/zones.jsonc",
            "\"parent\": \"island\"",
            "\"parent\": \"village\"",
        ),
        (
            "basic",
            "planes/overworld/stairwells.jsonc",
            "\"stairs_up\"",
            "\"nope\"",
        ),
        (
            "basic",
            "profile/profile.jsonc",
            "\"ground_layers\": [\"floor\"",
            "\"ground_layers\": [\"nope\"",
        ),
        (
            "basic",
            "profile/profile.jsonc",
            "\"ground_layers\": [\"floor\"",
            "\"ground_layers\": [\"zone\"",
        ),
        (
            "basic",
            "profile/profile.jsonc",
            "\"schema/loot.jsonc\"",
            "\"schema/nope.jsonc\"",
        ),
        (
            "basic",
            "profile/schema/loot.jsonc",
            "\"target\": \"proto\"",
            "\"target\": \"nope\"",
        ),
        ("basic", "world.jsonc", "\"vault\"", "\"nope\""),
        ("basic", super::LOOT, "\"chest\"", "\"nope\""),
        (
            "terrain",
            transitions,
            "\"layer\": \"floor\"",
            "\"layer\": \"nope\"",
        ),
        (
            "terrain",
            transitions,
            "\"layer\": \"edge\"",
            "\"layer\": \"nope\"",
        ),
        (
            "terrain",
            transitions,
            "\"protos\": [\"sand\"]",
            "\"protos\": [\"nope\"]",
        ),
        (
            "terrain",
            transitions,
            "\"with\": [\"dirt\"]",
            "\"with\": [\"nope\"]",
        ),
        ("terrain", transitions, "\"0\": \"pond\"", "\"0\": \"nope\""),
    ];
    let mut trees = Vec::new();
    for (fixture_name, path, from, to) in cases {
        let source = match *fixture_name {
            "basic" => fixture(),
            _ => terrain_fixture(),
        };
        let root = normalised(&source);
        edit(root.path(), path, from, to);
        trees.push((format!("{fixture_name} {path} {to}"), root));
    }
    // Every prototype of the chunk renamed, past a small diagnostic cap.
    let root = normalised(&fixture());
    let text = fs::read_to_string(root.path().join(chunk)).unwrap();
    fs::write(
        root.path().join(chunk),
        text.replace("\"proto\": \"", "\"proto\": \"x"),
    )
    .unwrap();
    trees.push(("basic every proto".to_owned(), root));
    trees
}

#[test]
fn an_indexed_load_of_a_broken_tree_gives_the_same_diagnostics() {
    for (name, tree) in broken_trees() {
        for options in [
            LoadOptions::default(),
            LoadOptions {
                maximum_diagnostics: 2,
                ..LoadOptions::default()
            },
        ] {
            let root = SourceRoot::new(tree.path());
            let plain = load_edited_in(&root, &options, &Overlay::new()).unwrap_err();
            let indexed = load_indexed_in(&root, &options, &Overlay::new()).unwrap_err();
            assert_eq!(plain, indexed, "{name}");
        }
    }
}

/// The diagnostics of a broken reference keep their codes, wording and
/// places; the golden was checked against the loader before references
/// had one check.
#[test]
fn broken_references_are_reported_as_they_always_were_golden() {
    let mut text = String::new();
    for (name, tree) in broken_trees() {
        let _ = writeln!(text, "{name}");
        for diagnostic in load_edited_in(
            &SourceRoot::new(tree.path()),
            &LoadOptions::default(),
            &Overlay::new(),
        )
        .unwrap_err()
        {
            let _ = writeln!(
                text,
                "  {} {}..{} {}:{} {} {}",
                diagnostic.path,
                diagnostic.start,
                diagnostic.end,
                diagnostic.line,
                diagnostic.column,
                diagnostic.code,
                diagnostic.message
            );
        }
    }
    golden("reference-diagnostics", &text);
}

#[test]
fn an_indexed_load_reads_the_overlay() {
    let root = normalised(&fixture());
    let path = "defs/loot/chest_common.jsonc";
    let text = r#"{"format": "tilewright.def/1", "type": "loot", "id": "chest_common", "fields": {"entries": [{"item": "wall", "weight": 1}]}}"#;
    let overlay = Overlay::from([(path.to_owned(), Some(text.to_owned()))]);
    let (world, index) = indexed_with(root.path(), &overlay);
    let wall: Vec<(Holder<'_>, std::ops::Range<usize>)> = index
        .to(Target::Definition {
            kind: "proto",
            id: "wall",
        })
        .filter(|reference| reference.file() == path)
        .map(|reference| (reference.holder(), reference.span()))
        .collect();
    let start = text.find("\"wall\"").unwrap();
    assert_eq!(
        wall,
        [(
            Holder::Definition {
                kind: "loot",
                id: "chest_common"
            },
            start..start + "\"wall\"".len()
        )]
    );
    let length = u64::try_from(text.len()).unwrap();
    assert_eq!(world.files()[path].length(), length);
}

#[test]
fn an_index_holds_about_32_bytes_a_reference() {
    let root = normalised(&fixture());
    let (_, index) = indexed(root.path());
    // Small trees are dominated by strings; the records themselves are
    // 32 bytes and their holder order 4 more.
    let per_reference = index.heap_bytes() / index.len();
    assert!(per_reference < 200, "{per_reference} bytes a reference");
}

#[test]
fn copying_a_tree_keeps_its_index() {
    // The index depends on the bytes, not on where the tree is.
    let first = normalised(&fixture());
    let second = tempfile::tempdir().unwrap();
    copy_tree(first.path(), second.path());
    assert_eq!(indexed(first.path()).1, indexed(second.path()).1);
}
