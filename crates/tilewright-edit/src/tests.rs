// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

use super::*;
use std::{collections::BTreeMap, fmt::Write as _, fs};

const CHUNK: &str = "planes/overworld/layers/0/0_0.jsonc";
const WEST: &str = "planes/overworld/layers/0/-1_0.jsonc";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/basic")
}

/// Every file under `root`, by relative path.
fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let relative = path.strip_prefix(root).unwrap();
                let relative = relative.to_string_lossy().replace('\\', "/");
                files.insert(relative, fs::read(&path).unwrap());
            }
        }
    }
    files
}

/// Every directory under `root`, by relative path.
fn directories(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                found.push(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
                pending.push(path);
            }
        }
    }
    found.sort();
    found
}

fn copy() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    for (path, bytes) in snapshot(&fixture()) {
        let target = directory.path().join(&path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, bytes).unwrap();
    }
    directory
}

fn read(root: &Path, path: &str) -> String {
    fs::read_to_string(root.join(path)).unwrap()
}

fn transaction(operations: &str) -> Transaction {
    let text = format!(
        "{{\n  \"format\": \"tilewright.transaction/1\",\n  \"operations\": [\n    {operations}\n  ]\n}}\n"
    );
    match Transaction::parse("tx.jsonc", text) {
        Ok(transaction) => transaction,
        Err(problems) => panic!("{problems:#?}"),
    }
}

fn planned(root: &Path, transaction: &Transaction) -> Plan {
    match plan(root, transaction, &Options::default()) {
        Ok(plan) => plan,
        Err(problems) => panic!("{problems:#?}"),
    }
}

/// Plans and publishes, returning the plan.
fn apply(root: &Path, transaction: &Transaction) -> Plan {
    let plan = planned(root, transaction);
    if let Err(problems) = plan.publish() {
        panic!("{problems:#?}");
    }
    plan
}

fn problems(root: &Path, transaction: &Transaction) -> Vec<Diagnostic> {
    match plan(root, transaction, &Options::default()) {
        Ok(_) => panic!("the transaction should fail"),
        Err(problems) => problems,
    }
}

fn has(problems: &[Diagnostic], code: &str, text: &str) -> bool {
    problems
        .iter()
        .any(|problem| problem.code == code && problem.message.contains(text))
}

/// Applies `transaction`, then its inverse, and checks that every file and
/// directory is as it was. Returns the files after the transaction.
fn round_trip(root: &Path, transaction: &Transaction) -> BTreeMap<String, Vec<u8>> {
    let before = snapshot(root);
    let folders = directories(root);
    let plan = apply(root, transaction);
    let after = snapshot(root);
    for file in plan.files() {
        assert_eq!(
            after.get(&file.path).map(Vec::as_slice),
            file.after.as_deref().map(str::as_bytes),
            "{}",
            file.path
        );
    }
    let inverse = Transaction::parse("inverse.jsonc", plan.inverse()).unwrap();
    apply(root, &inverse);
    assert_eq!(snapshot(root), before);
    assert_eq!(directories(root), folders);
    after
}

#[test]
fn objects_are_inserted_removed_moved_and_edited_losslessly() {
    let tree = copy();
    let root = tree.path();
    let original = read(root, CHUNK);
    let tx = transaction(
        r#"{"op": "object.insert", "plane": "overworld", "layer": 0, "at": [7, 0], "index": 0, "object": {"proto": "wall"}},
    {"op": "object.remove", "plane": "overworld", "layer": 0, "at": [12, 12], "index": 0, "expect": {"proto": "tree", "z": 4}},
    {"op": "object.move", "plane": "overworld", "layer": 0, "at": [9, 4], "index": 0, "expect": {"proto": "stairs_down"}, "to": {"at": [10, 4]}},
    {"op": "set", "object": {"plane": "overworld", "layer": 0, "at": [15, 0], "index": 0}, "pointer": ["fields", "name"], "value": "New wall", "expect": "Old wall"},
    {"op": "insert", "object": {"plane": "overworld", "layer": 0, "at": [2, 2], "index": 0}, "pointer": ["inventory", 1], "value": {"proto": "coin"}},
    {"op": "object.move", "plane": "overworld", "layer": 0, "at": [7, 0], "index": 1, "expect": {"proto": "tree"}, "to": {"index": 0}}"#,
    );
    let after = round_trip(root, &tx);
    let edited = String::from_utf8(after[CHUNK].clone()).unwrap();
    let head = original.split("  \"objects\": [").next().unwrap();
    assert!(
        edited.starts_with(head),
        "everything before the objects is kept"
    );
    let objects = r#"  "objects": [
    {"at": [7, 0], "proto": "tree"},
    {"at": [7, 0], "proto": "wall"},
    {"at": [15, 0], "proto": "wall", "layer": "wall", "fields": {"name": "New wall", "tags": ["ruin", "north"]}},
    {"at": [2, 2], "proto": "chest", "fields": {"locked": true, "loot": "chest_common"},
     "inventory": [
       {"proto": "coin", "fields": {"count": 3}},
       {"proto": "coin"},
       {"proto": "chest", "inventory": [{"proto": "coin", "fields": {"count": 1}}]}
     ]},
    {"at": [3, 3], "proto": "stairs_down", "portal": {"plane": "vault", "layer": 0, "at": [1, 1]},
     "custom": {"example.org/notes": {"reviewed": true}}},
    {"at": [10, 4], "proto": "stairs_down", "portal": {"anchor": "cave.entrance"}}
  ]
}
"#;
    assert_eq!(&edited[head.len()..], objects);
}

#[test]
fn the_planned_world_is_validated_and_nothing_is_written() {
    let tree = copy();
    let root = tree.path();
    let before = snapshot(root);
    let tx = transaction(
        r#"{"op": "object.insert", "plane": "overworld", "layer": 0, "at": [1, 1], "object": {"proto": "tree", "z": 3}}"#,
    );
    let plan = planned(root, &tx);
    assert_eq!(snapshot(root), before, "planning writes nothing");
    let chunk = &plan.world().planes()["overworld"].chunks[&(0, 0, 0)];
    assert!(
        chunk
            .objects
            .iter()
            .any(|object| object.at == Some([1, 1]) && object.z == 3)
    );
    assert_eq!(plan.files().len(), 1);
    assert!(plan.files()[0].is_changed());

    // A result the loader rejects is returned as its diagnostics.
    let bad = transaction(
        r#"{"op": "object.insert", "plane": "overworld", "layer": 0, "at": [1, 1], "object": {"proto": "unicorn"}}"#,
    );
    let found = problems(root, &bad);
    assert!(
        found
            .iter()
            .any(|problem| problem.code == "ref.unresolved" && problem.path == CHUNK),
        "{found:#?}"
    );
    assert_eq!(snapshot(root), before);
}

#[test]
fn chunks_appear_and_disappear_with_their_content() {
    let tree = copy();
    let root = tree.path();
    let folders = directories(root);
    let placed = transaction(
        r#"{"op": "object.insert", "plane": "overworld", "layer": 7, "at": [100, -40], "object": {"proto": "tree"}},
    {"op": "terrain.set", "plane": "overworld", "layer": 7, "grid": "floor", "cells": [{"at": [-5, 3], "value": "grass"}]}"#,
    );
    apply(root, &placed);
    assert_eq!(
        read(root, "planes/overworld/layers/7/3_-2.jsonc"),
        "{\n  \"format\": \"tilewright.chunk/1\",\n  \"plane\": \"overworld\",\n  \"layer\": 7,\n  \"chunk\": [3, -2],\n  \"objects\": [{\"at\": [4, 24], \"proto\": \"tree\"}]\n}\n"
    );
    let mut rows = vec![".".repeat(32); 32];
    rows[3].replace_range(27..28, "g");
    let rows: Vec<String> = rows
        .iter()
        .map(|row| format!("        \"{row}\""))
        .collect();
    assert_eq!(
        read(root, "planes/overworld/layers/7/-1_0.jsonc"),
        format!(
            "{{\n  \"format\": \"tilewright.chunk/1\",\n  \"plane\": \"overworld\",\n  \"layer\": 7,\n  \"chunk\": [-1, 0],\n  \"terrain\": {{\n    \"floor\": {{\n      \"palette\": {{\"g\": \"grass\"}},\n      \"rows\": [\n{}\n      ]\n    }}\n  }}\n}}\n",
            rows.join(",\n")
        )
    );
    // Removing the content removes the files, and the layer directory with
    // them.
    let cleared = transaction(
        r#"{"op": "object.remove", "plane": "overworld", "layer": 7, "at": [100, -40], "index": 0, "expect": {"proto": "tree"}},
    {"op": "terrain.set", "plane": "overworld", "layer": 7, "grid": "floor", "cells": [{"at": [-5, 3], "value": null, "expect": "grass"}]}"#,
    );
    apply(root, &cleared);
    assert!(!root.join("planes/overworld/layers/7").exists());
    assert_eq!(directories(root), folders);
    // Setting empty cells where there is no chunk creates nothing.
    let nothing = transaction(
        r#"{"op": "terrain.set", "plane": "overworld", "layer": 9, "grid": "height", "cells": [{"at": [0, 0], "value": 0, "expect": 0}]}"#,
    );
    let plan = planned(root, &nothing);
    assert!(plan.files().iter().all(|file| !file.is_changed()));
}

