// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Objects addressed by uid (`docs/spec/TRANSACTIONS.md` section 2).
//!
//! These tests write a plan's files themselves rather than publish it, so
//! they run wherever planning does.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use tilewright_edit::{Options, Plan, Transaction, plan};
use tilewright_jsonc::{Document, Limits};
use tilewright_source::{Diagnostic, FieldValue, ObjectLocation, World};

const CHUNK: &str = "planes/overworld/layers/0/0_0.jsonc";
const VAULT: &str = "planes/vault/layers/0/0_0.jsonc";
const COIN: &str = "o.k3vq7zr2mxa5d6wfh4pnyt2cgu";

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

fn copy() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    for (path, bytes) in snapshot(&fixture()) {
        let target = directory.path().join(&path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, bytes).unwrap();
    }
    directory
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

fn planned(root: &Path, transaction: &Transaction, rebase: bool) -> Plan {
    let options = Options {
        rebase,
        ..Options::default()
    };
    match plan(root, transaction, &options) {
        Ok(plan) => plan,
        Err(problems) => panic!("{problems:#?}"),
    }
}

fn problems(root: &Path, transaction: &Transaction, rebase: bool) -> Vec<Diagnostic> {
    let options = Options {
        rebase,
        ..Options::default()
    };
    match plan(root, transaction, &options) {
        Ok(_) => panic!("the transaction should fail"),
        Err(problems) => problems,
    }
}

/// Writes a plan's files, removing the directories deleting files empties.
fn write(root: &Path, plan: &Plan) {
    for file in plan.files() {
        let target = root.join(&file.path);
        if let Some(text) = &file.after {
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(&target, text).unwrap();
            continue;
        }
        if target.exists() {
            fs::remove_file(&target).unwrap();
        }
        let mut directory = target.parent();
        while let Some(parent) = directory.filter(|parent| *parent != root) {
            if fs::remove_dir(parent).is_err() {
                break;
            }
            directory = parent.parent();
        }
    }
}

/// Plans and writes `transaction`, then plans and writes its inverse, and
/// checks that every file is as it was. Returns the plan.
fn round_trip(root: &Path, transaction: &Transaction) -> Plan {
    let before = snapshot(root);
    let plan = planned(root, transaction, false);
    write(root, &plan);
    let inverse = Transaction::parse("inverse.jsonc", plan.inverse()).unwrap();
    assert_addressed_by_path(plan.inverse());
    write(root, &planned(root, &inverse, false));
    assert_eq!(snapshot(root), before);
    plan
}

/// An inverse names files by path, whatever its transaction's addresses.
fn assert_addressed_by_path(inverse: &str) {
    let document = Document::parse(inverse, &Limits::default()).unwrap();
    let operations = document.root().get("operations").unwrap();
    for operation in operations.as_array().unwrap() {
        assert!(operation.get("path").is_some(), "{inverse}");
        for key in ["object", "uid", "plane"] {
            assert!(operation.get(key).is_none(), "{inverse}");
        }
    }
}

fn location(world: &World, uid: &str) -> ObjectLocation {
    world.object_location(uid).cloned().unwrap()
}

fn field(world: &World, uid: &str, name: &str) -> FieldValue {
    let object = world.object(uid).unwrap();
    let (_, value) = object.fields.iter().find(|(key, _)| key == name).unwrap();
    value.clone()
}

fn at(plane: &str, layer: i32, chunk: (i64, i64), path: &[usize]) -> ObjectLocation {
    ObjectLocation {
        plane: plane.to_owned(),
        chunk: (layer, chunk.0, chunk.1),
        path: path.to_vec(),
    }
}

/// The diagnostics with `code` whose message contains `text`, each with
/// the text of the operation it is at.
fn found<'a>(
    problems: &'a [Diagnostic],
    transaction: &'a Transaction,
    code: &str,
    text: &str,
) -> Vec<&'a str> {
    problems
        .iter()
        .filter(|problem| problem.code == code && problem.message.contains(text))
        .map(|problem| &transaction.text()[problem.start..problem.end])
        .collect()
}

