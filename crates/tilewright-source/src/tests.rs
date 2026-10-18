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