/// The cells of a whole chunk, for terrain operations.
fn whole_chunk(cx: i64, value: &str) -> String {
    let mut cells = String::new();
    for y in 0..32 {
        for x in 0..32 {
            if !cells.is_empty() {
                cells.push_str(", ");
            }
            let _ = write!(
                cells,
                "{{\"at\": [{}, {y}], \"value\": {value}}}",
                cx * 32 + x
            );
        }
    }
    cells
}

#[test]
fn terrain_cells_are_replaced_in_place() {
    let tree = copy();
    let root = tree.path();
    let original = read(root, CHUNK);
    let tx = transaction(
        r#"{"op": "terrain.set", "plane": "overworld", "layer": 0, "grid": "floor", "cells": [
      {"at": [1, 0], "value": "sand", "expect": "grass"},
      {"at": [2, 1], "value": "cave_floor", "expect": "grass"}]},
    {"op": "terrain.set", "plane": "overworld", "layer": 0, "grid": "height", "cells": [{"at": [12, 12], "value": 40, "expect": 32}]},
    {"op": "terrain.set", "plane": "overworld", "layer": 0, "grid": "zone", "cells": [{"at": [0, 31], "value": "wilderness", "expect": "island"}]}"#,
    );
    let after = round_trip(root, &tx);
    let expected = original
        .replacen(
            "\"sggggggsggggggsggggggsggggggsggg\",\n        \"ggggggsgg",
            "\"ssgggggsggggggsggggggsggggggsggg\",\n        \"ggcgggsgg",
            1,
        )
        .replacen(
            "\"palette\": {\"g\": \"grass\", \"s\": \"sand\"}",
            "\"palette\": {\"g\": \"grass\", \"s\": \"sand\", \"c\": \"cave_floor\"}",
            1,
        )
        .replacen("16,20,24,28,32,28,24", "16,20,24,28,40,28,24", 1)
        .replacen(
            "\"palette\": {\"i\": \"island\", \"v\": \"village\"}",
            "\"palette\": {\"i\": \"island\", \"v\": \"village\", \"w\": \"wilderness\"}",
            1,
        );
    let expected = {
        // The last zone row gains its first cell.
        let last = "        \"iiiiiiiiiiiiiiiiiiiiiiiiiiiiiiii\"\n      ]\n    }\n  },";
        let at = expected.rfind(last).unwrap();
        let mut text = expected.clone();
        text.replace_range(
            at..at + last.len(),
            "        \"wiiiiiiiiiiiiiiiiiiiiiiiiiiiiiii\"\n      ]\n    }\n  },",
        );
        text
    };
    assert_eq!(String::from_utf8(after[CHUNK].clone()).unwrap(), expected);
}

#[test]
fn grids_change_form_and_drop_unused_entries() {
    let tree = copy();
    let root = tree.path();
    // A fill grid filled again stays a fill and drops the unused key; a
    // height fill with one different cell becomes rows.
    let tx = transaction(&format!(
        r#"{{"op": "terrain.set", "plane": "overworld", "layer": 0, "grid": "floor", "cells": [{}]}},
    {{"op": "terrain.set", "plane": "overworld", "layer": 0, "grid": "height", "cells": [{{"at": [-32, 0], "value": 5}}]}},
    {{"op": "terrain.set", "plane": "overworld", "layer": 0, "grid": "mask", "cells": [{{"at": [-31, 1], "value": "edge_n"}}]}}"#,
        whole_chunk(-1, "\"sand\"")
    ));
    let after = round_trip(root, &tx);
    let west = String::from_utf8(after[WEST].clone()).unwrap();
    assert!(
        west.contains("\"floor\": {\"palette\": {\"s\": \"sand\"}, \"fill\": \"s\"}"),
        "{west}"
    );
    assert!(
        west.contains("\"mask\": {\n"),
        "a new grid goes before height: {west}"
    );
    assert!(west.find("\"mask\"") < west.find("\"height\""));
    assert!(
        west.contains("\"height\": {\"rows\": [\n      [5, 0, 0,"),
        "{west}"
    );

    // Clearing a grid removes it; heights back to 0 remove the height grid.
    let mut cells = String::new();
    for x in 10..15 {
        let _ = write!(
            cells,
            "{}{{\"at\": [{x}, 5], \"value\": null, \"expect\": \"edge_n\"}}",
            if cells.is_empty() { "" } else { ", " }
        );
    }
    let clear = transaction(&format!(
        r#"{{"op": "terrain.set", "plane": "overworld", "layer": 0, "grid": "mask", "cells": [{cells}]}},
    {{"op": "terrain.set", "plane": "overworld", "layer": 0, "grid": "height", "cells": [{}]}}"#,
        whole_chunk(-1, "0")
    ));
    let after = round_trip(root, &clear);
    let chunk = String::from_utf8(after[CHUNK].clone()).unwrap();
    assert!(!chunk.contains("\"mask\""));
    assert!(
        chunk.contains("\"floor\": {\n      \"palette\": {\"g\": \"grass\", \"s\": \"sand\"},")
    );
    let west = String::from_utf8(after[WEST].clone()).unwrap();
    assert!(!west.contains("\"height\""), "{west}");
}

#[test]
fn a_full_palette_is_widened() {
    let tree = copy();
    let root = tree.path();
    // Ninety-one prototypes fill every one-character key.
    let mut operations = String::new();
    let mut cells = String::new();
    for index in 0..92 {
        let _ = write!(
            operations,
            "{{\"op\": \"document.create\", \"path\": \"defs/proto/p{index}.jsonc\", \"document\": {{\"format\": \"tilewright.def/1\", \"type\": \"proto\", \"id\": \"p{index}\", \"fields\": {{\"face\": \"p.png\", \"layer\": \"floor\"}}}}}},\n    "
        );
        let _ = write!(
            cells,
            "{}{{\"at\": [{}, {}], \"value\": \"p{index}\"}}",
            if cells.is_empty() { "" } else { ", " },
            index % 32,
            index / 32 + 10
        );
    }
    let tx = transaction(&format!(
        "{operations}{{\"op\": \"terrain.set\", \"plane\": \"overworld\", \"layer\": 0, \"grid\": \"floor\", \"cells\": [{cells}]}}"
    ));
    let after = round_trip(root, &tx);
    let chunk = String::from_utf8(after[CHUNK].clone()).unwrap();
    assert!(chunk.contains("\"cell_width\": 2"), "{chunk}");
}

