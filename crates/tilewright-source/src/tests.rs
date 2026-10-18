// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

use super::{
    FieldValue, LoadOptions, PlaneKind, Portal, Projection, StairwellDirection, StairwellStep,
    load, load_with,
};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/basic")
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A writable copy of the fixture.
fn workspace() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    copy_tree(&fixture(), directory.path());
    directory
}

fn edit(root: &Path, path: &str, from: &str, to: &str) {
    let file = root.join(path);
    let text = fs::read_to_string(&file).unwrap();
    assert!(text.contains(from), "{path} does not contain {from:?}");
    fs::write(file, text.replacen(from, to, 1)).unwrap();
}

/// Loads `root` and returns `(code, path, line)` for every diagnostic.
fn problems(root: &Path) -> Vec<(&'static str, String, usize)> {
    load(root)
        .expect_err("the tree must be rejected")
        .into_iter()
        .map(|diagnostic| (diagnostic.code, diagnostic.path, diagnostic.line))
        .collect()
}

fn has(problems: &[(&'static str, String, usize)], code: &str, path: &str) -> bool {
    problems
        .iter()
        .any(|(found, found_path, _)| *found == code && found_path == path)
}

const CHUNK: &str = "planes/overworld/layers/0/0_0.jsonc";

#[test]
fn the_fixture_loads_with_every_feature() {
    let world = load(&fixture()).unwrap_or_else(|diagnostics| {
        panic!(
            "{}",
            diagnostics
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        )
    });
    assert_eq!(world.profile.id, "example");
    assert_eq!(
        world.profile.projection,
        Projection::Isometric {
            tile_width: 48,
            tile_height: 24
        }
    );
    assert_eq!(world.profile.ground_layers, ["floor", "mask"]);
    assert_eq!(world.profile.layer_height, Some(50));
    assert_eq!(world.planes.len(), 2);
    assert_eq!(world.definitions.len(), 12);
    let recipe = &world.definitions[&("recipe".to_owned(), "chest".to_owned())];
    assert_eq!(
        recipe.fields,
        [
            ("output".to_owned(), FieldValue::String("chest".to_owned())),
            (
                "ingredients".to_owned(),
                FieldValue::Json(
                    "[{\"item\":\"wall\",\"count\":2},{\"item\":\"coin\",\"count\":5}]".to_owned()
                )
            )
        ]
    );

    let overworld = &world.planes["overworld"];
    assert_eq!(overworld.kind, PlaneKind::Shared);
    assert_eq!(overworld.zones.len(), 3);
    assert_eq!(overworld.zones["village"].parent.as_deref(), Some("island"));
    assert_eq!(overworld.anchors["cave.entrance"].layer, -1);
    assert_eq!(
        overworld.chunks.keys().copied().collect::<Vec<_>>(),
        [(-1, 0, 0), (0, -1, 0), (0, 0, 0), (1, 0, 0)]
    );
    assert_eq!(world.planes["vault"].kind, PlaneKind::Instanced);

    let chunk = &overworld.chunks[&(0, 0, 0)];
    assert_eq!(chunk.ground[0].value(0), Some("sand"));
    assert_eq!(chunk.ground[0].value(1), Some("grass"));
    assert_eq!(chunk.ground[1].value(5 * 32 + 10), Some("edge_n"));
    assert_eq!(chunk.ground[1].value(0), None);
    assert_eq!(chunk.heights[12 * 32 + 12], 32);
    assert_eq!(chunk.zones.value(9 * 32 + 9), Some("village"));
    assert_eq!(chunk.objects.len(), 6);
    assert_eq!(
        chunk.objects[1].fields[1],
        (
            "tags".to_owned(),
            FieldValue::Json("[\"ruin\",\"north\"]".to_owned())
        )
    );
    let chest = &chunk.objects[4];
    assert_eq!(chest.at, Some([2, 2]));
    assert_eq!(chest.inventory.len(), 2);
    assert_eq!(
        chest.inventory[1].inventory[0].fields,
        [("count".to_owned(), FieldValue::Integer(1))]
    );
    assert_eq!(
        chunk.objects[3].portal,
        Some(Portal::Anchor {
            plane: None,
            anchor: "cave.entrance".to_owned()
        })
    );
    assert_eq!(
        chunk.objects[5].custom.as_deref(),
        Some("{\"example.org/notes\":{\"reviewed\":true}}")
    );
    let cave = &overworld.chunks[&(-1, 0, 0)];
    assert!(cave.ground[0].cells.iter().all(|cell| *cell == 1));
    assert_eq!(cave.ground[0].palette, ["cave_floor"]);
}

#[test]
fn the_fixture_has_straight_sloped_and_one_way_stairwells() {
    let world = load(&fixture()).expect("the fixture is valid");
    let overworld = &world.planes["overworld"];
    let step = |layer, at: [i64; 2], proto: &str| StairwellStep {
        layer,
        at,
        proto: proto.to_owned(),
    };
    assert_eq!(
        overworld.stairwells.keys().collect::<Vec<_>>(),
        ["cave.chute", "cliff.path", "tower.stairs"]
    );
    let tower = &overworld.stairwells["tower.stairs"];
    assert_eq!(
        tower.steps,
        [
            step(-1, [20, 20], "stairs_up"),
            step(0, [20, 20], "stairs_up"),
            step(1, [20, 20], "stairs_down")
        ]
    );
    assert_eq!(tower.direction, StairwellDirection::Both);
    assert!(tower.hidden_on_other_layers);
    assert_eq!(tower.lower_end(), tower.steps.first());
    assert_eq!(tower.upper_end(), tower.steps.last());
    let cliff = &overworld.stairwells["cliff.path"];
    assert_eq!(cliff.steps[2], step(-1, [26, 9], "stairs_up"));
    assert!(!cliff.hidden_on_other_layers);
    assert_eq!(cliff.upper_end(), cliff.steps.first());
    let chute = &overworld.stairwells["cave.chute"];
    assert_eq!(chute.direction, StairwellDirection::Down);
    assert_eq!(
        chute.custom.as_deref(),
        Some("{\"example.org/notes\":{\"reviewed\":true}}")
    );
    assert!(world.planes["vault"].stairwells.is_empty());
}

#[test]
fn the_source_digest_is_stable_and_covers_every_byte() {
    let first = load(&fixture()).unwrap().source_digest;
    assert_eq!(load(&fixture()).unwrap().source_digest, first);
    let copy = workspace();
    edit(copy.path(), CHUNK, "\"tree\"}", "\"tree\" }");
    assert_ne!(load(copy.path()).unwrap().source_digest, first);
}

#[test]
fn shape_errors_carry_codes_paths_and_lines() {
    let copy = workspace();
    edit(copy.path(), CHUNK, "\"objects\": [", "\"objcts\": [");
    edit(
        copy.path(),
        "planes/overworld/layers/0/-1_0.jsonc",
        "\"objects\": []",
        "\"objects\": [{\"at\": [1, 1], \"proto\": \"tree\", \"fields\": {\"count\": \"3\"}}]",
    );
    let found = problems(copy.path());
    let unknown = found
        .iter()
        .find(|(code, path, _)| *code == "shape.unknown_key" && path == CHUNK)
        .expect("unknown key reported");
    assert!(unknown.2 > 100, "line of the objects key: {}", unknown.2);
    assert!(has(
        &found,
        "shape.type",
        "planes/overworld/layers/0/-1_0.jsonc"
    ));
}

#[test]
fn unresolved_references_are_reported() {
    let copy = workspace();
    edit(
        copy.path(),
        CHUNK,
        "\"proto\": \"tree\"}",
        "\"proto\": \"bush\"}",
    );
    edit(copy.path(), CHUNK, "\"cave.entrance\"", "\"cave.exit\"");
    edit(
        copy.path(),
        CHUNK,
        "\"plane\": \"vault\"",
        "\"plane\": \"attic\"",
    );
    edit(
        copy.path(),
        "defs/loot/chest_common.jsonc",
        "\"item\": \"coin\"",
        "\"item\": \"gem\"",
    );
    let found = problems(copy.path());
    let unresolved = found
        .iter()
        .filter(|(code, path, _)| *code == "ref.unresolved" && path == CHUNK)
        .count();
    assert_eq!(unresolved, 3, "{found:?}");
    assert!(has(
        &found,
        "ref.unresolved",
        "defs/loot/chest_common.jsonc"
    ));
}

#[test]
fn paths_must_agree_with_content() {
    let copy = workspace();
    edit(copy.path(), CHUNK, "\"chunk\": [0, 0]", "\"chunk\": [1, 0]");
    edit(
        copy.path(),
        "defs/proto/tree.jsonc",
        "\"id\": \"tree\"",
        "\"id\": \"oak\"",
    );
    let found = problems(copy.path());
    assert!(has(&found, "source.name_mismatch", CHUNK));
    assert!(has(&found, "source.name_mismatch", "defs/proto/tree.jsonc"));
}

#[test]
fn unexpected_and_non_canonical_files_are_rejected() {
    let copy = workspace();
    let root = copy.path();
    fs::write(root.join("planes/overworld/notes.txt"), "x").unwrap();
    fs::copy(
        root.join(CHUNK),
        root.join("planes/overworld/layers/0/00_1.jsonc"),
    )
    .unwrap();
    fs::create_dir(root.join("planes/attic")).unwrap();
    fs::create_dir(root.join("defs/sound")).unwrap();
    let found = problems(root);
    for path in [
        "planes/overworld/notes.txt",
        "planes/overworld/layers/0/00_1.jsonc",
        "planes/attic",
        "defs/sound",
    ] {
        assert!(
            has(&found, "source.unexpected_file", path),
            "{path}: {found:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn symbolic_links_are_not_followed() {
    let copy = workspace();
    let root = copy.path();
    std::os::unix::fs::symlink(
        root.join(CHUNK),
        root.join("planes/overworld/layers/0/1_0.jsonc"),
    )
    .unwrap();
    assert!(has(
        &problems(root),
        "source.symlink",
        "planes/overworld/layers/0/1_0.jsonc"
    ));
}

#[test]
fn grids_zones_and_fields_are_checked() {
    let copy = workspace();
    let root = copy.path();
    edit(
        root,
        CHUNK,
        "\"ggggggsggggggsggggggsggggggsgggg\"",
        "\"gggggsggggggsggggggsggggggsgggg\"",
    );
    edit(
        root,
        "planes/overworld/zones.jsonc",
        "{\"id\": \"wilderness\", \"name\": \"Wilderness\"}",
        "{\"id\": \"wilderness\", \"parent\": \"village\", \"name\": \"Wilderness\"}",
    );
    edit(
        root,
        "defs/proto/sand.jsonc",
        "\"face\": \"sand.png\", ",
        "",
    );
    let found = problems(root);
    assert!(has(&found, "grid.invalid", CHUNK), "{found:?}");
    assert!(has(&found, "ref.cycle", "planes/overworld/zones.jsonc"));
    assert!(has(&found, "shape.missing_key", "defs/proto/sand.jsonc"));
}

#[test]
fn syntax_errors_use_the_jsonc_codes() {
    let copy = workspace();
    edit(copy.path(), "world.jsonc", "\"vault\"]", "\"vault\",]");
    let found = problems(copy.path());
    assert_eq!(
        found,
        [("jsonc.trailing_comma", "world.jsonc".to_owned(), 5)]
    );
}

#[test]
fn missing_planes_and_bad_custom_keys_are_reported() {
    let copy = workspace();
    let root = copy.path();
    edit(root, "world.jsonc", "\"vault\"]", "\"vault\", \"attic\"]");
    edit(root, CHUNK, "\"example.org/notes\"", "\"notes\"");
    let found = problems(root);
    assert!(has(
        &found,
        "source.missing_file",
        "planes/attic/plane.jsonc"
    ));
    assert!(has(&found, "shape.identifier", CHUNK));
}

#[test]
fn loads_stop_at_their_budget() {
    let options = LoadOptions {
        maximum_files: 4,
        ..LoadOptions::default()
    };
    let found = load_with(&fixture(), &options).expect_err("budget exceeded");
    assert!(
        found
            .iter()
            .any(|diagnostic| diagnostic.code == "source.budget")
    );
}

#[test]
fn extreme_chunk_names_are_rejected_without_overflow() {
    let copy = workspace();
    let root = copy.path();
    for name in [
        "-9223372036854775808_0.jsonc",
        "0_-9223372036854775808.jsonc",
    ] {
        fs::copy(
            root.join(CHUNK),
            root.join("planes/overworld/layers/0").join(name),
        )
        .unwrap();
    }
    let found = problems(root);
    for name in [
        "-9223372036854775808_0.jsonc",
        "0_-9223372036854775808.jsonc",
    ] {
        let path = format!("planes/overworld/layers/0/{name}");
        assert!(
            has(&found, "source.unexpected_file", &path),
            "{path}: {found:?}"
        );
    }
}

#[test]
fn directory_listings_count_against_the_budget() {
    let copy = workspace();
    let root = copy.path();
    for index in 0..50 {
        fs::write(root.join(format!("planes/overworld/junk{index}")), "x").unwrap();
    }
    let options = LoadOptions {
        maximum_entries: 20,
        ..LoadOptions::default()
    };
    let found = load_with(root, &options).expect_err("entry budget exceeded");
    assert!(
        found
            .iter()
            .any(|diagnostic| diagnostic.code == "source.budget")
    );
    // Enumeration stopped early: not every junk file was reported.
    let reported = found
        .iter()
        .filter(|diagnostic| diagnostic.code == "source.unexpected_file")
        .count();
    assert!(reported < 50, "{reported} unexpected files reported");
}

#[test]
fn positions_match_a_direct_count() {
    // Several checkpoints long, with multi-byte characters and lines that
    // cross checkpoints.
    let text: String = (0..900)
        .map(|index| match index % 97 {
            96 => '\n',
            value if value % 3 == 0 => 'a',
            value if value % 3 == 1 => 'é',
            _ => '🦀',
        })
        .collect();
    let positions = super::diagnostic::Positions::new(&text);
    for offset in 0..=text.len() + 2 {
        let mut boundary = offset.min(text.len());
        while !text.is_char_boundary(boundary) {
            boundary -= 1;
        }
        let before = &text[..boundary];
        let start = before.rfind('\n').map_or(0, |index| index + 1);
        let expected = (
            before.matches('\n').count() + 1,
            text[start..boundary].chars().count() + 1,
        );
        assert_eq!(positions.locate(offset), expected, "offset {offset}");
    }
    let exact = "a".repeat(512);
    assert_eq!(
        super::diagnostic::Positions::new(&exact).locate(512),
        (1, 513)
    );
    assert_eq!(super::diagnostic::Positions::new("").locate(0), (1, 1));
}

#[test]
fn many_objects_keep_exact_positions() {
    let copy = workspace();
    let root = copy.path();
    let count = 20_000;
    let mut objects = vec!["{\"at\": [1, 1], \"proto\": \"coin\"}"; count];
    objects.push("{\"at\": [1, 1], \"proto\": \"missing\"}");
    fs::write(
        root.join("planes/vault/layers/0/0_0.jsonc"),
        format!(
            "{{\"format\": \"tilewright.chunk/1\", \"plane\": \"vault\", \"layer\": 0, \"chunk\": [0, 0], \"objects\": [\n{}\n]}}\n",
            objects.join(",\n")
        ),
    )
    .unwrap();
    let found = load(root).expect_err("the missing proto is reported");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(
        (found[0].code, found[0].line, found[0].column),
        ("ref.unresolved", count + 2, 25)
    );
}

#[test]
fn profile_lists_are_capped_before_their_entries_are_checked() {
    const LAYERS: &str =
        "\"render_layers\": [\"floor\", \"mask\", \"item\", \"wall\", \"living\", \"effect\"]";
    let copy = workspace();
    edit(
        copy.path(),
        "profile/profile.jsonc",
        LAYERS,
        &format!("\"render_layers\": [{}]", vec!["\"X\""; 65_535].join(", ")),
    );
    assert_eq!(
        problems(copy.path()),
        [("shape.range", "profile/profile.jsonc".to_owned(), 6)]
    );

    let copy = workspace();
    let mut layers: Vec<String> = (0..60_000)
        .map(|index| format!("\"layer.{index}\""))
        .collect();
    layers.extend(["\"floor\"", "\"mask\"", "\"layer.59999\""].map(str::to_owned));
    edit(
        copy.path(),
        "profile/profile.jsonc",
        LAYERS,
        &format!("\"render_layers\": [{}]", layers.join(", ")),
    );
    assert_eq!(
        problems(copy.path()),
        [("ref.duplicate", "profile/profile.jsonc".to_owned(), 6)]
    );

    let copy = workspace();
    edit(
        copy.path(),
        "profile/schema/proto.jsonc",
        "\"values\": [\"floor\",",
        "\"values\": [\"floor\", \"floor\",",
    );
    assert!(has(
        &problems(copy.path()),
        "ref.duplicate",
        "profile/schema/proto.jsonc"
    ));
}

#[test]
fn unknown_object_layers_are_reported() {
    let copy = workspace();
    edit(
        copy.path(),
        CHUNK,
        "\"layer\": \"wall\"",
        "\"layer\": \"roof\"",
    );
    let found = problems(copy.path());
    assert_eq!(found, [("ref.unresolved", CHUNK.to_owned(), 157)]);
}

#[test]
fn long_zone_chains_are_walked_once() {
    let copy = workspace();
    // A rooted chain listed leaf first, a two-zone loop, and a chain that
    // runs into the loop.
    let length = 5_000;
    let mut zones: Vec<String> = (1..length)
        .rev()
        .map(|index| format!("{{\"id\": \"z{index}\", \"parent\": \"z{}\"}}", index - 1))
        .collect();
    zones.push("{\"id\": \"z0\"}".to_owned());
    zones.push("{\"id\": \"loop.a\", \"parent\": \"loop.b\"}".to_owned());
    zones.push("{\"id\": \"loop.b\", \"parent\": \"loop.a\"}".to_owned());
    zones.extend((0..length).map(|index| {
        let parent = if index == 0 {
            "loop.a".to_owned()
        } else {
            format!("tail{}", index - 1)
        };
        format!("{{\"id\": \"tail{index}\", \"parent\": \"{parent}\"}}")
    }));
    fs::write(
        copy.path().join("planes/vault/zones.jsonc"),
        format!(
            "{{\"format\": \"tilewright.zones/1\", \"zones\": [\n{}\n]}}\n",
            zones.join(",\n")
        ),
    )
    .unwrap();
    let found = problems(copy.path());
    assert!(
        found
            .iter()
            .all(|(code, path, _)| *code == "ref.cycle" && path == "planes/vault/zones.jsonc"),
        "{found:?}"
    );
    assert_eq!(found.len(), length + 2);
}

#[cfg(target_os = "linux")]
#[test]
fn files_that_outgrow_their_metadata_count_against_the_budget() {
    // Files in /proc report a length of 0 yet read as text, like a file
    // that grows after its size was checked.
    let options = LoadOptions {
        maximum_total_bytes: 16,
        ..LoadOptions::default()
    };
    let mut tree = super::files::Tree::new(Path::new("/proc/self"), &options);
    assert!(tree.document("status").is_none());
    let found = tree.take_diagnostics().finish();
    assert!(
        found
            .iter()
            .any(|diagnostic| diagnostic.code == "source.budget"),
        "{found:?}"
    );
}

#[cfg(unix)]
#[test]
fn linked_top_level_directories_are_reported() {
    let copy = workspace();
    let root = copy.path();
    for name in ["defs", "planes"] {
        let real = root.join(format!("{name}-real"));
        fs::rename(root.join(name), &real).unwrap();
        std::os::unix::fs::symlink(&real, root.join(name)).unwrap();
    }
    let found = problems(root);
    assert!(has(&found, "source.symlink", "defs"), "{found:?}");
    assert!(has(&found, "source.symlink", "planes"), "{found:?}");
}

#[test]
fn boundary_chunks_load_and_the_next_ones_are_rejected() {
    // The last chunk on each side whose tiles stay within ±(2^53 - 1).
    const EDGE: i64 = 281_474_976_710_655;
    let copy = workspace();
    let root = copy.path();
    let layer = root.join("planes/vault/layers/0");
    let text = fs::read_to_string(layer.join("0_0.jsonc")).unwrap();
    for (cx, cy) in [(EDGE, 0), (-EDGE, 0), (0, EDGE), (0, -EDGE)] {
        fs::write(
            layer.join(format!("{cx}_{cy}.jsonc")),
            text.replace("\"chunk\": [0, 0]", &format!("\"chunk\": [{cx}, {cy}]")),
        )
        .unwrap();
    }
    let world = load(root).expect("boundary chunks are valid");
    assert_eq!(world.planes["vault"].chunks.len(), 5);
    let names = [
        format!("{}_0.jsonc", EDGE + 1),
        format!("0_{}.jsonc", -EDGE - 1),
    ];
    for name in &names {
        fs::write(layer.join(name), &text).unwrap();
    }
    let found = problems(root);
    for name in &names {
        let path = format!("planes/vault/layers/0/{name}");
        assert!(
            has(&found, "source.unexpected_file", &path),
            "{path}: {found:?}"
        );
    }
}

#[test]
fn missing_required_fields_are_one_diagnostic_per_owner() {
    let copy = workspace();
    let required: Vec<String> = (1..=5)
        .map(|index| format!("\"r{index}\": {{\"type\": \"boolean\", \"required\": true}}"))
        .collect();
    edit(
        copy.path(),
        "profile/schema/object-fields.jsonc",
        "\"fields\": {",
        &format!("\"fields\": {{{}, ", required.join(", ")),
    );
    let found = load(copy.path()).expect_err("required fields are missing");
    let missing: Vec<_> = found
        .iter()
        .filter(|diagnostic| diagnostic.code == "shape.missing_key")
        .collect();
    assert!(!missing.is_empty());
    for diagnostic in &missing {
        assert_eq!(
            diagnostic.message,
            "required fields \"r1\", \"r2\", \"r3\" and 2 more are missing"
        );
    }
    let owners: std::collections::BTreeSet<_> = missing
        .iter()
        .map(|diagnostic| (&diagnostic.path, diagnostic.start))
        .collect();
    assert_eq!(owners.len(), missing.len());
}

#[test]
fn roles_naming_one_schema_share_it() {
    let copy = workspace();
    edit(
        copy.path(),
        "profile/profile.jsonc",
        "\"loot\": {\"schema\": \"schema/loot.jsonc\"}",
        "\"loot\": {\"schema\": \"schema/loot.jsonc\"}, \"scenery\": {\"schema\": \"schema/proto.jsonc\"}",
    );
    let world = load(copy.path()).expect("valid");
    let types = &world.profile.definition_types;
    assert!(std::sync::Arc::ptr_eq(&types["proto"], &types["scenery"]));
}

#[test]
fn document_paths_have_at_most_32_components() {
    for (depth, valid) in [(31, true), (32, false)] {
        let copy = workspace();
        let root = copy.path();
        let nested = vec!["p"; depth].join("/");
        copy_tree(&root.join("profile"), &root.join(&nested));
        edit(
            root,
            "world.jsonc",
            "\"profile/profile.jsonc\"",
            &format!("\"{nested}/profile.jsonc\""),
        );
        if valid {
            load(root).expect("32 components are allowed");
        } else {
            assert_eq!(
                problems(root),
                [("source.path", "world.jsonc".to_owned(), 4)]
            );
        }
    }
}

#[test]
fn diagnostics_stop_at_their_cap() {
    let copy = workspace();
    let scalars = vec!["1"; 500].join(", ");
    fs::write(
        copy.path().join("planes/vault/layers/0/0_0.jsonc"),
        format!(
            "{{\"format\": \"tilewright.chunk/1\", \"plane\": \"vault\", \"layer\": 0, \"chunk\": [0, 0], \"objects\": [{scalars}]}}\n"
        ),
    )
    .unwrap();
    let options = LoadOptions {
        maximum_diagnostics: 10,
        ..LoadOptions::default()
    };
    let found = load_with(copy.path(), &options).expect_err("scalars are not objects");
    assert_eq!(found.len(), 11, "{found:?}");
    let budget: Vec<_> = found
        .iter()
        .filter(|diagnostic| diagnostic.code == "source.budget")
        .collect();
    assert_eq!(budget.len(), 1);
    assert_eq!(
        budget[0].message,
        "more than 10 problems; the rest are not reported"
    );
    assert_eq!(load(copy.path()).expect_err("still invalid").len(), 500);
}

#[test]
fn custom_data_is_only_allowed_where_it_is_kept() {
    let copy = workspace();
    let root = copy.path();
    edit(
        root,
        "world.jsonc",
        "\"format\": \"tilewright.world/1\",",
        "\"format\": \"tilewright.world/1\", \"custom\": {\"example.org\": 1},",
    );
    let found = problems(root);
    assert!(has(&found, "shape.unknown_key", "world.jsonc"), "{found:?}");

    let copy = workspace();
    let root = copy.path();
    edit(
        root,
        "planes/vault/layers/0/0_0.jsonc",
        "\"layer\": 0,",
        "\"layer\": 0, \"custom\": {\"example.org\": 1},",
    );
    edit(
        root,
        "planes/overworld/anchors.jsonc",
        "\"at\": [10, 12]}",
        "\"at\": [10, 12], \"custom\": {\"example.org\": 1}}",
    );
    let found = problems(root);
    assert!(
        has(
            &found,
            "shape.unknown_key",
            "planes/vault/layers/0/0_0.jsonc"
        ),
        "{found:?}"
    );
    assert!(
        has(
            &found,
            "shape.unknown_key",
            "planes/overworld/anchors.jsonc"
        ),
        "{found:?}"
    );
}

#[test]
fn heights_z_and_sub_layers_have_their_documented_ranges() {
    const VAULT: &str = "planes/vault/layers/0/0_0.jsonc";
    let set = |root: &Path, terrain: &str, object: &str| {
        edit(
            root,
            VAULT,
            "\"terrain\": {\"floor\": {\"palette\": {\"c\": \"cave_floor\"}, \"fill\": \"c\"}}",
            &format!(
                "\"terrain\": {{\"floor\": {{\"palette\": {{\"c\": \"cave_floor\"}}, \"fill\": \"c\"}}, \"height\": {terrain}}}"
            ),
        );
        edit(
            root,
            VAULT,
            "\"proto\": \"chest\",",
            &format!("\"proto\": \"chest\", {object},"),
        );
    };
    let copy = workspace();
    set(
        copy.path(),
        "{\"fill\": 2147483647}",
        "\"sub_layer\": -32768, \"z\": -2147483648",
    );
    load(copy.path()).expect("the extremes are valid");
    for (terrain, object) in [
        ("{\"fill\": 2147483648}", "\"z\": 0"),
        ("{\"fill\": 0}", "\"sub_layer\": 32768"),
        ("{\"fill\": 0}", "\"z\": 2147483648"),
    ] {
        let copy = workspace();
        set(copy.path(), terrain, object);
        let found = problems(copy.path());
        assert!(
            found.len() == 1 && has(&found, "shape.range", VAULT),
            "{terrain} {object}: {found:?}"
        );
    }
}

#[test]
fn source_containers_of_the_wrong_kind_are_reported() {
    let copy = workspace();
    let root = copy.path();
    fs::remove_dir_all(root.join("defs")).unwrap();
    fs::write(root.join("defs"), "not a directory").unwrap();
    assert!(has(&problems(root), "source.unexpected_file", "defs"));

    let copy = workspace();
    let root = copy.path();
    edit(
        root,
        "world.jsonc",
        "\"planes\": [\"overworld\", \"vault\"]",
        "\"planes\": []",
    );
    fs::remove_dir_all(root.join("planes")).unwrap();
    fs::write(root.join("planes"), "not a directory").unwrap();
    assert_eq!(
        problems(root),
        [("source.unexpected_file", "planes".to_owned(), 0)]
    );
}

#[test]
fn load_options_cannot_loosen_the_format_limits() {
    let copy = workspace();
    let deep = format!("{}1{}", "[".repeat(70), "]".repeat(70));
    edit(
        copy.path(),
        "defs/proto/coin.jsonc",
        "\"weight\": 0.01}",
        &format!("\"weight\": 0.01, \"footprint\": {deep}}}"),
    );
    let strict = problems(copy.path());
    let options = LoadOptions {
        limits: tilewright_jsonc::Limits {
            max_depth: 100_000,
            ..tilewright_jsonc::Limits::default()
        },
        ..LoadOptions::default()
    };
    let relaxed: Vec<_> = load_with(copy.path(), &options)
        .expect_err("still deeper than the format allows")
        .into_iter()
        .map(|diagnostic| (diagnostic.code, diagnostic.path, diagnostic.line))
        .collect();
    assert_eq!(relaxed, strict);
    assert!(
        has(&strict, "jsonc.depth_limit", "defs/proto/coin.jsonc"),
        "{strict:?}"
    );
}

#[test]
fn palette_keys_are_visible_ascii() {
    let copy = workspace();
    edit(
        copy.path(),
        "planes/vault/layers/0/0_0.jsonc",
        "{\"palette\": {\"c\": \"cave_floor\"}, \"fill\": \"c\"}",
        "{\"palette\": {\" \": \"cave_floor\"}, \"fill\": \" \"}",
    );
    assert!(has(
        &problems(copy.path()),
        "grid.invalid",
        "planes/vault/layers/0/0_0.jsonc"
    ));
}

#[test]
fn zones_are_listed_once_per_palette() {
    let copy = workspace();
    edit(
        copy.path(),
        CHUNK,
        "\"palette\": {\"i\": \"island\", \"v\": \"village\"}",
        "\"palette\": {\"i\": \"island\", \"v\": \"village\", \"w\": \"island\"}",
    );
    assert!(has(&problems(copy.path()), "ref.duplicate", CHUNK));
}

#[test]
fn portals_resolve_anchors_of_planes_read_later() {
    // The overworld's chunks are read before the vault's, so this anchor
    // resolves only because every plane's anchors are read before any chunk.
    let copy = workspace();
    let root = copy.path();
    fs::write(
        root.join("planes/vault/anchors.jsonc"),
        "{\"format\": \"tilewright.anchors/1\", \"anchors\": [{\"id\": \"vault.door\", \"layer\": 0, \"at\": [1, 1]}]}\n",
    )
    .unwrap();
    edit(
        root,
        CHUNK,
        "\"portal\": {\"plane\": \"vault\", \"layer\": 0, \"at\": [1, 1]}",
        "\"portal\": {\"plane\": \"vault\", \"anchor\": \"vault.door\"}",
    );
    let world = load(root).expect("the anchor resolves");
    assert!(world.planes()["vault"].anchors.contains_key("vault.door"));
    edit(root, CHUNK, "\"vault.door\"", "\"vault.window\"");
    assert_eq!(
        problems(root)
            .iter()
            .filter(|(code, path, _)| *code == "ref.unresolved" && path == CHUNK)
            .count(),
        1
    );
}

/// The `(code, path)` of every diagnostic, for trees expected to fail.
fn codes(root: &Path) -> Vec<(&'static str, String)> {
    problems(root)
        .into_iter()
        .map(|(code, path, _)| (code, path))
        .collect()
}

#[test]
fn layer_height_has_its_documented_range() {
    const PROFILE: &str = "profile/profile.jsonc";
    for (value, expected) in [
        ("1", None),
        ("65535", None),
        ("0", Some("shape.range")),
        ("65536", Some("shape.range")),
        ("-50", Some("shape.range")),
        ("\"50\"", Some("shape.type")),
        ("2.5", Some("shape.type")),
    ] {
        let copy = workspace();
        edit(
            copy.path(),
            PROFILE,
            "\"layer_height\": 50",
            &format!("\"layer_height\": {value}"),
        );
        match expected {
            None => {
                let world = load(copy.path()).expect("in range");
                assert_eq!(
                    world.profile().layer_height.map(i64::from),
                    value.parse().ok()
                );
            }
            Some(code) => assert_eq!(codes(copy.path()), [(code, PROFILE.to_owned())], "{value}"),
        }
    }
    let copy = workspace();
    edit(copy.path(), PROFILE, "\"layer_height\": 50,", "");
    assert_eq!(load(copy.path()).unwrap().profile().layer_height, None);
}

const STAIRWELLS: &str = "planes/vault/stairwells.jsonc";

/// A copy of the fixture whose vault has these stairwell entries.
fn with_stairwells(entries: &str) -> tempfile::TempDir {
    let copy = workspace();
    fs::write(
        copy.path().join(STAIRWELLS),
        format!("{{\"format\": \"tilewright.stairwells/1\", \"stairwells\": [\n{entries}\n]}}\n"),
    )
    .unwrap();
    copy
}

/// A stairwell entry with steps at `(layer, x, y)`.
fn stairwell(id: &str, steps: &[(i64, i64, i64)]) -> String {
    let steps: Vec<String> = steps
        .iter()
        .map(|(layer, x, y)| {
            format!("{{\"layer\": {layer}, \"at\": [{x}, {y}], \"proto\": \"stairs_up\"}}")
        })
        .collect();
    format!("{{\"id\": \"{id}\", \"steps\": [{}]}}", steps.join(", "))
}

#[test]
fn stairwell_steps_connect_adjacent_layers_in_one_direction() {
    const MAX: i64 = tilewright_jsonc::MAX_SAFE_INTEGER;
    const TOP: i64 = i32::MAX as i64;
    for steps in [
        vec![(0, 1, 1), (1, 1, 1), (2, 1, 1)],
        vec![(0, 1, 1), (-1, 2, 0), (-2, 3, -1)],
        vec![(TOP - 1, MAX, -MAX), (TOP, MAX - 1, -MAX + 1)],
    ] {
        let copy = with_stairwells(&stairwell("s", &steps));
        let world = load(copy.path()).unwrap_or_else(|found| panic!("{steps:?}: {found:?}"));
        assert_eq!(
            world.planes()["vault"].stairwells["s"].steps.len(),
            steps.len()
        );
    }
    for (case, steps) in [
        ("a skipped layer", vec![(0, 1, 1), (2, 1, 1)]),
        ("the same layer", vec![(0, 1, 1), (0, 2, 1)]),
        ("a reversal", vec![(0, 1, 1), (1, 1, 1), (0, 2, 2)]),
        ("two tiles on x", vec![(0, 1, 1), (1, 3, 1)]),
        ("two tiles on y", vec![(0, 1, 1), (-1, 1, -1)]),
    ] {
        let copy = with_stairwells(&stairwell("s", &steps));
        assert_eq!(
            codes(copy.path()),
            [("stairwell.step", STAIRWELLS.to_owned())],
            "{case}"
        );
    }
}

#[test]
fn stairwell_tiles_hold_one_step() {
    let copy = with_stairwells(&format!(
        "{},\n{}",
        stairwell("a", &[(0, 1, 1), (1, 1, 1)]),
        stairwell("b", &[(2, 2, 2), (1, 1, 1), (0, 0, 0)])
    ));
    let found = problems(copy.path());
    assert_eq!(
        found,
        [("stairwell.overlap", STAIRWELLS.to_owned(), 3)],
        "{found:?}"
    );
    assert_eq!(
        load(copy.path()).unwrap_err()[0].message,
        "layer 1 tile [1, 1] already holds a step of stairwell \"a\""
    );
}

#[test]
fn stairwell_entries_have_their_documented_shape() {
    const STEPS: &str = "[{\"layer\": 0, \"at\": [1, 1], \"proto\": \"stairs_up\"}, {\"layer\": 1, \"at\": [1, 1], \"proto\": \"stairs_down\"}]";
    let step = |extra: &str| {
        format!(
            "{{\"id\": \"s\", \"steps\": [{{\"layer\": 0, \"at\": [1, 1], \"proto\": \"stairs_up\"}}, {{\"layer\": 1, {extra}}}]}}"
        )
    };
    for (entry, code) in [
        (
            "{\"id\": \"s\", \"steps\": [{\"layer\": 0, \"at\": [1, 1], \"proto\": \"stairs_up\"}]}"
                .to_owned(),
            "shape.range",
        ),
        ("{\"id\": \"s\"}".to_owned(), "shape.missing_key"),
        ("{\"id\": \"s\", \"steps\": {}}".to_owned(), "shape.type"),
        (format!("{{\"id\": \"S\", \"steps\": {STEPS}}}"), "shape.identifier"),
        (
            format!("{{\"id\": \"s\", \"steps\": {STEPS}, \"color\": 1}}"),
            "shape.unknown_key",
        ),
        (
            format!("{{\"id\": \"s\", \"steps\": {STEPS}, \"direction\": \"sideways\"}}"),
            "shape.range",
        ),
        (
            format!("{{\"id\": \"s\", \"steps\": {STEPS}, \"direction\": 1}}"),
            "shape.type",
        ),
        (
            format!("{{\"id\": \"s\", \"steps\": {STEPS}, \"hidden_on_other_layers\": \"no\"}}"),
            "shape.type",
        ),
        (
            format!("{{\"id\": \"s\", \"steps\": {STEPS}, \"custom\": {{\"notes\": 1}}}}"),
            "shape.identifier",
        ),
        (
            format!("{{\"id\": \"s\", \"steps\": {STEPS}}},\n{{\"id\": \"s\", \"steps\": [{{\"layer\": 5, \"at\": [1, 1], \"proto\": \"stairs_up\"}}, {{\"layer\": 6, \"at\": [1, 1], \"proto\": \"stairs_up\"}}]}}"),
            "ref.duplicate",
        ),
        (step("\"at\": [1, 1], \"proto\": \"ladder\""), "ref.unresolved"),
        (step("\"at\": [1, 1], \"proto\": \"Ladder\""), "shape.identifier"),
        (step("\"at\": [1, 1]"), "shape.missing_key"),
        (
            step("\"at\": [1, 1], \"proto\": \"stairs_up\", \"z\": 4"),
            "shape.unknown_key",
        ),
        // Positions past the integer range do not even parse.
        (
            step("\"at\": [9007199254740992, 1], \"proto\": \"stairs_up\""),
            "jsonc.integer_out_of_range",
        ),
        (step("\"at\": [1], \"proto\": \"stairs_up\""), "shape.type"),
        (
            "{\"id\": \"s\", \"steps\": [{\"layer\": 2147483648, \"at\": [1, 1], \"proto\": \"stairs_up\"}, {\"layer\": 2147483647, \"at\": [1, 1], \"proto\": \"stairs_up\"}]}"
                .to_owned(),
            "shape.range",
        ),
    ] {
        let copy = with_stairwells(&entry);
        assert_eq!(
            codes(copy.path()),
            [(code, STAIRWELLS.to_owned())],
            "{entry}"
        );
    }

    // A lone step is still checked.
    let copy = with_stairwells(
        "{\"id\": \"s\", \"steps\": [{\"layer\": 0, \"at\": [1, 1], \"proto\": \"ladder\"}]}",
    );
    assert_eq!(
        codes(copy.path()),
        [
            ("shape.range", STAIRWELLS.to_owned()),
            ("ref.unresolved", STAIRWELLS.to_owned())
        ]
    );

    let copy = with_stairwells("");
    edit(
        copy.path(),
        STAIRWELLS,
        "tilewright.stairwells/1",
        "tilewright.stairs/1",
    );
    assert_eq!(
        codes(copy.path()),
        [("shape.format", STAIRWELLS.to_owned())]
    );

    let copy = workspace();
    fs::create_dir(copy.path().join(STAIRWELLS)).unwrap();
    assert!(has(
        &problems(copy.path()),
        "source.unexpected_file",
        STAIRWELLS
    ));
}

const LOOT: &str = "defs/loot/chest_common.jsonc";

/// A copy of the fixture whose loot table has these fields.
fn with_loot(fields: &str) -> tempfile::TempDir {
    let copy = workspace();
    fs::write(
        copy.path().join(LOOT),
        format!("{{\"format\": \"tilewright.def/1\", \"type\": \"loot\", \"id\": \"chest_common\", \"fields\": {fields}}}\n"),
    )
    .unwrap();
    copy
}

#[test]
fn list_and_object_values_are_checked_at_every_level() {
    let many = vec!["{\"item\": \"coin\", \"weight\": 1}"; 65].join(", ");
    for (fields, code) in [
        ("{\"entries\": []}".to_owned(), "shape.range"),
        (format!("{{\"entries\": [{many}]}}"), "shape.range"),
        ("{\"entries\": {}}".to_owned(), "shape.type"),
        ("{\"entries\": [3]}".to_owned(), "shape.type"),
        (
            "{\"entries\": [{\"item\": \"coin\", \"weight\": 1, \"rarity\": 2}]}".to_owned(),
            "shape.unknown_key",
        ),
        (
            "{\"entries\": [{\"item\": \"coin\"}]}".to_owned(),
            "shape.missing_key",
        ),
        (
            "{\"entries\": [{\"item\": \"coin\", \"weight\": 0}]}".to_owned(),
            "shape.range",
        ),
        (
            "{\"entries\": [{\"item\": \"Coin\", \"weight\": 1}]}".to_owned(),
            "shape.identifier",
        ),
        (
            "{\"entries\": [{\"item\": \"gem\", \"weight\": 1}]}".to_owned(),
            "ref.unresolved",
        ),
        (
            "{\"entries\": [{\"item\": \"coin\", \"weight\": 1, \"count\": {\"min\": 1}}]}"
                .to_owned(),
            "shape.missing_key",
        ),
        (
            "{\"entries\": [{\"item\": \"coin\", \"weight\": 1, \"count\": [1, 2]}]}".to_owned(),
            "shape.type",
        ),
        ("{\"chance\": 0.5}".to_owned(), "shape.missing_key"),
    ] {
        let copy = with_loot(&fields);
        assert_eq!(codes(copy.path()), [(code, LOOT.to_owned())], "{fields}");
    }

    // Every problem in a list is reported, not just the first.
    let copy = with_loot(
        "{\"entries\": [{\"item\": \"gem\", \"weight\": 1}, {\"item\": \"coin\", \"weight\": 0}, 3, {}]}",
    );
    let found = load(copy.path()).expect_err("four bad entries");
    let found: Vec<(&str, &str)> = found
        .iter()
        .map(|diagnostic| (diagnostic.code, diagnostic.message.as_str()))
        .collect();
    assert_eq!(
        found,
        [
            ("ref.unresolved", "no proto definition \"gem\""),
            ("shape.range", "0 is outside 1..=65535"),
            ("shape.type", "expected an object, found integer"),
            (
                "shape.missing_key",
                "required fields \"item\", \"weight\" are missing"
            ),
        ]
    );

    // Items are checked even when there are too many of them.
    let copy = with_loot(&format!(
        "{{\"entries\": [{many}, {{\"item\": \"gem\", \"weight\": 1}}, {{\"item\": \"opal\", \"weight\": 1}}]}}"
    ));
    let found: Vec<_> = load(copy.path())
        .expect_err("too many entries, two unresolved")
        .into_iter()
        .map(|diagnostic| (diagnostic.code, diagnostic.message))
        .collect();
    assert_eq!(
        found,
        [
            ("shape.range", "67 items, outside 1..=64".to_owned()),
            ("ref.unresolved", "no proto definition \"gem\"".to_owned()),
            ("ref.unresolved", "no proto definition \"opal\"".to_owned()),
        ]
    );

    // Object fields of chunk objects are checked the same way.
    for (tags, code) in [
        (["\"a\""; 9].join(", "), "shape.range"),
        (format!("\"{}\"", "a".repeat(33)), "shape.range"),
        ("1".to_owned(), "shape.type"),
    ] {
        let copy = workspace();
        edit(
            copy.path(),
            CHUNK,
            "\"tags\": [\"ruin\", \"north\"]",
            &format!("\"tags\": [{tags}]"),
        );
        assert_eq!(codes(copy.path()), [(code, CHUNK.to_owned())], "{tags}");
    }
}

#[test]
fn nested_objects_name_their_missing_required_fields() {
    let copy = workspace();
    let required: Vec<String> = ["a", "b", "c", "d", "e"]
        .iter()
        .map(|name| format!("\"{name}\": {{\"type\": \"boolean\", \"required\": true}}"))
        .collect();
    edit(
        copy.path(),
        "profile/schema/object-fields.jsonc",
        "\"fields\": {",
        &format!(
            "\"fields\": {{\"flags\": {{\"type\": \"object\", \"fields\": {{{}}}}}, ",
            required.join(", ")
        ),
    );
    edit(
        copy.path(),
        CHUNK,
        "\"fields\": {\"name\": \"Old wall\"",
        "\"fields\": {\"flags\": {\"c\": true}, \"name\": \"Old wall\"",
    );
    let found = load(copy.path()).expect_err("four flags are missing");
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(
        found[0].message,
        "required fields \"a\", \"b\", \"d\" and 1 more are missing"
    );
}

/// A field spec of `levels` nested list and object types around a boolean,
/// and a value of it.
fn nested(levels: usize) -> (String, String) {
    let mut spec = "{\"type\": \"boolean\"}".to_owned();
    let mut value = "true".to_owned();
    for level in 0..levels {
        if level % 2 == 0 {
            spec = format!("{{\"type\": \"list\", \"items\": {spec}}}");
            value = format!("[{value}]");
        } else {
            spec = format!("{{\"type\": \"object\", \"fields\": {{\"inner\": {spec}}}}}");
            value = format!("{{\"inner\": {value}}}");
        }
    }
    (spec, value)
}

#[test]
fn list_and_object_types_nest_at_most_8_deep() {
    const FIELDS: &str = "profile/schema/object-fields.jsonc";
    assert_eq!(super::MAXIMUM_FIELD_DEPTH, 8);
    for levels in [7, 8] {
        let copy = workspace();
        let (spec, value) = nested(levels);
        edit(
            copy.path(),
            FIELDS,
            "\"fields\": {",
            &format!("\"fields\": {{\"deep\": {spec}, "),
        );
        edit(
            copy.path(),
            CHUNK,
            "\"fields\": {\"name\": \"Old wall\"",
            &format!("\"fields\": {{\"deep\": {value}, \"name\": \"Old wall\""),
        );
        let world = load(copy.path()).unwrap_or_else(|found| panic!("{levels}: {found:?}"));
        let wall = &world.planes()["overworld"].chunks[&(0, 0, 0)].objects[1];
        assert_eq!(
            wall.fields[0],
            ("deep".to_owned(), FieldValue::Json(value.replace(' ', "")))
        );
    }
    for levels in [9, 12] {
        let copy = workspace();
        edit(
            copy.path(),
            FIELDS,
            "\"fields\": {",
            &format!("\"fields\": {{\"deep\": {}, ", nested(levels).0),
        );
        assert_eq!(
            codes(copy.path()),
            [("shape.range", FIELDS.to_owned())],
            "{levels}"
        );
    }
}

#[test]
fn list_and_object_specs_are_checked() {
    const FIELDS: &str = "profile/schema/object-fields.jsonc";
    assert_eq!(
        super::MAXIMUM_LIST_ITEMS,
        tilewright_jsonc::Limits::default().max_array_items
    );
    for (spec, code) in [
        ("{\"type\": \"list\"}", "shape.missing_key"),
        ("{\"type\": \"list\", \"items\": 3}", "shape.type"),
        (
            "{\"type\": \"list\", \"items\": {\"type\": \"boolean\", \"required\": true}}",
            "shape.unknown_key",
        ),
        (
            "{\"type\": \"list\", \"items\": {\"type\": \"boolean\"}, \"values\": []}",
            "shape.unknown_key",
        ),
        (
            "{\"type\": \"list\", \"items\": {\"type\": \"boolean\"}, \"min_items\": 3, \"max_items\": 2}",
            "shape.range",
        ),
        (
            "{\"type\": \"list\", \"items\": {\"type\": \"boolean\"}, \"min_items\": -1}",
            "shape.range",
        ),
        (
            "{\"type\": \"list\", \"items\": {\"type\": \"boolean\"}, \"max_items\": 250001}",
            "shape.range",
        ),
        (
            "{\"type\": \"list\", \"items\": {\"type\": \"set\"}}",
            "shape.range",
        ),
        ("{\"type\": \"object\"}", "shape.missing_key"),
        ("{\"type\": \"object\", \"fields\": []}", "shape.type"),
        (
            "{\"type\": \"object\", \"fields\": {\"Bad\": {\"type\": \"boolean\"}}}",
            "shape.identifier",
        ),
        (
            "{\"type\": \"object\", \"fields\": {\"a\": {\"type\": \"boolean\", \"required\": 1}}}",
            "shape.type",
        ),
        (
            "{\"type\": \"object\", \"fields\": {\"a\": {\"type\": \"boolean\"}}, \"items\": {}}",
            "shape.unknown_key",
        ),
        (
            "{\"type\": \"list\", \"items\": {\"type\": \"object\", \"fields\": {\"a\": {\"type\": \"reference\", \"target\": \"spell\"}}}}",
            "ref.unresolved",
        ),
    ] {
        let copy = workspace();
        edit(
            copy.path(),
            FIELDS,
            "\"fields\": {",
            &format!("\"fields\": {{\"extra\": {spec}, "),
        );
        assert_eq!(codes(copy.path()), [(code, FIELDS.to_owned())], "{spec}");
    }
    let copy = workspace();
    edit(
        copy.path(),
        FIELDS,
        "\"fields\": {",
        "\"fields\": {\"extra\": {\"type\": \"list\", \"items\": {\"type\": \"object\", \"fields\": {\"a\": {\"type\": \"reference\", \"target\": \"spell\"}}}}, ",
    );
    let found = load(copy.path()).expect_err("unknown target");
    assert_eq!(
        found[0].message,
        "field \"extra[].a\" references unknown definition type \"spell\""
    );
}
