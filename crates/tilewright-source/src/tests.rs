// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

use super::{FieldValue, LoadOptions, PlaneKind, Portal, Projection, load, load_with};
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
    assert_eq!(world.planes.len(), 2);
    assert_eq!(world.definitions.len(), 11);

    let overworld = &world.planes["overworld"];
    assert_eq!(overworld.kind, PlaneKind::Shared);
    assert_eq!(overworld.zones.len(), 3);
    assert_eq!(overworld.zones["village"].parent.as_deref(), Some("island"));
    assert_eq!(overworld.anchors["cave.entrance"].layer, -1);
    assert_eq!(
        overworld.chunks.keys().copied().collect::<Vec<_>>(),
        [(-1, 0, 0), (0, -1, 0), (0, 0, 0)]
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
    let found = tree.take_diagnostics();
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