#[test]
fn documents_are_edited_by_path_and_identity() {
    let tree = copy();
    let root = tree.path();
    let tx = transaction(
        r#"{"op": "set", "path": "planes/overworld/zones.jsonc", "pointer": ["zones", {"id": "island"}, "name"], "value": "Green Isle", "expect": "Green Island"},
    {"op": "set", "path": "planes/overworld/zones.jsonc", "pointer": ["zones", {"id": "wilderness"}, "properties"], "value": {"music": "wind"}},
    {"op": "remove", "path": "planes/overworld/zones.jsonc", "pointer": ["zones", {"id": "village"}, "properties"], "expect": {"safe": true}},
    {"op": "insert", "path": "planes/overworld/anchors.jsonc", "pointer": ["anchors", 2], "value": {"id": "tower.top", "layer": 1, "at": [20, 20]}},
    {"op": "test", "path": "planes/overworld/anchors.jsonc", "pointer": ["anchors", {"id": "village.well"}, "layer"], "value": 0},
    {"op": "test", "path": "planes/overworld/anchors.jsonc", "pointer": ["anchors", 3], "absent": true},
    {"op": "document.create", "path": "defs/proto/rock.jsonc", "document": {"format": "tilewright.def/1", "type": "proto", "id": "rock", "fields": {"face": "rock.png", "layer": "item", "blocks_view": true}}},
    {"op": "document.rename", "path": "defs/proto/edge_n.jsonc", "to": "defs/proto/edge_north.jsonc"},
    {"op": "set", "path": "defs/proto/edge_north.jsonc", "pointer": ["id"], "value": "edge_north"},
    {"op": "set", "path": "planes/overworld/layers/0/0_0.jsonc", "pointer": ["terrain", "mask", "palette", "n"], "value": "edge_north", "expect": "edge_n"},
    {"op": "document.delete", "path": "defs/recipe/chest.jsonc"}"#,
    );
    let after = round_trip(root, &tx);
    let text = |path: &str| String::from_utf8(after[path].clone()).unwrap();
    assert_eq!(
        text("planes/overworld/zones.jsonc"),
        "{\n  \"format\": \"tilewright.zones/1\",\n  \"zones\": [\n    {\"id\": \"wilderness\", \"name\": \"Wilderness\", \"properties\": {\"music\": \"wind\"}},\n    {\"id\": \"island\", \"parent\": \"wilderness\", \"name\": \"Green Isle\", \"properties\": {\"music\": \"calm\"}},\n    {\"id\": \"village\", \"parent\": \"island\", \"name\": \"Little Village\"}\n  ]\n}\n"
    );
    assert_eq!(
        text("planes/overworld/anchors.jsonc"),
        "{\n  \"format\": \"tilewright.anchors/1\",\n  \"anchors\": [\n    {\"id\": \"village.well\", \"layer\": 0, \"at\": [10, 12]},\n    {\"id\": \"cave.entrance\", \"layer\": -1, \"at\": [4, 4]},\n    {\"id\": \"tower.top\", \"layer\": 1, \"at\": [20, 20]}\n  ]\n}\n"
    );
    assert_eq!(
        text("defs/proto/rock.jsonc"),
        "{\n  \"format\": \"tilewright.def/1\",\n  \"type\": \"proto\",\n  \"id\": \"rock\",\n  \"fields\": {\"face\": \"rock.png\", \"layer\": \"item\", \"blocks_view\": true}\n}\n"
    );
    assert_eq!(
        text("defs/proto/edge_north.jsonc"),
        "{\"format\": \"tilewright.def/1\", \"type\": \"proto\", \"id\": \"edge_north\", \"fields\": {\"face\": \"edge_n.png\", \"layer\": \"mask\"}}\n"
    );
    assert!(!after.contains_key("defs/proto/edge_n.jsonc"));
    assert!(!after.contains_key("defs/recipe/chest.jsonc"));
}

#[test]
fn comments_and_untouched_bytes_are_kept() {
    let tree = copy();
    let root = tree.path();
    let stairwells = "planes/overworld/stairwells.jsonc";
    let tx = transaction(
        r#"{"op": "remove", "path": "planes/overworld/stairwells.jsonc", "pointer": ["stairwells", {"id": "cliff.path"}]},
    {"op": "set", "path": "planes/overworld/stairwells.jsonc", "pointer": ["stairwells", {"id": "cave.chute"}, "direction"], "value": "both"},
    {"op": "terrain.set", "plane": "overworld", "layer": 1, "grid": "floor", "cells": [{"at": [18, 6], "value": null, "expect": "cave_floor"}]}"#,
    );
    let after = round_trip(root, &tx);
    let text = String::from_utf8(after[stairwells].clone()).unwrap();
    let original = read(&fixture(), stairwells);
    let start = original.find("    // Sloped").unwrap();
    let end = original.find("    // One-way").unwrap();
    let expected = format!("{}{}", &original[..start], &original[end..])
        .replace("\"direction\": \"down\"", "\"direction\": \"both\"");
    assert_eq!(text, expected);
    assert!(text.contains("// Straight: the same tile on three layers."));
    let layer = String::from_utf8(after["planes/overworld/layers/1/0_0.jsonc"].clone()).unwrap();
    assert!(layer.contains("// A raised platform; heights are relative to layer 1."));
    assert!(layer.contains("\"...................ccccccccc....\""));
}

#[test]
fn a_changed_base_is_a_conflict_until_rebased() {
    let tree = copy();
    let root = tree.path();
    let tx = transaction(
        r#"{"op": "terrain.set", "plane": "overworld", "layer": 0, "grid": "floor", "cells": [{"at": [0, 0], "value": "grass", "expect": "sand"}]}"#,
    );
    let sealed = Transaction::parse("sealed.jsonc", planned(root, &tx).sealed()).unwrap();
    assert!(sealed.base().unwrap().contains_key(CHUNK));
    assert!(sealed.result().unwrap().contains_key(CHUNK));
    // Applying a sealed transaction checks that it replays exactly.
    planned(root, &sealed);

    // Main moves: another cell of the same chunk changes.
    apply(
        root,
        &transaction(
            r#"{"op": "terrain.set", "plane": "overworld", "layer": 0, "grid": "floor", "cells": [{"at": [5, 5], "value": "sand", "expect": "grass"}]}"#,
        ),
    );
    let found = problems(root, &sealed);
    assert!(
        has(&found, "tx.conflict", "rebase the transaction"),
        "{found:#?}"
    );

    // Replaying its operations on the new base succeeds and records it.
    let rebased = plan(
        root,
        &sealed,
        &Options {
            rebase: true,
            ..Options::default()
        },
    )
    .unwrap();
    let rebased = Transaction::parse("rebased.jsonc", rebased.sealed()).unwrap();
    apply(root, &rebased);
    let chunk = read(root, CHUNK);
    assert!(chunk.contains("\"gggggggsggggggsggggggsggggggsggg\""));
    assert!(chunk.contains("\"ggsggsgggsggggggsggggggsggggggsg\""));

    // A conflicting change fails the cell's own precondition.
    let Err(found) = plan(
        root,
        &sealed,
        &Options {
            rebase: true,
            ..Options::default()
        },
    ) else {
        panic!("the cell changed");
    };
    assert!(
        has(
            &found,
            "tx.conflict",
            "floor cell (0, 0) is \"grass\", expected \"sand\""
        ),
        "{found:#?}"
    );
    let cell = sealed.text().find("{\"at\": [0, 0]").unwrap();
    assert_eq!(found[0].start, cell, "the conflict points at the cell");
}

#[test]
fn a_replay_must_give_the_recorded_result() {
    let tree = copy();
    let root = tree.path();
    let tx = transaction(
        r#"{"op": "set", "path": "planes/overworld/plane.jsonc", "pointer": ["title"], "value": "The Overworld"}"#,
    );
    let sealed = planned(root, &tx).sealed().to_owned();
    let digest = transaction::hex(&super::digest(Some("other")).unwrap());
    let path = "planes/overworld/plane.jsonc";
    // The path's entry in the result.
    let result = sealed.find("\"result\": [").unwrap();
    let entry = result
        + sealed[result..]
            .find(&format!("\"path\": \"{path}\""))
            .unwrap();
    let at = entry + sealed[entry..].find("\"sha256\": \"").unwrap() + 11;
    let mut tampered = sealed.clone();
    tampered.replace_range(at..at + 64, &digest);
    let found = problems(root, &Transaction::parse("t.jsonc", tampered).unwrap());
    assert!(has(&found, "tx.result", path), "{found:#?}");
}

#[test]
fn plans_are_deterministic() {
    let tx = transaction(&format!(
        r#"{{"op": "object.insert", "plane": "overworld", "layer": 0, "at": [40, 40], "object": {{"proto": "tree"}}}},
    {{"op": "terrain.set", "plane": "overworld", "layer": 0, "grid": "floor", "cells": [{}]}},
    {{"op": "document.rename", "path": "defs/recipe/chest.jsonc", "to": "defs/recipe/box.jsonc"}},
    {{"op": "set", "path": "defs/recipe/box.jsonc", "pointer": ["id"], "value": "box"}}"#,
        whole_chunk(-1, "\"sand\"")
    ));
    let first = copy();
    let second = copy();
    let one = planned(first.path(), &tx);
    let two = planned(second.path(), &tx);
    assert_eq!(one.files(), two.files());
    assert_eq!(one.sealed(), two.sealed());
    assert_eq!(one.inverse(), two.inverse());
    assert_eq!(one.world().source_digest(), two.world().source_digest());
    one.publish().unwrap();
    two.publish().unwrap();
    assert_eq!(snapshot(first.path()), snapshot(second.path()));
}