#[test]
fn every_operation_takes_a_uid_address() {
    let tree = copy();
    let root = tree.path();
    let tx = transaction(
        r#"{"op": "set", "object": {"uid": "ruin.north_wall"}, "pointer": ["fields", "name"], "value": "New wall", "expect": "Old wall"},
    {"op": "insert", "object": {"uid": "village.chest"}, "pointer": ["inventory", 1], "value": {"proto": "coin"}},
    {"op": "remove", "object": {"uid": "village.chest"}, "pointer": ["fields", "locked"], "expect": true},
    {"op": "test", "object": {"uid": "vault.chest"}, "pointer": ["fields", "loot"], "value": "chest_common"},
    {"op": "object.move", "uid": "ruin.north_wall", "expect": {"proto": "wall"}, "to": {"at": [40, 0]}},
    {"op": "object.remove", "uid": "vault.chest", "expect": {"uid": "vault.chest", "proto": "chest"}},
    {"op": "object.remove", "uid": "o.k3vq7zr2mxa5d6wfh4pnyt2cgu", "expect": {"proto": "coin", "fields": {"count": 1}}}"#,
    );
    let plan = round_trip(root, &tx);
    let world = plan.world();
    assert_eq!(
        location(world, "ruin.north_wall"),
        at("overworld", 0, (1, 0), &[0])
    );
    assert_eq!(
        field(world, "ruin.north_wall", "name"),
        FieldValue::String("New wall".to_owned())
    );
    let chest = world.object("village.chest").unwrap();
    assert_eq!(chest.inventory.len(), 3);
    assert!(chest.fields.iter().all(|(key, _)| key != "locked"));
    // An inventory entry is removed from its inventory.
    assert!(
        world
            .object("village.chest.small")
            .unwrap()
            .inventory
            .is_empty()
    );
    assert!(world.object(COIN).is_none());
    assert!(world.object("vault.chest").is_none());
    let paths: Vec<&str> = plan.files().iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, [CHUNK, "planes/overworld/layers/0/1_0.jsonc", VAULT]);
}

#[test]
fn a_uid_address_follows_its_object_through_the_transaction() {
    let tree = copy();
    let root = tree.path();
    let tx = transaction(
        r#"{"op": "object.insert", "plane": "overworld", "layer": 0, "at": [15, 0], "index": 0, "object": {"proto": "tree"}},
    {"op": "set", "object": {"uid": "ruin.north_wall"}, "pointer": ["fields", "name"], "value": "Moved wall", "expect": "Old wall"},
    {"op": "object.move", "uid": "ruin.north_wall", "expect": {"proto": "wall"}, "to": {"index": 0}},
    {"op": "test", "object": {"plane": "overworld", "layer": 0, "at": [15, 0], "index": 0}, "pointer": ["uid"], "value": "ruin.north_wall"},
    {"op": "object.move", "uid": "ruin.north_wall", "expect": {"proto": "wall"}, "to": {"at": [5, 5]}},
    {"op": "object.move", "uid": "ruin.north_wall", "expect": {"proto": "wall"}, "to": {"at": [40, 3]}},
    {"op": "object.move", "uid": "ruin.north_wall", "expect": {"proto": "wall"}, "to": {"layer": 1}},
    {"op": "test", "object": {"plane": "overworld", "layer": 1, "at": [40, 3], "index": 0}, "pointer": ["uid"], "value": "ruin.north_wall"},
    {"op": "object.move", "uid": "ruin.north_wall", "expect": {"proto": "wall"}, "to": {"plane": "vault", "layer": 0, "at": [3, 4]}},
    {"op": "set", "object": {"uid": "ruin.north_wall"}, "pointer": ["fields", "name"], "value": "Vault wall", "expect": "Moved wall"},
    {"op": "object.move", "uid": "village.chest", "expect": {"proto": "chest"}, "to": {"plane": "vault", "layer": 0, "at": [70, 70]}},
    {"op": "set", "object": {"uid": "o.k3vq7zr2mxa5d6wfh4pnyt2cgu"}, "pointer": ["fields", "count"], "value": 2, "expect": 1}"#,
    );
    let plan = round_trip(root, &tx);
    let world = plan.world();
    assert_eq!(
        location(world, "ruin.north_wall"),
        at("vault", 0, (0, 0), &[1])
    );
    assert_eq!(
        field(world, "ruin.north_wall", "name"),
        FieldValue::String("Vault wall".to_owned())
    );
    // The object keeps its inventory, which keeps its uids.
    assert_eq!(location(world, COIN), at("vault", 0, (2, 2), &[0, 1, 0]));
    assert_eq!(field(world, COIN, "count"), FieldValue::Integer(2));
    // Chunks the object passed through hold nothing, so they are gone.
    let paths: Vec<(&str, bool)> = plan
        .files()
        .iter()
        .map(|file| (file.path.as_str(), file.after.is_some()))
        .collect();
    assert_eq!(
        paths,
        [
            (CHUNK, true),
            ("planes/overworld/layers/0/1_0.jsonc", false),
            ("planes/overworld/layers/1/1_0.jsonc", false),
            (VAULT, true),
            ("planes/vault/layers/0/2_2.jsonc", true),
        ]
    );
}