#[test]
fn a_failed_publish_restores_what_it_published() {
    let tree = copy();
    let root = tree.path();
    let before = snapshot(root);
    let folders = directories(root);
    let tx = transaction(
        r#"{"op": "object.insert", "plane": "overworld", "layer": 3, "at": [1, 1], "object": {"proto": "tree"}},
    {"op": "document.delete", "path": "defs/recipe/chest.jsonc"},
    {"op": "set", "path": "planes/overworld/plane.jsonc", "pointer": ["title"], "value": "Changed"}"#,
    );
    let plan = planned(root, &tx);
    for step in 0..3 {
        publish::FAIL_STEP.with(|fail| fail.set(Some(step)));
        let found = plan.publish().unwrap_err().problems;
        publish::FAIL_STEP.with(|fail| fail.set(None));
        assert!(
            found[0]
                .message
                .contains("every file published before it was restored"),
            "{found:#?}"
        );
        assert_eq!(snapshot(root), before, "step {step}");
        assert_eq!(directories(root), folders, "step {step}");
    }
    plan.publish().unwrap();
    assert_ne!(snapshot(root), before);
}

#[test]
fn a_file_changed_during_publish_stops_it() {
    let tree = copy();
    let root = tree.path().to_path_buf();
    let tx = transaction(
        r#"{"op": "set", "path": "planes/overworld/plane.jsonc", "pointer": ["title"], "value": "Changed"},
    {"op": "test", "path": "planes/overworld/zones.jsonc", "pointer": ["format"], "value": "tilewright.zones/1"}"#,
    );
    let plan = planned(&root, &tx);
    assert_eq!(
        plan.files().len(),
        2,
        "consulted files are part of the base"
    );
    let zones = root.join("planes/overworld/zones.jsonc");
    let edited = format!("{}\n", read(&root, "planes/overworld/zones.jsonc"));
    let hook_path = zones.clone();
    let hook_text = edited.clone();
    publish::BEFORE_CHECK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || fs::write(&hook_path, &hook_text).unwrap()));
    });
    let found = plan.publish().unwrap_err().problems;
    assert!(
        has(&found, "tx.conflict", "nothing was written"),
        "{found:#?}"
    );
    assert_eq!(fs::read_to_string(&zones).unwrap(), edited);
    assert_eq!(
        read(&root, "planes/overworld/plane.jsonc"),
        read(&fixture(), "planes/overworld/plane.jsonc")
    );
    assert!(
        !root
            .join("planes/overworld/.plane.jsonc.tilewright-tmp")
            .exists()
    );
}

#[test]
fn another_holder_of_the_lock_stops_a_publish() {
    let tree = copy();
    let root = tree.path();
    let tx = transaction(
        r#"{"op": "set", "path": "planes/overworld/plane.jsonc", "pointer": ["title"], "value": "Changed"}"#,
    );
    let plan = planned(root, &tx);
    let source = SourceRoot::new(root);
    let lock = source.lock().unwrap();
    if cfg!(unix) {
        let found = plan.publish().unwrap_err().problems;
        assert!(has(&found, "tx.conflict", "lock"), "{found:#?}");
    }
    drop(lock);
    plan.publish().unwrap();
}

#[cfg(unix)]
#[test]
fn links_are_never_followed() {
    let tree = copy();
    let root = tree.path();
    let outside = tempfile::tempdir().unwrap();
    let layer = root.join("planes/overworld/layers/1");
    fs::rename(&layer, outside.path().join("1")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("1"), &layer).unwrap();
    let before = snapshot(outside.path());
    for operations in [
        r#"{"op": "terrain.set", "plane": "overworld", "layer": 1, "grid": "height", "cells": [{"at": [0, 0], "value": 7}]}"#,
        r#"{"op": "document.create", "path": "planes/overworld/layers/1/5_5.jsonc", "text": "{}"}"#,
    ] {
        let found = problems(root, &transaction(operations));
        assert!(has(&found, "tx.path", "symbolic links"), "{found:#?}");
    }
    assert_eq!(snapshot(outside.path()), before);
}

#[test]
fn a_stale_staging_file_is_replaced() {
    let tree = copy();
    let root = tree.path();
    let stale = root.join("planes/overworld/layers/0/.0_0.jsonc.tilewright-tmp");
    fs::write(&stale, "left by a crash").unwrap();
    let tx = transaction(
        r#"{"op": "terrain.set", "plane": "overworld", "layer": 0, "grid": "height", "cells": [{"at": [0, 0], "value": 1}]}"#,
    );
    apply(root, &tx);
    assert!(!stale.exists());
    // One beside a file the transaction does not touch is still an error.
    fs::write(&stale, "left by a crash").unwrap();
    let other = transaction(
        r#"{"op": "set", "path": "planes/overworld/plane.jsonc", "pointer": ["title"], "value": "Changed"}"#,
    );
    let found = problems(root, &other);
    assert!(
        found
            .iter()
            .any(|problem| problem.code == "source.unexpected_file"),
        "{found:#?}"
    );
}

#[test]
fn the_inverse_recovers_an_interrupted_apply() {
    let base = copy();
    let tx = transaction(
        r#"{"op": "object.insert", "plane": "overworld", "layer": 3, "at": [1, 1], "object": {"proto": "tree"}},
    {"op": "document.delete", "path": "defs/recipe/chest.jsonc"},
    {"op": "set", "path": "planes/overworld/plane.jsonc", "pointer": ["title"], "value": "Changed"}"#,
    );
    let plan = planned(base.path(), &tx);
    let inverse = Transaction::parse("inverse.jsonc", plan.inverse()).unwrap();
    let before = snapshot(base.path());
    // Only some files were published when the apply stopped.
    let partial = copy();
    let changed = &plan.files()[1];
    assert_eq!(changed.path, "planes/overworld/layers/3/0_0.jsonc");
    fs::create_dir_all(partial.path().join("planes/overworld/layers/3")).unwrap();
    fs::write(
        partial.path().join(&changed.path),
        changed.after.as_ref().unwrap(),
    )
    .unwrap();
    fs::write(
        partial
            .path()
            .join("planes/overworld/layers/3/.0_0.jsonc.tilewright-tmp"),
        "staged",
    )
    .unwrap();
    // A file the apply staged but never published is already restored; its
    // staging file is removed too.
    fs::write(
        partial
            .path()
            .join("planes/overworld/.plane.jsonc.tilewright-tmp"),
        "staged",
    )
    .unwrap();
    apply(partial.path(), &inverse);
    assert_eq!(snapshot(partial.path()), before);
    // Applying it again finds every file restored and does nothing.
    let again = planned(partial.path(), &inverse);
    assert!(again.files().iter().all(|file| !file.is_changed()));
    // The forward transaction, whose operations read across files, does not
    // resume: it reports the conflict.
    let sealed = Transaction::parse("sealed.jsonc", plan.sealed()).unwrap();
    fs::create_dir_all(partial.path().join("planes/overworld/layers/3")).unwrap();
    fs::write(
        partial.path().join(&changed.path),
        changed.after.as_ref().unwrap(),
    )
    .unwrap();
    let found = problems(partial.path(), &sealed);
    assert!(has(&found, "tx.conflict", "rebase"), "{found:#?}");
}