#[test]
fn objects_a_transaction_creates_can_be_given_uids_and_addressed_by_them() {
    let tree = copy();
    let root = tree.path();
    let tx = transaction(
        r#"{"op": "object.insert", "plane": "overworld", "layer": 0, "at": [20, 20], "object": {"uid": "orchard.apple", "proto": "tree"}},
    {"op": "set", "object": {"uid": "orchard.apple"}, "pointer": ["z"], "value": 2},
    {"op": "object.move", "uid": "orchard.apple", "expect": {"proto": "tree", "z": 2}, "to": {"at": [70, 20]}},
    {"op": "set", "object": {"uid": "vault.chest"}, "pointer": ["inventory"], "value": [{"uid": "vault.key", "proto": "coin"}]},
    {"op": "test", "object": {"uid": "vault.key"}, "pointer": ["proto"], "value": "coin"},
    {"op": "set", "object": {"uid": "village.chest"}, "pointer": ["uid"], "value": "village.big_chest", "expect": "village.chest"},
    {"op": "test", "object": {"uid": "village.big_chest"}, "pointer": ["proto"], "value": "chest"}"#,
    );
    let plan = round_trip(root, &tx);
    let world = plan.world();
    assert_eq!(
        location(world, "orchard.apple"),
        at("overworld", 0, (2, 0), &[0])
    );
    assert_eq!(
        location(world, "vault.key"),
        at("vault", 0, (0, 0), &[0, 0])
    );
    assert!(world.object("village.chest").is_none());
    assert_eq!(
        location(world, "village.big_chest"),
        at("overworld", 0, (0, 0), &[4])
    );
}

#[test]
fn a_rebase_finds_an_object_the_base_moved() {
    let tree = copy();
    let root = tree.path();
    let by_uid = transaction(
        r#"{"op": "set", "object": {"uid": "ruin.north_wall"}, "pointer": ["fields", "name"], "value": "New wall", "expect": "Old wall"}"#,
    );
    let by_place = transaction(
        r#"{"op": "set", "object": {"plane": "overworld", "layer": 0, "at": [15, 0], "index": 0}, "pointer": ["fields", "name"], "value": "New wall", "expect": "Old wall"}"#,
    );
    let sealed = planned(root, &by_uid, false).sealed().to_owned();
    let sealed = Transaction::parse("sealed.jsonc", sealed).unwrap();
    assert_eq!(sealed.base().unwrap().keys().collect::<Vec<_>>(), [CHUNK]);

    // The base moves on: an object is inserted before the wall on its tile,
    // and then the wall moves to another plane.
    let moves = transaction(
        r#"{"op": "object.insert", "plane": "overworld", "layer": 0, "at": [15, 0], "index": 0, "object": {"proto": "tree"}},
    {"op": "object.move", "plane": "overworld", "layer": 0, "at": [15, 0], "index": 1, "expect": {"proto": "wall"}, "to": {"plane": "vault", "layer": 0, "at": [3, 4]}}"#,
    );
    write(root, &planned(root, &moves, false));

    // The sealed transaction's base changed, so it must be rebased.
    let found = problems(root, &sealed, false);
    assert_eq!(
        found
            .iter()
            .filter(|problem| problem.message.contains("rebase"))
            .count(),
        1,
        "{found:#?}"
    );
    // Addressed by place, the edit now meets another object.
    let found = problems(root, &by_place, false);
    assert_eq!(
        found.iter().map(|problem| problem.code).collect::<Vec<_>>(),
        ["tx.conflict"],
        "{found:#?}"
    );
    // Addressed by uid, it is carried out on the object wherever it went.
    assert!(
        planned(root, &by_uid, false)
            .files()
            .iter()
            .any(|file| file.path == VAULT)
    );
    let before = snapshot(root);
    let rebased = planned(root, &sealed, true);
    assert_eq!(
        field(rebased.world(), "ruin.north_wall", "name"),
        FieldValue::String("New wall".to_owned())
    );
    // The rebased transaction keeps its uid address and records the chunk
    // the object is in now as its base.
    let resealed = Transaction::parse("rebased.jsonc", rebased.sealed()).unwrap();
    assert!(
        rebased
            .sealed()
            .contains(r#""object": {"uid": "ruin.north_wall"}"#)
    );
    assert_eq!(resealed.base().unwrap().keys().collect::<Vec<_>>(), [VAULT]);
    // It applies as sealed, and its inverse restores the moved base.
    let applied = planned(root, &resealed, false);
    assert_eq!(applied.sealed(), rebased.sealed());
    write(root, &applied);
    let inverse = Transaction::parse("inverse.jsonc", applied.inverse()).unwrap();
    write(root, &planned(root, &inverse, false));
    assert_eq!(snapshot(root), before);
}

#[test]
fn uid_conflicts_are_failed_preconditions_at_their_operations() {
    let tree = copy();
    let root = tree.path();
    let tx = transaction(
        r#"{"op": "set", "object": {"uid": "no.such"}, "pointer": ["z"], "value": 1},
    {"op": "insert", "object": {"uid": "no.such"}, "pointer": ["inventory", 0], "value": {"proto": "coin"}},
    {"op": "remove", "object": {"uid": "no.such"}, "pointer": ["z"]},
    {"op": "test", "object": {"uid": "no.such"}, "pointer": ["z"], "value": 0},
    {"op": "object.remove", "uid": "no.such", "expect": {"proto": "tree"}},
    {"op": "object.move", "uid": "no.such", "expect": {"proto": "tree"}, "to": {"at": [1, 1]}},
    {"op": "object.remove", "uid": "vault.chest", "expect": {"proto": "tree"}},
    {"op": "object.move", "uid": "village.chest", "expect": {"proto": "chest", "z": 1}, "to": {"at": [1, 1]}},
    {"op": "set", "object": {"uid": "ruin.north_wall"}, "pointer": ["fields", "name"], "value": "x", "expect": "Older wall"},
    {"op": "object.move", "uid": "village.chest.small", "expect": {"proto": "chest"}, "to": {"at": [1, 1]}},
    {"op": "object.remove", "uid": "ruin.north_wall", "expect": {"proto": "wall"}},
    {"op": "test", "object": {"uid": "ruin.north_wall"}, "pointer": ["proto"], "value": "wall"},
    {"op": "object.insert", "plane": "vault", "layer": 0, "at": [2, 2], "object": {"uid": "village.chest", "proto": "chest"}},
    {"op": "test", "object": {"uid": "village.chest"}, "pointer": ["proto"], "value": "chest"}"#,
    );
    let found = problems(root, &tx, false);
    let missing = found_in(&found, &tx, "no object has uid \"no.such\"");
    assert_eq!(missing.len(), 6, "{found:#?}");
    assert_eq!(
        found_in(
            &found,
            &tx,
            "object \"vault.chest\" has \"proto\" \"chest\", expected \"tree\""
        ),
        [r#"{"op": "object.remove", "uid": "vault.chest", "expect": {"proto": "tree"}}"#]
    );
    assert_eq!(
        found_in(
            &found,
            &tx,
            "object \"village.chest\" has \"z\" absent, expected 1"
        )
        .len(),
        1,
        "{found:#?}"
    );
    assert_eq!(
        found_in(
            &found,
            &tx,
            "\"fields\", \"name\"] is \"Old wall\", expected \"Older wall\""
        )
        .len(),
        1,
        "{found:#?}"
    );
    assert_eq!(
        found_in(
            &found,
            &tx,
            "object \"village.chest.small\" is in an inventory"
        )
        .len(),
        1,
        "{found:#?}"
    );
    // A uid the transaction removed is gone for the operations after it.
    assert_eq!(
        found_in(&found, &tx, "no object has uid \"ruin.north_wall\""),
        [
            r#"{"op": "test", "object": {"uid": "ruin.north_wall"}, "pointer": ["proto"], "value": "wall"}"#
        ]
    );
    // One the transaction repeats names no single object.
    assert_eq!(
        found_in(
            &found,
            &tx,
            "more than one object has uid \"village.chest\""
        ),
        [
            r#"{"op": "test", "object": {"uid": "village.chest"}, "pointer": ["proto"], "value": "chest"}"#
        ]
    );
    assert_eq!(found.len(), 12, "{found:#?}");
    assert_eq!(snapshot(root), snapshot(&fixture()));
}

fn found_in<'a>(
    problems: &'a [Diagnostic],
    transaction: &'a Transaction,
    text: &str,
) -> Vec<&'a str> {
    found(problems, transaction, "tx.conflict", text)
}