#[test]
fn preconditions_fail_on_the_operation() {
    let tree = copy();
    let root = tree.path();
    for (operations, message) in [
        (
            r#"{"op": "object.remove", "plane": "overworld", "layer": 0, "at": [7, 0], "index": 0, "expect": {"proto": "wall"}}"#,
            "has proto \"tree\", expected \"wall\"",
        ),
        (
            r#"{"op": "object.remove", "plane": "overworld", "layer": 0, "at": [7, 0], "index": 1, "expect": {"proto": "tree"}}"#,
            "has no object at index 1",
        ),
        (
            r#"{"op": "object.insert", "plane": "overworld", "layer": 0, "at": [7, 0], "index": 2, "object": {"proto": "tree"}}"#,
            "index 2 is past the end",
        ),
        (
            r#"{"op": "set", "path": "planes/overworld/zones.jsonc", "pointer": ["zones", {"id": "nowhere"}, "name"], "value": "x"}"#,
            "no entry with id \"nowhere\"",
        ),
        (
            r#"{"op": "set", "path": "planes/overworld/plane.jsonc", "pointer": ["title"], "value": "x", "expect": "Underworld"}"#,
            "is \"Overworld\", expected \"Underworld\"",
        ),
        (
            r#"{"op": "test", "path": "planes/overworld/plane.jsonc", "pointer": ["title"], "absent": true}"#,
            "expected it to be absent",
        ),
        (
            r#"{"op": "document.create", "path": "world.jsonc", "text": "{}"}"#,
            "already exists",
        ),
        (
            r#"{"op": "document.delete", "path": "defs/proto/nothing.jsonc"}"#,
            "does not exist",
        ),
        (
            r#"{"op": "document.rename", "path": "defs/proto/tree.jsonc", "to": "defs/proto/wall.jsonc"}"#,
            "already exists",
        ),
        (
            r#"{"op": "insert", "path": "planes/overworld/anchors.jsonc", "pointer": ["anchors", 0], "value": 1, "before": ", 1, ", "after": ","}"#,
            "before and after must be whitespace and comments",
        ),
        (
            r#"{"op": "remove", "path": "planes/overworld/anchors.jsonc", "pointer": ["anchors", 0], "separator": "// open"}"#,
            "separator must be whitespace and comments",
        ),
        (
            r#"{"op": "insert", "path": "planes/overworld/plane.jsonc", "pointer": [0], "key": "title", "value": "x"}"#,
            "already has the key \"title\"",
        ),
        (
            r#"{"op": "document.create", "path": "defs/proto/broken.jsonc", "text": "{"}"#,
            "invalid",
        ),
    ] {
        let found = problems(root, &transaction(operations));
        assert!(
            has(&found, "tx.conflict", message),
            "{operations}: {found:#?}"
        );
        assert!(found[0].line >= 4, "{found:#?}");
    }
}

fn wrap(operations: &str) -> String {
    format!("{{\"format\": \"tilewright.transaction/1\", \"operations\": [{operations}]}}")
}

fn rejected(cases: &[(String, &str, &str)]) {
    for (text, code, message) in cases {
        let found = Transaction::parse("t.jsonc", text.as_str()).unwrap_err();
        let shown: String = text.chars().take(300).collect();
        assert!(has(&found, code, message), "{shown}: {found:#?}");
    }
}