#[test]
fn a_uid_does_not_resolve_in_a_tree_that_does_not_load() {
    let tree = copy();
    let root = tree.path();
    fs::write(root.join("defs/proto/tree.jsonc"), "{}").unwrap();
    let tx = transaction(
        r#"{"op": "set", "object": {"uid": "ruin.north_wall"}, "pointer": ["z"], "value": 1}"#,
    );
    let found = problems(root, &tx, false);
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(found[0].code, "tx.conflict");
    assert!(
        found[0]
            .message
            .starts_with("uid \"ruin.north_wall\" cannot be resolved: the tree does not load (defs/proto/tree.jsonc: "),
        "{found:#?}"
    );
}

#[test]
fn uid_addresses_are_checked_for_shape() {
    let cases = [
        (
            r#"{"op": "object.remove", "uid": "ruin.north_wall", "plane": "overworld", "index": 0, "expect": {"proto": "wall"}}"#,
            "addressed by its uid alone",
        ),
        (
            r#"{"op": "set", "object": {"uid": "ruin.north_wall", "at": [15, 0]}, "pointer": ["z"], "value": 1}"#,
            "addressed by its uid alone",
        ),
        (
            r#"{"op": "set", "object": {"uid": "Not.An.Id"}, "pointer": ["z"], "value": 1}"#,
            "is not an identifier",
        ),
        (
            r#"{"op": "object.move", "uid": 7, "expect": {"proto": "wall"}, "to": {}}"#,
            "expected a string",
        ),
        (
            r#"{"op": "object.insert", "plane": "overworld", "layer": 0, "at": [1, 1], "uid": "x", "object": {"proto": "tree"}}"#,
            "unknown key \"uid\"",
        ),
        (
            r#"{"op": "set", "object": {"uid": "ruin.north_wall"}, "pointer": ["at"], "value": [1, 1]}"#,
            "move the object to change its tile",
        ),
        (
            r#"{"op": "object.remove", "uid": "ruin.north_wall", "expect": {"proto": "wall", "at": [15, 0]}}"#,
            "leave out \"at\"",
        ),
    ];
    for (operation, message) in cases {
        let text =
            format!("{{\"format\": \"tilewright.transaction/1\", \"operations\": [{operation}]}}");
        let found = Transaction::parse("tx.jsonc", text).unwrap_err();
        assert!(
            found
                .iter()
                .any(|problem| problem.code == "tx.shape" && problem.message.contains(message)),
            "{operation}: {found:#?}"
        );
    }
}

#[test]
fn uid_plans_are_deterministic() {
    let tx = transaction(
        r#"{"op": "object.move", "uid": "village.chest", "expect": {"proto": "chest"}, "to": {"plane": "vault", "layer": 0, "at": [40, 1]}},
    {"op": "object.insert", "plane": "vault", "layer": 0, "at": [40, 1], "index": 0, "object": {"uid": "vault.lamp", "proto": "tree"}},
    {"op": "object.move", "uid": "vault.lamp", "expect": {"proto": "tree"}, "to": {"index": 1}},
    {"op": "set", "object": {"uid": "village.chest.small"}, "pointer": ["z"], "value": 3}"#,
    );
    let plans: Vec<(String, String, Vec<tilewright_edit::FileChange>)> = (0..2)
        .map(|_| {
            let tree = copy();
            let plan = planned(tree.path(), &tx, false);
            (
                plan.sealed().to_owned(),
                plan.inverse().to_owned(),
                plan.files().to_vec(),
            )
        })
        .collect();
    assert_eq!(plans[0], plans[1]);
    let moved = plans[0]
        .2
        .iter()
        .find(|file| file.path == "planes/vault/layers/0/1_0.jsonc")
        .and_then(|file| file.after.as_deref())
        .unwrap();
    let lamp = moved.find("vault.lamp").unwrap();
    let chest = moved.find("village.chest\"").unwrap();
    assert!(chest < lamp, "{moved}");
}

#[test]
fn a_move_by_uid_to_where_the_object_is_changes_nothing() {
    let tree = copy();
    let root = tree.path();
    let tx = transaction(
        r#"{"op": "object.move", "uid": "vault.chest", "expect": {"proto": "chest"}, "to": {}},
    {"op": "object.move", "uid": "ruin.north_wall", "expect": {"proto": "wall"}, "to": {"plane": "overworld", "layer": 0, "at": [15, 0], "index": 0}}"#,
    );
    let plan = planned(root, &tx, false);
    let paths: Vec<&str> = plan.files().iter().map(|file| file.path.as_str()).collect();
    assert_eq!(paths, [CHUNK, VAULT]);
    assert!(plan.files().iter().all(|file| !file.is_changed()));
}