#[test]
fn malformed_transactions_are_rejected() {
    let mut cases = vec![
        ("{".to_owned(), "jsonc.unexpected_end", "invalid JSONC"),
        (
            "{\"format\": \"x\", \"operations\": []}".to_owned(),
            "tx.shape",
            "format must be",
        ),
        (wrap(r#"{"op": "frobnicate"}"#), "tx.shape", "unknown operation"),
        (
            wrap(r#"{"op": "document.delete", "path": "defs/x.jsonc", "extra": 1}"#),
            "tx.shape",
            "unknown key",
        ),
        (
            "{\"format\": \"tilewright.transaction/1\", \"base\": [{\"path\": \"world.jsonc\", \"sha256\": \"abc\"}], \"operations\": []}".to_owned(),
            "tx.shape",
            "64 lowercase hexadecimal",
        ),
        (
            wrap(r#"{"op": "terrain.set", "plane": "p", "layer": 0, "grid": "floor", "cells": [{"at": [0, 0], "value": "a"}, {"at": [0, 0], "value": "b"}]}"#),
            "tx.shape",
            "listed twice",
        ),
        (
            wrap(r#"{"op": "terrain.set", "plane": "p", "layer": 0, "grid": "height", "cells": [{"at": [0, 0], "value": "a"}]}"#),
            "tx.shape",
            "expected an integer",
        ),
        (
            wrap(r#"{"op": "object.insert", "plane": "p", "layer": 0, "at": [0, 0], "object": {"at": [1, 1], "proto": "x"}}"#),
            "tx.shape",
            "leave out \"at\"",
        ),
        (
            wrap(r#"{"op": "object.remove", "plane": "p", "layer": 0, "at": [0, 0], "index": 0, "expect": {}}"#),
            "tx.shape",
            "names at least the object's proto",
        ),
        (
            wrap(r#"{"op": "set", "object": {"plane": "p", "layer": 0, "at": [0, 0], "index": 0}, "pointer": ["at"], "value": [1, 1]}"#),
            "tx.shape",
            "move the object",
        ),
        (
            wrap(r#"{"op": "object.insert", "plane": "p", "layer": 0, "at": [-9007199254740991, 0], "object": {"proto": "x"}}"#),
            "tx.shape",
            "outside the tiles",
        ),
        (
            wrap(r#"{"op": "insert", "path": "world.jsonc", "pointer": ["planes"], "value": 1}"#),
            "tx.shape",
            "ends with an index",
        ),
        (
            wrap(r#"{"op": "insert", "path": "world.jsonc", "pointer": ["planes", 0], "value": "x", "colon": "garbage"}"#),
            "tx.shape",
            "colon goes with key",
        ),
        (
            "{\"format\": \"tilewright.transaction/1\", \"base\": [{\"path\": \"world.jsonc\", \"sha256\": null}, {\"path\": \"world.jsonc\", \"sha256\": null}], \"operations\": []}".to_owned(),
            "tx.shape",
            "listed twice",
        ),
    ];
    for path in [
        "../x.jsonc",
        "notes.jsonc",
        "planes/.hidden.jsonc",
        "planes/x.json",
        "planes//x.jsonc",
    ] {
        cases.push((
            wrap(&format!(r#"{{"op": "document.delete", "path": "{path}"}}"#)),
            "tx.path",
            "not a source document path",
        ));
    }
    rejected(&cases);
}

#[test]
fn transactions_are_bounded() {
    let long: Vec<String> = (0..65).map(|_| "\"a\"".to_owned()).collect();
    let many = vec![r#"{"op": "document.delete", "path": "defs/x.jsonc"}"#; limits::OPERATIONS + 1];
    rejected(&[
        (
            wrap(&format!(
                r#"{{"op": "test", "path": "world.jsonc", "pointer": [{}], "absent": true}}"#,
                long.join(", ")
            )),
            "tx.limit",
            "at most 64 steps",
        ),
        (
            wrap(&format!(
                r#"{{"op": "remove", "path": "world.jsonc", "pointer": ["a"], "separator": "{}"}}"#,
                " ".repeat(limits::SEPARATOR + 1)
            )),
            "tx.limit",
            "longer than",
        ),
        (
            wrap(&many.join(",\n")),
            "tx.limit",
            "more than 100000 operations",
        ),
    ]);
}

#[test]
fn canonical_layout_is_bounded_and_stable() {
    use render::Node;
    let short = Node::Object(vec![("a".to_owned(), Node::Array(vec![Node::integer(1)]))]);
    assert_eq!(short.layout(""), "{\"a\": [1]}");
    assert_eq!(short.file(), "{\n  \"a\": [1]\n}\n");
    let numbers = Node::Array((0..40).map(Node::integer).collect());
    assert!(
        !numbers.layout("").contains('\n'),
        "arrays of numbers stay on one line"
    );
    let strings = Node::Array(
        (0..40)
            .map(|index| Node::String(format!("s{index}")))
            .collect(),
    );
    assert!(strings.layout("  ").contains("\n    \"s1\",\n"));
    // Deep nesting renders in time linear in its size.
    let mut deep = Node::String("x".repeat(1 << 20));
    for _ in 0..60 {
        deep = Node::Array(vec![deep]);
    }
    let text = deep.file();
    assert!(text.len() > 1 << 20);
    assert_eq!(text.matches('[').count(), 60);
    assert_eq!(
        Node::Array(vec![Node::String("\u{1}\"".to_owned())]).layout(""),
        "[\"\\u0001\\\"\"]"
    );
}

#[test]
fn every_splice_parses_and_its_inverse_is_exact() {
    use splice::{Insertion, Sequence, insert, remove};
    use tilewright_jsonc::Document;
    let containers = [
        "[]",
        "[ ]",
        "[\n]",
        "[1]",
        "[ 1 ]",
        "[1,2]",
        "[1, 2, 3]",
        "[\n  1,\n  2\n]",
        "[\r\n  1,\r\n  2\r\n]",
        "[ // c\n 1 /* d */, // e\n 2 // f\n]",
        "[/* a */1/* b */,/* c */2/* d */]",
        "{}",
        "{\"a\": 1}",
        "{\"a\":1,\"b\":2}",
        "{\n  // x\n  \"a\" : 1,\n  \"b\": 2 /* y */\n}",
    ];
    let limits = Limits::default();
    for source in containers {
        let document = Document::parse(source, &limits).unwrap();
        let root = document.root();
        let length = Sequence::of(root).unwrap().len();
        let object = root.as_object().is_some();
        for index in 0..=length {
            let insertion = Insertion {
                key: object.then_some(("new", "\"new\"")),
                value: "{\"z\": [9]}",
                layout: None,
                colon: None,
            };
            let inserted = insert(source, root, index, &insertion).unwrap();
            let edited = Document::parse(inserted.text.as_str(), &limits)
                .unwrap_or_else(|error| panic!("{source:?} at {index}: {error}"));
            let restored = remove(&inserted.text, edited.root(), index, Some(&inserted.region))
                .unwrap_or_else(|error| panic!("{source:?} at {index}: {error}"));
            assert_eq!(restored.text, source, "insert at {index}");
        }
        for index in 0..length {
            let removed = remove(source, root, index, None).unwrap();
            let edited = Document::parse(removed.text.as_str(), &limits)
                .unwrap_or_else(|error| panic!("{source:?} without {index}: {error}"));
            let key = removed.key.as_ref().map(|(key, _)| key.clone());
            let decoded = key.as_ref().map(|key| {
                Document::parse(key.as_str(), &limits)
                    .unwrap()
                    .root()
                    .as_str()
                    .unwrap()
                    .to_owned()
            });
            let insertion = Insertion {
                key: decoded.as_deref().zip(key.as_deref()),
                value: &removed.value,
                layout: Some(removed.layout.clone()),
                colon: removed.key.as_ref().map(|(_, colon)| colon.as_str()),
            };
            let restored = insert(&removed.text, edited.root(), index, &insertion)
                .unwrap_or_else(|error| panic!("{source:?} without {index}: {error}"));
            assert_eq!(restored.text, source, "remove at {index}");
        }
    }
}

#[test]
fn work_is_metered_and_planning_stops_at_the_budget() {
    let tree = copy();
    let root = tree.path();
    // A large value that every test compares against.
    let line = format!("\"{}\"", "x".repeat(512 << 10));
    let big = format!("[\n{}\n]", [line.as_str(); 16].join(",\n"));
    apply(
        root,
        &transaction(&format!(
            r#"{{"op": "set", "path": "planes/overworld/plane.jsonc", "pointer": ["custom"], "value": {{"example.org/big": {big}}}}}"#
        )),
    );
    let test = r#"{"op": "test", "path": "planes/overworld/plane.jsonc", "pointer": ["custom", "example.org/big"], "value": "x"}"#;
    let tests = vec![test; 2_000];
    let started = std::time::Instant::now();
    let found = problems(root, &transaction(&tests.join(",\n    ")));
    assert!(
        has(&found, "tx.limit", "bytes of work"),
        "{:#?}",
        &found[..3]
    );
    assert!(found.len() <= limits::DIAGNOSTICS + 1);
    assert!(
        found
            .iter()
            .filter(|problem| problem.code == "tx.conflict")
            .all(|problem| problem
                .message
                .contains(&format!("array of {} bytes", big.len()))),
        "large values are described, not rendered"
    );
    assert!(started.elapsed() < std::time::Duration::from_mins(1));
}

#[test]
fn the_result_is_validated_again_under_the_lock() {
    let tree = copy();
    let root = tree.path();
    // Valid alone: remove a loot table and every reference it can see.
    let remove = transaction(
        r#"{"op": "document.delete", "path": "defs/loot/chest_common.jsonc"},
    {"op": "remove", "object": {"plane": "overworld", "layer": 0, "at": [2, 2], "index": 0}, "pointer": ["fields", "loot"]},
    {"op": "remove", "object": {"plane": "vault", "layer": 0, "at": [1, 1], "index": 0}, "pointer": ["fields", "loot"]}"#,
    );
    // Valid alone: a new reference to it, in a file the other does not read.
    let reference = transaction(
        r#"{"op": "object.insert", "plane": "overworld", "layer": 2, "at": [0, 0], "object": {"proto": "chest", "fields": {"loot": "chest_common"}}}"#,
    );
    let removing = planned(root, &remove);
    apply(root, &reference);
    let before = snapshot(root);
    let failure = removing.publish().unwrap_err();
    assert!(!failure.applied);
    assert!(
        failure
            .problems
            .iter()
            .any(|problem| problem.code == "ref.unresolved"),
        "{failure:#?}"
    );
    assert_eq!(snapshot(root), before, "nothing was written");
}

#[test]
fn a_blocked_deletion_keeps_its_file() {
    let tree = copy();
    let root = tree.path();
    let tx = transaction(
        r#"{"op": "set", "path": "defs/proto/tree.jsonc", "pointer": ["fields", "face"], "value": "oak.png"},
    {"op": "document.delete", "path": "defs/recipe/chest.jsonc"}"#,
    );
    let plan = planned(root, &tx);
    // Something that cannot be removed sits at the deleted file's staging
    // name.
    fs::create_dir(root.join("defs/recipe/.chest.jsonc.tilewright-tmp")).unwrap();
    let before = snapshot(root);
    let failure = plan.publish().unwrap_err();
    assert!(!failure.applied);
    assert!(
        failure.problems[0].message.contains("restored"),
        "{failure:#?}"
    );
    assert_eq!(snapshot(root), before);
    assert!(root.join("defs/recipe/chest.jsonc").exists());
}

#[test]
fn a_failed_stage_leaves_nothing_behind() {
    let tree = copy();
    let root = tree.path();
    let folders = directories(root);
    let tx = transaction(
        r#"{"op": "object.insert", "plane": "overworld", "layer": 9, "at": [0, 0], "object": {"proto": "tree"}},
    {"op": "set", "path": "planes/overworld/plane.jsonc", "pointer": ["title"], "value": "Changed"}"#,
    );
    let plan = planned(root, &tx);
    let obstacle = root.join("planes/overworld/.plane.jsonc.tilewright-tmp");
    fs::create_dir(&obstacle).unwrap();
    let failure = plan.publish().unwrap_err();
    assert!(!failure.applied, "{failure:#?}");
    fs::remove_dir(&obstacle).unwrap();
    assert!(!root.join("planes/overworld/layers/9").exists());
    assert_eq!(directories(root), folders);
    assert_eq!(snapshot(root), snapshot(&fixture()));
}

#[test]
fn a_sync_failure_after_publishing_reports_the_result_applied() {
    let tree = copy();
    let root = tree.path();
    let tx = transaction(
        r#"{"op": "set", "path": "planes/overworld/plane.jsonc", "pointer": ["title"], "value": "Changed"}"#,
    );
    let plan = planned(root, &tx);
    publish::FAIL_SYNC.with(|fail| fail.set(true));
    let failure = plan.publish().unwrap_err();
    publish::FAIL_SYNC.with(|fail| fail.set(false));
    assert!(failure.applied, "{failure:#?}");
    assert!(
        failure
            .to_string()
            .contains("applied, but not made durable")
    );
    assert!(read(root, "planes/overworld/plane.jsonc").contains("\"Changed\""));
}

#[test]
fn names_cannot_carry_other_separators() {
    let tree = copy();
    let source = SourceRoot::new(tree.path());
    for path in [
        "defs\\proto",
        "c:x.jsonc",
        "defs/a\\..\\b.jsonc",
        "defs/x\u{0}.jsonc",
    ] {
        assert!(
            matches!(
                source.read(path, 1024),
                Err(tilewright_source::AccessError::Path(_))
            ),
            "{path}"
        );
    }
    let directory = source.directory("defs", false).unwrap().unwrap();
    for name in ["..\\outside", "C:\\outside", "a/b", "stream:x"] {
        assert!(
            matches!(
                directory.write(name, "x", b"x"),
                Err(tilewright_source::AccessError::Path(_))
            ),
            "{name}"
        );
    }
}

#[cfg(unix)]
#[test]
fn reading_stops_at_the_read_budget() {
    let tree = copy();
    let root = tree.path();
    // Many chunks, each a hard link to one large file.
    let layer = root.join("planes/overworld/layers/4");
    fs::create_dir_all(&layer).unwrap();
    let large = tree.path().join("large");
    fs::write(&large, " ".repeat(40 << 20)).unwrap();
    let mut cells = String::new();
    for cx in 0..100 {
        fs::hard_link(&large, layer.join(format!("{cx}_0.jsonc"))).unwrap();
        let _ = write!(
            cells,
            "{}{{\"at\": [{}, 0], \"value\": 1}}",
            if cells.is_empty() { "" } else { ", " },
            cx * 32
        );
    }
    let tx = transaction(&format!(
        r#"{{"op": "terrain.set", "plane": "overworld", "layer": 4, "grid": "height", "cells": [{cells}]}}"#
    ));
    let found = problems(root, &tx);
    assert!(
        has(&found, "tx.limit", "hold more than"),
        "{:#?}",
        &found[..3.min(found.len())]
    );
    // Seven files fit the 256 MiB budget; nothing past the eighth is read.
    assert!(found.len() <= 8, "{found:#?}");
}

#[test]
fn a_transaction_may_touch_more_files_than_an_object_has_keys() {
    let tree = copy();
    let root = tree.path();
    // Three hundred missing chunks, consulted and left empty.
    let cells: Vec<String> = (0..300)
        .map(|cx| format!("{{\"at\": [{}, 0], \"value\": 0}}", cx * 32 + 64))
        .collect();
    let tx = transaction(&format!(
        r#"{{"op": "terrain.set", "plane": "overworld", "layer": 0, "grid": "height", "cells": [{}]}}"#,
        cells.join(", ")
    ));
    let plan = planned(root, &tx);
    let sealed = Transaction::parse("sealed.jsonc", plan.sealed()).unwrap();
    assert_eq!(sealed.base().unwrap().len(), 300);
    assert!(sealed.base().unwrap().values().all(Option::is_none));
    planned(root, &sealed);
}

#[test]
fn a_stale_staging_entry_that_cannot_go_is_reported() {
    let tree = copy();
    let root = tree.path();
    let tx = transaction(
        r#"{"op": "test", "path": "planes/overworld/plane.jsonc", "pointer": ["id"], "value": "overworld"},
    {"op": "set", "path": "planes/overworld/zones.jsonc", "pointer": ["zones", 0, "name"], "value": "Wilds"}"#,
    );
    fs::create_dir(root.join("planes/overworld/.plane.jsonc.tilewright-tmp")).unwrap();
    let plan = planned(root, &tx);
    let failure = plan.publish().unwrap_err();
    assert!(
        failure.applied && failure.durable && !failure.cleaned,
        "{failure:#?}"
    );
    assert_eq!(
        failure.summary(),
        "the transaction was applied, but its cleanup is incomplete"
    );
    assert!(
        failure.problems[0]
            .path
            .ends_with(".plane.jsonc.tilewright-tmp")
    );
    assert!(read(root, "planes/overworld/zones.jsonc").contains("\"Wilds\""));
}

#[test]
fn terrain_addresses_are_checked_when_nothing_is_written() {
    let tree = copy();
    let root = tree.path();
    for (operations, message) in [
        (
            r#"{"op": "terrain.set", "plane": "nowhere", "layer": 0, "grid": "floor", "cells": [{"at": [0, 0], "value": null}]}"#,
            "plane \"nowhere\" does not exist",
        ),
        (
            r#"{"op": "terrain.set", "plane": "overworld", "layer": 5, "grid": "floro", "cells": [{"at": [0, 0], "value": null}]}"#,
            "\"floro\" is not a grid",
        ),
    ] {
        let found = problems(root, &transaction(operations));
        assert!(has(&found, "tx.conflict", message), "{found:#?}");
    }
}

#[test]
fn one_operation_touches_a_bounded_number_of_chunks() {
    let tree = copy();
    let cells: Vec<String> = (0..=limits::FILES)
        .map(|cx| format!("{{\"at\": [{}, 0], \"value\": 0}}", cx * 32))
        .collect();
    let tx = transaction(&format!(
        r#"{{"op": "terrain.set", "plane": "overworld", "layer": 0, "grid": "height", "cells": [{}]}}"#,
        cells.join(",\n")
    ));
    let found = problems(tree.path(), &tx);
    assert!(
        has(&found, "tx.limit", "more than 10000 chunks"),
        "{found:#?}"
    );
}

#[test]
fn the_inverse_log_is_bounded_while_carrying_out() {
    let tree = copy();
    let root = tree.path();
    // A large object, moved back and forth: each move keeps a copy of it
    // for the inverse.
    let line = format!("\"{}\"", "x".repeat(512 << 10));
    let custom = [line.as_str(); 8].join(",\n");
    let chunk = format!(
        "{{\"format\": \"tilewright.chunk/1\", \"plane\": \"overworld\", \"layer\": 6, \"chunk\": [0, 0],\n \"objects\": [{{\"at\": [0, 0], \"proto\": \"tree\", \"custom\": {{\"example.org/big\": [\n{custom}\n]}}}}]}}\n"
    );
    fs::create_dir_all(root.join("planes/overworld/layers/6")).unwrap();
    fs::write(root.join("planes/overworld/layers/6/0_0.jsonc"), chunk).unwrap();
    let there = r#"{"op": "object.move", "plane": "overworld", "layer": 6, "at": [0, 0], "index": 0, "expect": {"proto": "tree"}, "to": {"at": [1, 0]}}"#;
    let back = r#"{"op": "object.move", "plane": "overworld", "layer": 6, "at": [1, 0], "index": 0, "expect": {"proto": "tree"}, "to": {"at": [0, 0]}}"#;
    let moves: Vec<&str> = (0..20)
        .map(|step| if step % 2 == 0 { there } else { back })
        .collect();
    let found = problems(root, &transaction(&moves.join(",\n    ")));
    assert!(
        has(&found, "tx.limit", "the inverse would hold more than"),
        "{found:#?}"
    );
    assert_eq!(found.len(), 1);
}

#[cfg(unix)]
#[test]
fn a_rolled_back_deletion_keeps_its_permissions() {
    use std::os::unix::fs::PermissionsExt as _;
    let tree = copy();
    let root = tree.path();
    let recipe = root.join("defs/recipe/chest.jsonc");
    fs::set_permissions(&recipe, fs::Permissions::from_mode(0o600)).unwrap();
    let tx = transaction(
        r#"{"op": "document.delete", "path": "defs/recipe/chest.jsonc"},
    {"op": "set", "path": "planes/overworld/plane.jsonc", "pointer": ["title"], "value": "Changed"}"#,
    );
    let plan = planned(root, &tx);
    publish::FAIL_STEP.with(|fail| fail.set(Some(1)));
    let failure = plan.publish().unwrap_err();
    publish::FAIL_STEP.with(|fail| fail.set(None));
    assert!(!failure.applied);
    let mode = fs::metadata(&recipe).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    assert_eq!(snapshot(root), snapshot(&fixture()));
}

#[cfg(unix)]
#[test]
fn a_directory_that_cannot_be_pruned_is_reported() {
    use std::os::unix::fs::PermissionsExt as _;
    let tree = copy();
    let root = tree.path();
    apply(
        root,
        &transaction(
            r#"{"op": "object.insert", "plane": "overworld", "layer": 7, "at": [0, 0], "object": {"proto": "tree"}}"#,
        ),
    );
    let layers = root.join("planes/overworld/layers");
    fs::set_permissions(&layers, fs::Permissions::from_mode(0o555)).unwrap();
    // A user that can still write there (such as root) cannot see the
    // failure.
    let writable = fs::write(layers.join("probe"), "").is_ok();
    let _ = fs::remove_file(layers.join("probe"));
    let plan = planned(
        root,
        &transaction(
            r#"{"op": "object.remove", "plane": "overworld", "layer": 7, "at": [0, 0], "index": 0, "expect": {"proto": "tree"}}"#,
        ),
    );
    let outcome = plan.publish();
    fs::set_permissions(&layers, fs::Permissions::from_mode(0o755)).unwrap();
    if !writable {
        let failure = outcome.unwrap_err();
        assert!(failure.applied, "{failure:#?}");
        assert!(
            failure.problems[0].path.ends_with("layers/7"),
            "{failure:#?}"
        );
    }
    assert!(!root.join("planes/overworld/layers/7/0_0.jsonc").exists());
}

#[test]
fn an_added_member_keeps_the_key_spelling_of_its_pointer() {
    let tree = copy();
    let root = tree.path();
    let tx = transaction(
        r#"{"op": "set", "path": "planes/overworld/zones.jsonc", "pointer": ["zones", {"id": "wilderness"}, "properties"], "value": {"music": "wind"}}"#,
    );
    let after = round_trip(root, &tx);
    let zones = String::from_utf8(after["planes/overworld/zones.jsonc"].clone()).unwrap();
    assert!(
        zones.contains(r#""properties": {"music": "wind"}"#),
        "{zones}"
    );
}

#[cfg(unix)]
#[test]
fn cleanup_that_cannot_be_done_is_reported_with_the_outcome() {
    let tree = copy();
    let root = tree.path().to_path_buf();
    let tx = transaction(
        r#"{"op": "set", "path": "planes/overworld/plane.jsonc", "pointer": ["title"], "value": "Changed"},
    {"op": "test", "path": "planes/overworld/zones.jsonc", "pointer": ["format"], "value": "tilewright.zones/1"}"#,
    );
    let plan = planned(&root, &tx);
    let staging = root.join("planes/overworld/.plane.jsonc.tilewright-tmp");
    let zones = root.join("planes/overworld/zones.jsonc");
    let (hook_staging, hook_zones) = (staging.clone(), zones.clone());
    // While the transaction is staged, another edit changes a file it
    // consulted, and something that cannot be removed takes the place of
    // its staging file.
    publish::BEFORE_CHECK.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            let text = fs::read_to_string(&hook_zones).unwrap();
            fs::write(&hook_zones, format!("{text}\n")).unwrap();
            fs::remove_file(&hook_staging).unwrap();
            fs::create_dir(&hook_staging).unwrap();
            fs::write(hook_staging.join("x"), "").unwrap();
        }));
    });
    let failure = plan.publish().unwrap_err();
    assert!(!failure.applied && !failure.cleaned, "{failure:#?}");
    assert_eq!(
        failure.summary(),
        "the transaction was not applied, and its cleanup is incomplete"
    );
    assert!(
        failure.problems.iter().any(|problem| problem
            .path
            .ends_with(".plane.jsonc.tilewright-tmp")
            && problem.message.contains("remove it by hand")),
        "{failure:#?}"
    );
    fs::remove_dir_all(&staging).unwrap();
    assert_eq!(
        read(&root, "planes/overworld/plane.jsonc"),
        read(&fixture(), "planes/overworld/plane.jsonc")
    );
}

#[test]
fn a_plan_publishes_into_the_root_it_was_made_on() {
    let directory = tempfile::tempdir().unwrap();
    let planned_root = directory.path().join("source");
    for (path, bytes) in snapshot(&fixture()) {
        let target = planned_root.join(&path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, bytes).unwrap();
    }
    let tx = transaction(
        r#"{"op": "set", "path": "planes/overworld/plane.jsonc", "pointer": ["title"], "value": "Changed"}"#,
    );
    let plan = planned(&planned_root, &tx);
    // The path now names another, identical tree.
    let moved = directory.path().join("moved");
    fs::rename(&planned_root, &moved).unwrap();
    for (path, bytes) in snapshot(&fixture()) {
        let target = planned_root.join(&path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, bytes).unwrap();
    }
    plan.publish().unwrap();
    if cfg!(unix) {
        assert!(read(&moved, "planes/overworld/plane.jsonc").contains("\"Changed\""));
        assert_eq!(snapshot(&planned_root), snapshot(&fixture()));
    }
}

#[cfg(unix)]
#[test]
fn a_link_at_a_staging_name_does_not_stop_its_file_being_edited() {
    let tree = copy();
    let root = tree.path();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(
        outside.path(),
        root.join("planes/overworld/.plane.jsonc.tilewright-tmp"),
    )
    .unwrap();
    let tx = transaction(
        r#"{"op": "set", "path": "planes/overworld/plane.jsonc", "pointer": ["title"], "value": "Changed"}"#,
    );
    apply(root, &tx);
    assert!(read(root, "planes/overworld/plane.jsonc").contains("\"Changed\""));
    assert!(
        fs::symlink_metadata(root.join("planes/overworld/.plane.jsonc.tilewright-tmp")).is_err()
    );
    assert!(outside.path().exists());
}

#[test]
fn a_large_document_can_be_deleted_and_restored() {
    let tree = copy();
    let root = tree.path();
    // A definition of several megabytes, every line within the limits.
    let line = format!("    \"{}\"", "x".repeat(512 << 10));
    let lines = [line.as_str(); 6].join(",\n");
    let text = format!(
        "{{\n  \"format\": \"tilewright.def/1\", \"type\": \"proto\", \"id\": \"big\",\n  \"fields\": {{\"face\": \"big.png\", \"layer\": \"item\"}},\n  \"custom\": {{\"example.org/big\": [\n{lines}\n  ]}}\n}}\n"
    );
    fs::write(root.join("defs/proto/big.jsonc"), &text).unwrap();
    let tx = transaction(r#"{"op": "document.delete", "path": "defs/proto/big.jsonc"}"#);
    round_trip(root, &tx);
    assert_eq!(read(root, "defs/proto/big.jsonc"), text);
}

#[test]
fn an_absent_test_may_name_an_entry_by_id() {
    let tree = copy();
    let root = tree.path();
    planned(
        root,
        &transaction(
            r#"{"op": "test", "path": "planes/overworld/zones.jsonc", "pointer": ["zones", {"id": "desert"}], "absent": true}"#,
        ),
    );
    let found = problems(
        root,
        &transaction(
            r#"{"op": "test", "path": "planes/overworld/zones.jsonc", "pointer": ["zones", {"id": "island"}], "absent": true}"#,
        ),
    );
    assert!(
        has(&found, "tx.conflict", "has an entry with id \"island\""),
        "{found:#?}"
    );
}

#[test]
fn every_problem_is_reported_past_a_bad_digest_list() {
    let text = "{\"format\": \"tilewright.transaction/1\", \"base\": [{\"path\": \"world.jsonc\"}], \"result\": 1, \"operations\": [{\"op\": \"frobnicate\"}]}";
    let found = Transaction::parse("t.jsonc", text).unwrap_err();
    assert!(
        has(&found, "tx.shape", "missing key \"sha256\""),
        "{found:#?}"
    );
    assert!(has(&found, "tx.shape", "expected an array"), "{found:#?}");
    assert!(has(&found, "tx.shape", "unknown operation"), "{found:#?}");
}

#[test]
fn long_keys_and_scalar_arrays_keep_lines_short() {
    let tree = copy();
    let root = tree.path();
    // A key of 600 KiB, twice in one pointer, and a long array of numbers.
    let key = "k".repeat(600 << 10);
    let numbers: Vec<String> = (0..200_000).map(|number| number.to_string()).collect();
    let numbers = numbers
        .chunks(1_000)
        .map(|chunk| chunk.join(", "))
        .collect::<Vec<_>>()
        .join(",\n");
    let tx = transaction(&format!(
        r#"{{"op": "set", "path": "planes/overworld/plane.jsonc", "pointer": ["custom"], "value": {{"example.org/a": {{"{key}":
{{"{key}": 1}}}}}}}},
    {{"op": "remove", "path": "planes/overworld/plane.jsonc", "pointer": ["custom", "example.org/a", "{key}",
"{key}"]}},
    {{"op": "document.create", "path": "defs/proto/numbers.jsonc", "document": {{"format": "tilewright.def/1", "type": "proto", "id": "numbers",
      "fields": {{"face": "n.png", "layer": "item"}}, "custom": {{"example.org/n": [
{numbers}
]}}}}}}"#
    ));
    let plan = planned(root, &tx);
    Transaction::parse("inverse.jsonc", plan.inverse()).unwrap();
    let created = plan
        .files()
        .iter()
        .find(|file| file.path == "defs/proto/numbers.jsonc")
        .and_then(|file| file.after.clone())
        .unwrap();
    assert!(created.lines().all(|line| line.len() < 1 << 20));
}
