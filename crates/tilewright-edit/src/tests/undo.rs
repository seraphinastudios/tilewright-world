// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! The undo check, [`Plan::check_undo`]: a plan is refused before anything
//! is written when its inverse could not be carried out on the tree it
//! leaves, or the inverse's own inverse on the tree that leaves, and so on
//! until undo and redo settle; and one it passes can be undone and redone
//! as often as wanted.

use super::*;
use crate::engine::{Budgets, TEST_BUDGETS};

/// This thread's budgets, lowered until dropped.
struct Lowered(Option<Budgets>);

impl Lowered {
    fn to(read_bytes: u64, work: u64) -> Self {
        Self(TEST_BUDGETS.replace(Some(Budgets { read_bytes, work })))
    }
}

impl Drop for Lowered {
    fn drop(&mut self) {
        TEST_BUDGETS.set(self.0);
    }
}

fn reparsed(name: &str, text: &str) -> Transaction {
    Transaction::parse(name, text).unwrap_or_else(|problems| panic!("{problems:#?}"))
}

fn published(plan: &Plan) {
    if let Err(error) = publish(plan) {
        panic!("{error:#?}");
    }
}

/// The check's problems, which must refuse `plan` at the transaction's
/// name.
fn refused(plan: &Plan) -> Vec<Diagnostic> {
    let found = plan
        .check_undo()
        .expect_err("the undo check should refuse the plan");
    assert!(
        found
            .iter()
            .all(|problem| problem.path == "tx.jsonc" && problem.code == "tx.limit"),
        "{found:#?}"
    );
    found
}

/// Publishes `plan`, which passed the undo check, then plans and publishes
/// its undo, its redo, and so on, until a round's inverse is the
/// transaction of the round before it: from then on undo and redo
/// alternate between two transactions. Each round plans; each undo
/// restores every file and each redo gives every file the plan did.
/// Returns the transactions of the rounds, the undo first, and the inverse
/// of the last.
fn cycled(root: &Path, plan: &Plan) -> Vec<String> {
    let before = snapshot(root);
    published(plan);
    let after = snapshot(root);
    let mut rounds = vec![plan.inverse().to_owned()];
    for round in 0..6 {
        let planned = apply(root, &reparsed("round.jsonc", &rounds[round]));
        let expected = if round % 2 == 0 { &before } else { &after };
        assert!(snapshot(root) == *expected, "round {round}");
        rounds.push(planned.inverse().to_owned());
        if round >= 1 && rounds[round + 1] == rounds[round - 1] {
            return rounds;
        }
    }
    panic!("undo and redo do not settle in six rounds");
}

/// Whether the check found that `what` would fail for the reason `problem`
/// gives, a problem of planning that round itself, whose messages name
/// the round as the transaction.
fn gives(found: &[Diagnostic], what: &str, problem: &Diagnostic) -> bool {
    let reason = ["the transaction ", "the inverse "]
        .iter()
        .find_map(|subject| problem.message.strip_prefix(subject))
        .unwrap_or(&problem.message);
    found.iter().any(|check| {
        check.code == problem.code
            && check.message.starts_with(what)
            && check.message.contains(reason)
    })
}

/// The number of operations of a transaction's text.
fn operations(text: &str) -> usize {
    reparsed("count.jsonc", text).len()
}

/// A tree with `chunks` chunks of grass on layer 2 of the overworld, and
/// `values` more floor protos, `p00` on.
fn painted(chunks: i64, values: usize) -> tempfile::TempDir {
    let tree = copy();
    let root = tree.path();
    for value in 0..values {
        fs::write(
            root.join(format!("defs/proto/p{value:02}.jsonc")),
            format!(
                "{{\"format\": \"tilewright.def/1\", \"type\": \"proto\", \"id\": \"p{value:02}\", \"fields\": {{\"face\": \"p.png\", \"layer\": \"floor\"}}}}\n"
            ),
        )
        .unwrap();
    }
    let layer = root.join("planes/overworld/layers/2");
    fs::create_dir_all(&layer).unwrap();
    let rows = vec![format!("        \"{}\"", "g".repeat(32)); 32].join(",\n");
    for cx in 0..chunks {
        fs::write(
            layer.join(format!("{cx}_0.jsonc")),
            format!(
                "{{\n  \"format\": \"tilewright.chunk/1\",\n  \"plane\": \"overworld\",\n  \"layer\": 2,\n  \"chunk\": [{cx}, 0],\n  \"terrain\": {{\n    \"floor\": {{\n      \"palette\": {{\"g\": \"grass\"}},\n      \"rows\": [\n{rows}\n      ]\n    }}\n  }}\n}}\n"
            ),
        )
        .unwrap();
    }
    tree
}

/// A thin stroke along row 0 of every chunk of [`painted`]: its first
/// `values` cells, each a value the palette lacks.
fn stroke(chunks: i64, values: usize) -> Transaction {
    let lines: Vec<String> = (0..chunks)
        .map(|cx| {
            (0..values)
                .map(|value| {
                    format!(
                        "{{\"at\": [{}, 0], \"value\": \"p{value:02}\"}}",
                        cx * 32 + i64::try_from(value).unwrap()
                    )
                })
                .collect::<Vec<_>>()
                .join(", ")
        })
        .collect();
    transaction(&format!(
        "{{\"op\": \"terrain.set\", \"plane\": \"overworld\", \"layer\": 2, \"grid\": \"floor\", \"cells\": [\n{}\n]}}",
        lines.join(",\n")
    ))
}

// Each value these strokes paint is a palette entry they add. The inverse
// removes each in 8 lines; the inverse's own inverse, which planning the
// undo builds, puts each back in 16. So a stroke over 479 chunks fits a
// transaction, and so does its inverse, but its undo does not: the redo
// would pass the 250,000 lines of a document.

#[test]
fn a_stroke_whose_undo_could_not_hold_its_redo_is_refused() {
    let tree = painted(479, 32);
    let root = tree.path();
    let plan = planned(root, &stroke(479, 32));
    let lines = plan.inverse().lines().count();
    assert!(lines < 130_000, "{lines}");
    let found = refused(&plan);
    assert!(
        has(
            &found,
            "tx.limit",
            "could not be undone: its inverse would be refused: its own inverse would exceed the limits of a transaction: invalid JSONC (jsonc.too_many_lines); split the transaction"
        ),
        "{found:#?}"
    );
    // Applied anyway, its undo is refused for that reason.
    published(&plan);
    let undo = problems(root, &reparsed("undo.jsonc", plan.inverse()));
    assert!(
        has(
            &undo,
            "tx.limit",
            "the inverse would exceed the limits of a transaction: invalid JSONC (jsonc.too_many_lines)"
        ),
        "{undo:#?}"
    );
}

#[test]
fn a_stroke_just_short_of_the_line_limit_is_undone_and_redone() {
    let tree = painted(478, 32);
    let root = tree.path();
    let plan = planned(root, &stroke(478, 32));
    plan.check_undo().unwrap();
    let rounds = cycled(root, &plan);
    let lines = rounds[1].lines().count();
    assert!((249_000..=250_000).contains(&lines), "{lines}");
    // The redo's inverse is the undo again.
    assert_eq!(rounds.len(), 3);
}

/// Which budget [`boundary`] lowers.
#[derive(Clone, Copy)]
enum Budget {
    Read,
    Work,
}

fn lowered(budget: Budget, value: u64) -> Lowered {
    match budget {
        Budget::Read => Lowered::to(value, limits::WORK),
        Budget::Work => Lowered::to(limits::READ_BYTES, value),
    }
}

/// The least value of `budget` under which `transaction` plans on a tree
/// of `tree` and passes the undo check, found by bisection, checked from
/// both sides: one less, the transaction plans but the check refuses it,
/// and once it is applied, planning its undo or its redo fails as the
/// check says; at the value, it is undone and redone. Returns the value
/// and what the check found one below.
fn boundary(
    tree: impl Fn() -> tempfile::TempDir,
    transaction: &Transaction,
    budget: Budget,
) -> (u64, Vec<Diagnostic>) {
    let probe = tree();
    let passes = |value| {
        let _lowered = lowered(budget, value);
        plan(probe.path(), transaction, &Options::default())
            .is_ok_and(|plan| plan.check_undo().is_ok())
    };
    let (mut low, mut high) = match budget {
        Budget::Read => (0, limits::READ_BYTES),
        Budget::Work => (0, limits::WORK),
    };
    assert!(passes(high) && !passes(low));
    while high - low > 1 {
        let middle = low + (high - low) / 2;
        if passes(middle) {
            high = middle;
        } else {
            low = middle;
        }
    }

    let below = tree();
    let root = below.path();
    let found = {
        let _lowered = lowered(budget, low);
        let plan = planned(root, transaction);
        let found = refused(&plan);
        published(&plan);
        let undo = reparsed("undo.jsonc", plan.inverse());
        let (what, failed) = match crate::plan(root, &undo, &Options::default()) {
            Err(failed) => (ROUNDS[0], failed),
            Ok(undone) => {
                published(&undone);
                let redo = reparsed("redo.jsonc", undone.inverse());
                (ROUNDS[1], problems(root, &redo))
            }
        };
        for problem in &failed {
            assert!(gives(&found, what, problem), "{found:#?}\n{failed:#?}");
        }
        found
    };

    let at = tree();
    let _lowered = lowered(budget, high);
    let plan = planned(at.path(), transaction);
    plan.check_undo().unwrap();
    cycled(at.path(), &plan);
    (high, found)
}

#[test]
fn an_undo_that_would_read_past_the_budget_is_refused() {
    // The step grows one definition and creates another. It reads only the
    // first as it was; its undo reads both as it leaves them.
    let pad = "x".repeat(3_000);
    let tx = transaction(&format!(
        r#"{{"op": "set", "path": "defs/proto/tree.jsonc", "pointer": ["custom"], "value": {{"example.org/pad": "{pad}"}}}},
    {{"op": "document.create", "path": "defs/proto/padded.jsonc", "document": {{"format": "tilewright.def/1", "type": "proto", "id": "padded", "fields": {{"face": "padded.png", "layer": "item"}}, "custom": {{"example.org/pad": "{pad}"}}}}}}"#
    ));
    let probe = copy();
    let leaves: u64 = planned(probe.path(), &tx)
        .files()
        .iter()
        .filter(|file| file.is_changed())
        .filter_map(|file| file.after.as_ref())
        .map(|text| u64::try_from(text.len()).unwrap())
        .sum();
    let (least, found) = boundary(copy, &tx, Budget::Read);
    // Exactly the files it leaves: it read far less.
    assert_eq!(least, leaves);
    assert!(read(&fixture(), "defs/proto/tree.jsonc").len() < 200);
    assert!(
        has(
            &found,
            "tx.limit",
            &format!(
                "could not be undone: its inverse would be refused: the touched files hold more than {} bytes; split the transaction",
                least - 1
            )
        ),
        "{found:#?}"
    );
}

/// The fixture with a chunk on layer 3 of the overworld that holds 32 rows
/// of grass and `objects` objects, each with 8 KiB of custom data.
fn heavy(objects: usize) -> tempfile::TempDir {
    let tree = copy();
    let layer = tree.path().join("planes/overworld/layers/3");
    fs::create_dir_all(&layer).unwrap();
    let rows = vec![format!("        \"{}\"", "g".repeat(32)); 32].join(",\n");
    let objects: Vec<String> = (0..objects)
        .map(|index| {
            format!(
                "    {{\"at\": [{index}, 31], \"proto\": \"tree\", \"custom\": {{\"example.org/pad\": \"{}\"}}}}",
                "x".repeat(8 << 10)
            )
        })
        .collect();
    fs::write(
        layer.join("0_0.jsonc"),
        format!(
            "{{\n  \"format\": \"tilewright.chunk/1\",\n  \"plane\": \"overworld\",\n  \"layer\": 3,\n  \"chunk\": [0, 0],\n  \"terrain\": {{\n    \"floor\": {{\n      \"palette\": {{\"g\": \"grass\"}},\n      \"rows\": [\n{rows}\n      ]\n    }}\n  }},\n  \"objects\": [\n{}\n  ]\n}}\n",
            objects.join(",\n")
        ),
    )
    .unwrap();
    tree
}

/// A thin stroke of sand down column 0 of the chunk of [`heavy`].
fn column() -> String {
    let cells: Vec<String> = (0..16)
        .map(|y| format!("{{\"at\": [0, {y}], \"value\": \"sand\"}}"))
        .collect();
    format!(
        r#"{{"op": "terrain.set", "plane": "overworld", "layer": 3, "grid": "floor", "cells": [{}]}}"#,
        cells.join(", ")
    )
}

// Planning an undo and its redo edits the same versions of each document,
// in opposite orders, so each reads and writes the same bytes; they differ
// in their operations, each of which holds the value it writes.

#[test]
fn an_undo_that_would_need_more_work_than_the_budget_is_refused() {
    // The stroke is one operation that reads and edits the chunk once; its
    // undo sets each row it changed in an operation of its own, each
    // reading and editing the whole chunk. The step also removes an
    // object, which its undo holds to put back.
    let tx = transaction(&format!(
        r#"{},
    {{"op": "object.remove", "plane": "overworld", "layer": 3, "at": [0, 31], "index": 0, "expect": {{"proto": "tree"}}}}"#,
        column()
    ));
    let (least, found) = boundary(|| heavy(16), &tx, Budget::Work);
    assert!(
        has(
            &found,
            "tx.limit",
            "could not be undone: its inverse would be refused"
        ) && has(&found, "tx.limit", "bytes of work"),
        "{found:#?}"
    );
    // The step itself needs a small part of that.
    let tree = heavy(16);
    let _lowered = Lowered::to(limits::READ_BYTES, least / 4);
    planned(tree.path(), &tx);
}

#[test]
fn a_redo_that_would_need_more_work_than_the_budget_is_refused() {
    // The step places an object, which its undo removes with a few bytes
    // and its redo holds to place again: it is the redo that does not
    // fit.
    let tx = transaction(&format!(
        r#"{},
    {{"op": "object.insert", "plane": "overworld", "layer": 3, "at": [1, 31], "object": {{"proto": "tree", "custom": {{"example.org/pad": "{}"}}}}}}"#,
        column(),
        "y".repeat(8 << 10)
    ));
    let (_, found) = boundary(|| heavy(16), &tx, Budget::Work);
    assert!(
        has(
            &found,
            "tx.limit",
            "could not be redone once undone: the inverse of its inverse would be refused"
        ) && has(&found, "tx.limit", "bytes of work")
            && !has(&found, "tx.limit", "could not be undone"),
        "{found:#?}"
    );
}

/// A small, deterministic generator of pseudo-random numbers
/// (xorshift64*).
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }

    fn between(&mut self, low: i64, high: i64) -> i64 {
        low + i64::try_from(self.below(u64::try_from(high - low + 1).unwrap())).unwrap()
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[usize::try_from(self.below(u64::try_from(items.len()).unwrap())).unwrap()]
    }
}

// An inverse holds, for each file, only the edits since the file was last,
// between operations, as it began or absent, so undoing a step that passes
// through a state it ends or begins in, or one that deletes a file and
// makes it again, gives a shorter inverse than the step's. The check goes
// on until the rounds settle.

#[test]
fn a_step_that_passes_through_its_final_state_settles_on_a_shorter_undo() {
    let tree = copy();
    let root = tree.path();
    let tx = transaction(
        r#"{"op": "set", "path": "defs/proto/tree.jsonc", "pointer": ["custom"], "value": {"example.org/a": 1}},
    {"op": "set", "path": "defs/proto/tree.jsonc", "pointer": ["custom"], "value": {"example.org/a": 2}},
    {"op": "set", "path": "defs/proto/tree.jsonc", "pointer": ["custom"], "value": {"example.org/a": 1}}"#,
    );
    let plan = planned(root, &tx);
    plan.check_undo().unwrap();
    // The undo sets 2, then 1, then removes the member; planning it passes
    // through the state the step ended in, so the redo only adds the
    // member, and the next undo only removes it.
    let rounds = cycled(root, &plan);
    let counts: Vec<usize> = rounds.iter().map(|text| operations(text)).collect();
    assert_eq!(counts, [3, 1, 1, 1]);
    assert_ne!(rounds[2], rounds[0]);
}

#[test]
fn placing_and_then_dragging_an_object_settles_on_a_shorter_undo_and_redo() {
    let tree = copy();
    let root = tree.path();
    let tx = transaction(
        r#"{"op": "object.insert", "plane": "overworld", "layer": 0, "at": [7, 0], "index": 0, "object": {"proto": "wall"}},
    {"op": "object.move", "plane": "overworld", "layer": 0, "at": [7, 0], "index": 0, "expect": {"proto": "wall"}, "to": {"at": [8, 0]}}"#,
    );
    let plan = planned(root, &tx);
    plan.check_undo().unwrap();
    // Moving it passes through the chunk as it began, inside one
    // operation: the undo and the redo hold every edit, but the redo
    // passes through its own start, so the rounds after it hold only the
    // last.
    let rounds = cycled(root, &plan);
    let counts: Vec<usize> = rounds.iter().map(|text| operations(text)).collect();
    assert_eq!(counts, [3, 3, 1, 1, 1]);
    assert_ne!(rounds[2], rounds[0]);
    assert_ne!(rounds[3], rounds[1]);
}

#[test]
fn a_document_deleted_and_made_again_settles_on_a_shorter_undo() {
    let tree = copy();
    let root = tree.path();
    let tx = transaction(
        r#"{"op": "document.delete", "path": "defs/proto/coin.jsonc"},
    {"op": "document.create", "path": "defs/proto/coin.jsonc", "text": "{\"format\": \"tilewright.def/1\", \"type\": \"proto\", \"id\": \"coin\", \"fields\": {\"face\": \"coin.png\", \"layer\": \"item\"}}\n"},
    {"op": "set", "path": "defs/proto/coin.jsonc", "pointer": ["fields", "weight"], "value": 0.5}"#,
    );
    let plan = planned(root, &tx);
    plan.check_undo().unwrap();
    // The undo removes the weight, deletes the file and creates it as it
    // began, passing through no state it ends or begins in. The file is
    // absent in it, though, so its inverse, the redo, holds only what the
    // undo did after that, and the rounds settle on deleting the file and
    // creating it.
    let rounds = cycled(root, &plan);
    assert_ne!(rounds[2], rounds[0]);
}

/// A random transaction of operations that are valid on the fixture, or
/// often are, many of which return a file to a state it was in: values
/// set back and forth, one of them large, which its redo holds; terrain
/// painted and cleared in a few cells, creating and deleting a chunk;
/// objects placed and then moved, or placed and removed again; anchors
/// added and removed; a definition deleted and made again. Others make
/// directories, which their undos must remove: a plane, with chunks or
/// without; a definition type; a layer.
fn revisiting_transaction(random: &mut Random) -> String {
    let coin = {
        let mut text = String::new();
        tilewright_jsonc::write_json_string(&read(&fixture(), "defs/proto/coin.jsonc"), &mut text);
        text
    };
    let pad = "p".repeat(4_000);
    let mut operations = Vec::new();
    for step in 0..random.between(1, 6) {
        let operation = match random.below(14) {
            0 | 1 => {
                let value = random.pick(&["1", "2", "pad"]);
                let value = if value == "pad" { format!("\"{pad}\"") } else { value.to_owned() };
                format!(
                    r#"{{"op": "set", "path": "defs/proto/tree.jsonc", "pointer": ["custom"], "value": {{"example.org/a": {value}}}}}"#
                )
            }
            2 | 3 => {
                let cells: Vec<String> = (0..random.between(1, 3))
                    .map(|offset| {
                        let value = random.pick(&["\"grass\"", "\"sand\"", "\"cave_floor\"", "null"]);
                        format!("{{\"at\": [{}, 3], \"value\": {value}}}", 30 + offset)
                    })
                    .collect();
                format!(
                    r#"{{"op": "terrain.set", "plane": "overworld", "layer": 0, "grid": "floor", "cells": [{}]}}"#,
                    cells.join(", ")
                )
            }
            4 => r#"{"op": "object.insert", "plane": "overworld", "layer": 0, "at": [7, 0], "index": 0, "object": {"proto": "wall"}},
    {"op": "object.move", "plane": "overworld", "layer": 0, "at": [7, 0], "index": 0, "expect": {"proto": "wall"}, "to": {"at": [8, 0]}}"#
                .to_owned(),
            5 => format!(
                r#"{{"op": "object.insert", "plane": "overworld", "layer": 0, "at": [5, 5], "object": {{"uid": "random.{step}", "proto": "tree"}}}},
    {{"op": "object.remove", "uid": "random.{step}", "expect": {{"proto": "tree"}}}}"#
            ),
            6 => format!(
                r#"{{"op": "insert", "path": "planes/overworld/anchors.jsonc", "pointer": ["anchors", 0], "value": {{"id": "random.{step}", "layer": 0, "at": [1, 1]}}}}"#
            ),
            7 => r#"{"op": "remove", "path": "planes/overworld/anchors.jsonc", "pointer": ["anchors", 0]}"#
                .to_owned(),
            8 => format!(
                r#"{{"op": "set", "path": "defs/proto/coin.jsonc", "pointer": ["fields", "weight"], "value": {}}}"#,
                random.below(3)
            ),
            10 => add_plane(&format!("attic{step}")),
            11 => format!(
                r#"{},
    {{"op": "object.insert", "plane": "attic{step}", "layer": {}, "at": [1, 1], "object": {{"proto": "tree"}}}}"#,
                add_plane(&format!("attic{step}")),
                random.between(0, 3)
            ),
            12 => add_definition_type(&format!("quest{step}")),
            13 => format!(
                r#"{{"op": "object.insert", "plane": "overworld", "layer": {}, "at": [1, 1], "object": {{"proto": "tree"}}}}"#,
                random.between(5, 8)
            ),
            _ => format!(
                r#"{{"op": "document.delete", "path": "defs/proto/coin.jsonc"}},
    {{"op": "document.create", "path": "defs/proto/coin.jsonc", "text": {coin}}}"#
            ),
        };
        operations.push(operation);
    }
    operations.join(",\n    ")
}

/// Plans six rounds after publishing `plan`, each the inverse of the one
/// before, publishing each that plans: each undo restores every file and
/// directory, and each redo gives every one the plan did. Returns the
/// transactions of the rounds that planned, and the round that did not
/// with its problems.
fn rounds(root: &Path, plan: &Plan) -> (Vec<String>, Option<(usize, Vec<Diagnostic>)>) {
    let before = (snapshot(root), directories(root));
    published(plan);
    let after = (snapshot(root), directories(root));
    let mut text = plan.inverse().to_owned();
    let mut done = Vec::new();
    for round in 0..6 {
        match crate::plan(root, &reparsed("round.jsonc", &text), &Options::default()) {
            Ok(planned) => {
                published(&planned);
                let expected = if round % 2 == 0 { &before } else { &after };
                assert!(
                    (snapshot(root), directories(root)) == *expected,
                    "round {round}"
                );
                done.push(std::mem::replace(&mut text, planned.inverse().to_owned()));
            }
            Err(problems) => return (done, Some((round, problems))),
        }
    }
    (done, None)
}

#[test]
fn the_check_passes_exactly_when_every_undo_and_redo_plans() {
    let mut random = Random(0x7e57_ab1e_5eed_0002);
    // Transactions refused themselves, checks passed, checks refused, and
    // passed transactions whose undo is not the third round's.
    let mut outcomes = [0_u32; 4];
    for case in 0..160 {
        let tree = copy();
        let root = tree.path();
        let text = revisiting_transaction(&mut random);
        let tx = transaction(&text);
        // Budgets around what such transactions read and do, or the
        // defaults.
        let _lowered = (random.below(4) != 0)
            .then(|| Lowered::to(random.below(24 << 10), random.below(160 << 10)));
        let Ok(plan) = crate::plan(root, &tx, &Options::default()) else {
            outcomes[0] += 1;
            continue;
        };
        let checked = plan.check_undo();
        let (done, failed) = rounds(root, &plan);
        match (checked, failed) {
            (Ok(()), None) => {
                outcomes[1] += 1;
                // Settled by the third round: each is the one two before.
                assert!(
                    done[4] == done[2] && done[5] == done[3],
                    "case {case}:\n{text}"
                );
                if done[2] != done[0] {
                    outcomes[3] += 1;
                }
            }
            (Err(found), Some((round, failed))) => {
                outcomes[2] += 1;
                // The check refused the round that fails, for its reason.
                for problem in &failed {
                    assert!(
                        gives(&found, ROUNDS[round], problem),
                        "case {case}:\n{text}\n{found:#?}\n{failed:#?}"
                    );
                }
            }
            (checked, failed) => {
                panic!("case {case}:\n{text}\n{checked:#?}\n{failed:#?}")
            }
        }
    }
    // Each kind of outcome happens. Refusals by the check are few here;
    // the next test finds them at the boundary.
    assert!(
        outcomes[0] > 10 && outcomes[1] > 40 && outcomes[2] > 0 && outcomes[3] > 10,
        "{outcomes:?}"
    );
}

#[test]
fn at_the_least_budget_the_check_accepts_every_undo_and_redo_plans() {
    let mut random = Random(0x0b0e_dace_5eed_0003);
    // Which round of the check bound the budget: the undo, the redo, or a
    // later one; or the transaction itself.
    let mut bound = [0_u32; 4];
    for case in 0..24 {
        let text = revisiting_transaction(&mut random);
        let tx = transaction(&text);
        for budget in [Budget::Read, Budget::Work] {
            let probe = copy();
            let passes = |value| {
                let _lowered = lowered(budget, value);
                crate::plan(probe.path(), &tx, &Options::default())
                    .is_ok_and(|plan| plan.check_undo().is_ok())
            };
            let (mut low, mut high) = (0, 1 << 19);
            if passes(low) || !passes(high) {
                continue;
            }
            while high - low > 1 {
                let middle = low + (high - low) / 2;
                if passes(middle) {
                    high = middle;
                } else {
                    low = middle;
                }
            }
            // At the least budget, every round plans.
            let at = copy();
            {
                let _lowered = lowered(budget, high);
                let plan = planned(at.path(), &tx);
                plan.check_undo().unwrap();
                let (_, failed) = rounds(at.path(), &plan);
                assert!(failed.is_none(), "case {case}:\n{text}\n{failed:#?}");
            }
            // One below, the transaction is refused, or the check refuses
            // it and that round fails.
            let below = copy();
            let _lowered = lowered(budget, low);
            let Ok(plan) = crate::plan(below.path(), &tx, &Options::default()) else {
                bound[3] += 1;
                continue;
            };
            let found = plan.check_undo().unwrap_err();
            let (_, failed) = rounds(below.path(), &plan);
            let Some((round, failed)) = failed else {
                panic!("case {case}:\n{text}\n{found:#?}");
            };
            bound[round.min(2)] += 1;
            for problem in &failed {
                assert!(
                    gives(&found, ROUNDS[round], problem),
                    "case {case}:\n{text}\n{found:#?}\n{failed:#?}"
                );
            }
        }
    }
    // The undo bound some budgets and the redo others. (Later rounds do
    // not: each holds no more than the one two before it.)
    assert!(bound[0] > 3 && bound[1] > 3, "{bound:?}");
}

#[test]
fn the_note_that_there_may_be_more_stays_last_and_as_it_is() {
    let tree = copy();
    let plan = planned(
        tree.path(),
        &transaction(
            r#"{"op": "set", "path": "defs/proto/tree.jsonc", "pointer": ["custom"], "value": {"example.org/a": 1}}"#,
        ),
    );
    let note = more_note("tx.jsonc");
    let found = plan.refused(
        "the transaction could not be undone",
        vec![
            diagnostic::file("tx.jsonc", "tx.limit", "a limit"),
            diagnostic::file("tx.jsonc", "tx.conflict", "a conflict"),
            note.clone(),
        ],
    );
    assert_eq!(found.last(), Some(&note));
    assert_eq!(
        found[..2]
            .iter()
            .map(|problem| problem.message.as_str())
            .collect::<Vec<_>>(),
        [
            "the transaction could not be undone: a conflict",
            "the transaction could not be undone: a limit; split the transaction",
        ]
    );
}

/// Operations that add the plane `id`.
fn add_plane(id: &str) -> String {
    format!(
        r#"{{"op": "insert", "path": "world.jsonc", "pointer": ["planes", 2], "value": "{id}"}},
    {{"op": "document.create", "path": "planes/{id}/plane.jsonc", "document": {{"format": "tilewright.plane/1", "id": "{id}", "title": "A plane", "kind": "shared", "chunk_size": 32}}}}"#
    )
}

/// Operations that add the definition type `id`, with a definition.
fn add_definition_type(id: &str) -> String {
    format!(
        r#"{{"op": "document.create", "path": "profile/schema/{id}.jsonc", "document": {{"format": "tilewright.schema/1", "fields": {{"title": {{"type": "string"}}}}}}}},
    {{"op": "insert", "path": "profile/profile.jsonc", "pointer": ["definition_types", 3], "key": "{id}", "value": {{"schema": "schema/{id}.jsonc"}}}},
    {{"op": "document.create", "path": "defs/{id}/hello.jsonc", "document": {{"format": "tilewright.def/1", "type": "{id}", "id": "hello", "fields": {{"title": "Hello"}}}}}}"#
    )
}

/// Applies `operations`, which pass the check, then undoes and redoes them:
/// the files are as they were, and so are the directories, and the source
/// digest of the tree undone is the one it had.
fn undone_with_its_directories(operations: &str) {
    let tree = copy();
    let root = tree.path();
    let digest = tilewright_source::load(root).unwrap().source_digest();
    let folders = directories(root);
    let tx = transaction(operations);
    let plan = planned(root, &tx);
    plan.check_undo().unwrap();
    let undo = {
        published(&plan);
        apply(root, &reparsed("undo.jsonc", plan.inverse()))
    };
    assert_eq!(directories(root), folders);
    assert_eq!(
        tilewright_source::load(root).unwrap().source_digest(),
        digest
    );
    let redo = apply(root, &reparsed("redo.jsonc", undo.inverse()));
    apply(root, &reparsed("undo.jsonc", redo.inverse()));
    assert_eq!(directories(root), folders);
}

#[test]
fn a_step_that_adds_a_plane_is_undone() {
    undone_with_its_directories(&add_plane("attic"));
}

#[test]
fn a_step_that_adds_a_plane_with_chunks_is_undone() {
    undone_with_its_directories(&format!(
        r#"{},
    {{"op": "object.insert", "plane": "attic", "layer": 0, "at": [1, 1], "object": {{"proto": "tree"}}}},
    {{"op": "object.insert", "plane": "attic", "layer": 3, "at": [70, 1], "object": {{"proto": "tree"}}}}"#,
        add_plane("attic")
    ));
}

#[test]
fn a_step_that_adds_a_definition_type_is_undone() {
    undone_with_its_directories(&add_definition_type("quest"));
}

#[test]
fn a_step_that_adds_a_layer_is_undone() {
    undone_with_its_directories(
        r#"{"op": "object.insert", "plane": "overworld", "layer": 9, "at": [1, 1], "object": {"proto": "tree"}}"#,
    );
}

/// A file of the tree as `plan` found it.
fn as_it_was<'a>(plan: &'a Plan, path: &str) -> &'a str {
    plan.files()
        .iter()
        .find(|file| file.path == path)
        .and_then(|file| file.before.as_deref())
        .unwrap()
}

/// Repairs the tree at `root` with `plan`'s inverse, and checks that it is
/// as it was: the undo passes the check and plans, as the inverse an
/// interrupted apply left must, and every file and directory is back.
fn repaired(root: &Path, plan: &Plan, before: &BTreeMap<String, Vec<u8>>, folders: &[String]) {
    let undo = planned(root, &reparsed("undo.jsonc", plan.inverse()));
    undo.check_undo().unwrap();
    published(&undo);
    assert_eq!(snapshot(root), *before);
    assert_eq!(directories(root), folders);
}

#[test]
fn an_apply_of_a_new_plane_stopped_after_a_rename_is_repaired() {
    let tree = copy();
    let root = tree.path();
    let (before, folders) = (snapshot(root), directories(root));
    let plan = planned(root, &transaction(&add_plane("attic")));
    published(&plan);
    // The plane's file came first; `world.jsonc` was staged, not published.
    fs::write(root.join("world.jsonc"), as_it_was(&plan, "world.jsonc")).unwrap();
    fs::write(root.join(staging_path("world.jsonc")), "staged").unwrap();
    assert!(tilewright_source::load(root).is_err());
    repaired(root, &plan, &before, &folders);
}

#[test]
fn an_apply_of_a_new_plane_stopped_before_a_rename_is_repaired() {
    let tree = copy();
    let root = tree.path();
    let (before, folders) = (snapshot(root), directories(root));
    let plan = planned(root, &transaction(&add_plane("attic")));
    published(&plan);
    // Nothing was published: all there is of the plane is its staged file,
    // in the directory staging made.
    fs::write(root.join("world.jsonc"), as_it_was(&plan, "world.jsonc")).unwrap();
    fs::remove_file(root.join("planes/attic/plane.jsonc")).unwrap();
    fs::write(
        root.join(staging_path("planes/attic/plane.jsonc")),
        "staged",
    )
    .unwrap();
    assert!(tilewright_source::load(root).is_err());
    repaired(root, &plan, &before, &folders);
}

#[test]
fn an_apply_of_a_new_definition_type_stopped_after_a_rename_is_repaired() {
    let tree = copy();
    let root = tree.path();
    let (before, folders) = (snapshot(root), directories(root));
    let plan = planned(root, &transaction(&add_definition_type("quest")));
    published(&plan);
    // The definition came first in path order.
    let profile = "profile/profile.jsonc";
    fs::write(root.join(profile), as_it_was(&plan, profile)).unwrap();
    fs::remove_file(root.join("profile/schema/quest.jsonc")).unwrap();
    assert!(tilewright_source::load(root).is_err());
    repaired(root, &plan, &before, &folders);
}

#[test]
fn a_plane_can_be_removed_and_made_again() {
    let tree = copy();
    let root = tree.path();
    apply(root, &transaction(&add_plane("attic")));
    apply(
        root,
        &transaction(
            r#"{"op": "object.insert", "plane": "attic", "layer": 2, "at": [1, 1], "object": {"proto": "tree"}}"#,
        ),
    );
    let tx = transaction(
        r#"{"op": "remove", "path": "world.jsonc", "pointer": ["planes", 2]},
    {"op": "document.delete", "path": "planes/attic/plane.jsonc"},
    {"op": "document.delete", "path": "planes/attic/layers/2/0_0.jsonc"}"#,
    );
    round_trip(root, &tx);
}

/// Whether every directory under `root` that `before` does not have holds
/// nothing but such directories, as an undo may leave them.
fn only_empty_directories_added(root: &Path, before: &[String]) -> bool {
    directories(root)
        .iter()
        .filter(|directory| !before.contains(directory))
        .all(|directory| {
            fs::read_dir(root.join(directory))
                .unwrap()
                .all(|entry| entry.unwrap().file_type().unwrap().is_dir())
        })
}

#[cfg(unix)]
#[test]
fn a_directory_an_undo_leaves_is_ignored() {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o7777;
    // Publishing leaves a directory whose mode or group is not that of the
    // one above it, whichever changed. It holds no file, so the tree is the
    // one the step was made on, and the undo plans and publishes.
    for changed in ["planes/attic", "planes"] {
        let tree = copy();
        let root = tree.path();
        let (before, folders) = (snapshot(root), directories(root));
        let digest = tilewright_source::load(root).unwrap().source_digest();
        let plan = planned(root, &transaction(&add_plane("attic")));
        plan.check_undo().unwrap();
        published(&plan);
        let bits = mode(&root.join(changed));
        fs::set_permissions(root.join(changed), fs::Permissions::from_mode(bits ^ 0o020)).unwrap();
        apply(root, &reparsed("undo.jsonc", plan.inverse()));
        assert!(root.join("planes/attic").is_dir(), "{changed}");
        assert_eq!(snapshot(root), before);
        assert!(only_empty_directories_added(root, &folders));
        assert_eq!(
            tilewright_source::load(root).unwrap().source_digest(),
            digest
        );
    }
}

#[test]
fn deleting_a_large_file_and_making_it_again_copies_it_no_more() {
    const PAD: &str = "defs/proto/pad.jsonc";
    let tree = copy();
    let root = tree.path();
    let text = format!(
        "{{\"format\": \"tilewright.def/1\", \"type\": \"proto\", \"id\": \"pad\", \"fields\": {{\"face\": \"p.png\", \"layer\": \"item\"}}, \"custom\": {{\"example.org/pad\": \"{}\"}}}}\n",
        "x".repeat(256 << 10)
    );
    fs::write(root.join(PAD), &text).unwrap();
    let delete = format!(r#"{{"op": "document.delete", "path": "{PAD}"}}"#);
    let create = format!(
        r#"{{"op": "document.create", "path": "{PAD}", "text": "{{\"format\": \"tilewright.def/1\", \"type\": \"proto\", \"id\": \"pad\", \"fields\": {{\"face\": \"q.png\", \"layer\": \"item\"}}}}"}}"#
    );
    let mut operations = vec![delete.clone()];
    for _ in 0..1_000 {
        operations.push(create.clone());
        operations.push(delete.clone());
    }
    operations.push(create.clone());
    let tx = transaction(&operations.join(",\n    "));
    // Each pair is charged its own small size and nothing for the file.
    let size = u64::try_from(text.len()).unwrap();
    let _lowered = Lowered::to(limits::READ_BYTES, size + (1 << 20));

    // No step of the inverse holds a copy of the file, though each
    // deletion dropped the steps before it, down to one creation of it.
    let source = SourceRoot::new(root);
    let on_disk = Overlay::new();
    let workspace = carry_out(&source, &on_disk, Names::OWN, &tx, &Options::default()).unwrap();
    let steps = &workspace.entries[PAD].inverse;
    assert!(matches!(steps[0], Primitive::CreateBase), "{steps:?}");
    assert!(
        steps
            .iter()
            .all(|step| !matches!(step, Primitive::Create { text } if text.len() > 1 << 10)),
        "{}",
        steps.len()
    );
    drop(workspace);

    // Its inverse still deletes the file and creates the text it began
    // with, and writing it copied that once.
    let plan = planned(root, &tx);
    plan.check_undo().unwrap();
    published(&plan);
    apply(root, &reparsed("undo.jsonc", plan.inverse()));
    assert_eq!(read(root, PAD), text);
}

/// The text of the definition `id`, with `pad` bytes of custom data.
fn padded(id: &str, pad: usize) -> String {
    format!(
        "{{\"format\": \"tilewright.def/1\", \"type\": \"proto\", \"id\": \"{id}\", \"fields\": {{\"face\": \"p.png\", \"layer\": \"item\"}}, \"custom\": {{\"example.org/pad\": \"{}\"}}}}\n",
        "x".repeat(pad)
    )
}

/// An operation that gives the definition `id` `pad` bytes of custom data.
fn set_pad(id: &str, pad: usize) -> String {
    format!(
        r#"{{"op": "set", "path": "defs/proto/{id}.jsonc", "pointer": ["custom", "example.org/pad"], "value": "{}"}}"#,
        "x".repeat(pad)
    )
}

/// The fixture with `aa` small and `zz` large, and the step that swaps
/// their sizes, so that each whole tree holds as much, but the first file
/// after it and the last before it hold more than that together. With
/// the bytes each tree holds in the two files.
fn swapping_sizes() -> (tempfile::TempDir, Transaction, u64) {
    let (small, large) = (1 << 10, 9 << 10);
    let tree = copy();
    fs::write(tree.path().join("defs/proto/aa.jsonc"), padded("aa", small)).unwrap();
    fs::write(tree.path().join("defs/proto/zz.jsonc"), padded("zz", large)).unwrap();
    let tx = transaction(&format!(
        "{},\n    {}",
        set_pad("aa", large),
        set_pad("zz", small)
    ));
    let whole = u64::try_from(padded("aa", small).len() + padded("zz", large).len()).unwrap();
    (tree, tx, whole)
}

#[test]
fn a_repair_reads_a_mix_of_the_trees_when_each_tree_is_within_the_budget() {
    const LAST: &str = "defs/proto/zz.jsonc";
    let (tree, tx, whole) = swapping_sizes();
    let root = tree.path();
    let (before, folders) = (snapshot(root), directories(root));
    let _lowered = Lowered::to(whole, limits::WORK);
    let plan = planned(root, &tx);
    plan.check_undo().unwrap();
    published(&plan);
    // It stopped after the first rename, so the last file, read last, is
    // as it was. Only by reading it can the repair tell that it is done.
    fs::write(root.join(LAST), as_it_was(&plan, LAST)).unwrap();
    let held: u64 = ["defs/proto/aa.jsonc", LAST]
        .iter()
        .map(|path| fs::metadata(root.join(path)).unwrap().len())
        .sum();
    assert!(held > whole && held <= 2 * whole, "{held} {whole}");
    repaired(root, &plan, &before, &folders);
}

#[test]
fn applying_again_reads_a_mix_of_the_trees_too() {
    const LAST: &str = "defs/proto/zz.jsonc";
    let (tree, tx, whole) = swapping_sizes();
    let root = tree.path();
    let _lowered = Lowered::to(whole, limits::WORK);
    let plan = planned(root, &tx);
    let done = {
        published(&plan);
        snapshot(root)
    };
    fs::write(root.join(LAST), as_it_was(&plan, LAST)).unwrap();
    // The first file is at its result, and read first; the last is edited.
    apply(root, &reparsed("sealed.jsonc", plan.sealed()));
    assert_eq!(snapshot(root), done);
}

/// A transaction with no operations that finds the definitions `ids`
/// already at their result.
fn all_done(root: &Path, ids: &[&str]) -> Transaction {
    let entries = |digest: &dyn Fn(&str) -> String| -> String {
        ids.iter()
            .map(|id| {
                format!(
                    "{{\"path\": \"defs/proto/{id}.jsonc\", \"sha256\": \"{}\"}}",
                    digest(id)
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let other = transaction::hex(&[0; 32]);
    let text = format!(
        "{{\"format\": \"tilewright.transaction/1\", \"base\": [{}], \"result\": [{}], \"operations\": []}}\n",
        entries(&|_| other.clone()),
        entries(&|id| {
            let text = read(root, &format!("defs/proto/{id}.jsonc"));
            transaction::hex(&digest(Some(&text)).unwrap())
        })
    );
    reparsed("tx.jsonc", &text)
}

#[test]
fn files_at_their_result_have_an_allowance_of_the_budget_and_no_more() {
    let tree = copy();
    let root = tree.path();
    let ids = ["a1", "a2", "a3", "a4"];
    for id in ids {
        fs::write(
            root.join(format!("defs/proto/{id}.jsonc")),
            padded(id, 1 << 10),
        )
        .unwrap();
    }
    let size = u64::try_from(padded("a1", 1 << 10).len()).unwrap();
    // Each file is read before it is known to be done, so these are all
    // read, and within the allowance.
    {
        let _lowered = Lowered::to(2 * size, limits::WORK);
        planned(root, &all_done(root, &ids[..2]));
    }
    // A transaction may claim that of every file, so that allowance has
    // to end too: this one stops at the third.
    let _lowered = Lowered::to(2 * size, limits::WORK);
    let found = problems(root, &all_done(root, &ids));
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(has(&found, "tx.limit", "hold more than"), "{found:#?}");
}

#[test]
fn files_at_their_base_have_the_budget_whether_or_not_it_can_resume() {
    let (tree, tx, whole) = swapping_sizes();
    let root = tree.path();
    let sealed = reparsed("sealed.jsonc", planned(root, &tx).sealed());
    let _lowered = Lowered::to(whole - 1, limits::WORK);
    // It can resume, so it reads past the budget to see, and is refused.
    assert!(has(&problems(root, &sealed), "tx.limit", "hold more than"));
    // It cannot: it is refused too. How much each reads is the next
    // test's.
    let tests = transaction(
        r#"{"op": "test", "path": "defs/proto/aa.jsonc", "pointer": ["id"], "value": "aa"},
    {"op": "test", "path": "defs/proto/zz.jsonc", "pointer": ["id"], "value": "zz"}"#,
    );
    let sealed = {
        let _raised = Lowered::to(limits::READ_BYTES, limits::WORK);
        reparsed("sealed.jsonc", planned(root, &tests).sealed())
    };
    assert!(has(&problems(root, &sealed), "tx.limit", "hold more than"));
}

#[test]
fn a_repair_of_an_invalid_tree_passes_the_check_and_its_undo_is_refused() {
    const TREE: &str = "defs/proto/tree.jsonc";
    let tree = copy();
    let root = tree.path();
    // A field its schema does not have, so the tree does not validate.
    let broken = read(root, TREE).replace("\"blocks_view\": true", "\"nonsense\": true");
    fs::write(root.join(TREE), broken).unwrap();
    assert!(tilewright_source::load(root).is_err());
    let tx = transaction(
        r#"{"op": "remove", "path": "defs/proto/tree.jsonc", "pointer": ["fields", "nonsense"]}"#,
    );
    let plan = planned(root, &tx);
    // The check does not validate the tree an undo leaves, so that a
    // transaction can repair a tree: it would refuse every repair.
    plan.check_undo().unwrap();
    published(&plan);
    assert!(tilewright_source::load(root).is_ok());
    // The undo recreates the invalid tree, and planning it does validate.
    let found = problems(root, &reparsed("undo.jsonc", plan.inverse()));
    assert!(has(&found, "shape.unknown_key", "nonsense"), "{found:#?}");
}

#[test]
fn an_interrupted_undo_of_a_new_plane_can_be_applied_again() {
    let tree = copy();
    let root = tree.path();
    let (before, folders) = (snapshot(root), directories(root));
    let plan = apply(
        root,
        &transaction(&format!(
            r#"{},
    {{"op": "object.insert", "plane": "attic", "layer": 0, "at": [1, 1], "object": {{"proto": "tree"}}}},
    {{"op": "object.insert", "plane": "attic", "layer": 3, "at": [70, 1], "object": {{"proto": "tree"}}}}"#,
            add_plane("attic")
        )),
    );
    let undo = reparsed("undo.jsonc", plan.inverse());
    let undoing = planned(root, &undo);
    // It stopped in step 4: `world.jsonc` was published, and the first
    // file in path order deleted, which left its layer's directory empty.
    let world = undoing
        .files()
        .iter()
        .find(|file| file.path == "world.jsonc");
    fs::write(
        root.join("world.jsonc"),
        world.unwrap().after.as_ref().unwrap(),
    )
    .unwrap();
    fs::remove_file(root.join("planes/attic/layers/0/0_0.jsonc")).unwrap();
    // Applied again, it finds those files done and does the rest.
    let again = planned(root, &undo);
    again.check_undo().unwrap();
    published(&again);
    assert_eq!(snapshot(root), before);
    assert!(only_empty_directories_added(root, &folders));
    assert!(tilewright_source::load(root).is_ok());
}

#[test]
fn an_interrupted_deletion_of_a_plane_can_be_applied_again() {
    let tree = copy();
    let root = tree.path();
    let (before, folders) = (snapshot(root), directories(root));
    apply(root, &transaction(&add_plane("attic")));
    apply(
        root,
        &transaction(
            r#"{"op": "object.insert", "plane": "attic", "layer": 2, "at": [1, 1], "object": {"proto": "tree"}}"#,
        ),
    );
    let plan = planned(
        root,
        &transaction(
            r#"{"op": "remove", "path": "world.jsonc", "pointer": ["planes", 2]},
    {"op": "document.delete", "path": "planes/attic/plane.jsonc"},
    {"op": "document.delete", "path": "planes/attic/layers/2/0_0.jsonc"}"#,
        ),
    );
    let sealed = reparsed("sealed.jsonc", plan.sealed());
    // It stopped in step 4, after `world.jsonc` and the chunk, the first
    // file deleted in path order.
    let world = plan.files().iter().find(|file| file.path == "world.jsonc");
    fs::write(
        root.join("world.jsonc"),
        world.unwrap().after.as_ref().unwrap(),
    )
    .unwrap();
    fs::remove_file(root.join("planes/attic/layers/2/0_0.jsonc")).unwrap();
    let again = planned(root, &sealed);
    again.check_undo().unwrap();
    published(&again);
    assert_eq!(snapshot(root), before);
    assert!(only_empty_directories_added(root, &folders));
    assert!(tilewright_source::load(root).is_ok());
}

#[test]
fn an_undo_plans_within_the_entry_budget_its_step_did() {
    let fits = |root: &Path, maximum_entries| {
        let options = LoadOptions {
            maximum_entries,
            ..LoadOptions::default()
        };
        tilewright_source::load_with(root, &options).is_ok()
    };
    let (mut low, mut high) = (0, 4_096);
    while high - low > 1 {
        let middle = low + (high - low) / 2;
        if fits(&fixture(), middle) {
            high = middle;
        } else {
            low = middle;
        }
    }
    // The least entry budget the tree loads with, for the step, its undo
    // and its redo alike.
    let options = Options {
        load: LoadOptions {
            maximum_entries: high,
            ..LoadOptions::default()
        },
        ..Options::default()
    };
    let tree = copy();
    let root = tree.path();
    let tx = transaction(
        r#"{"op": "document.create", "path": "profile/notes/a.jsonc", "document": {"note": 1}},
    {"op": "document.create", "path": "profile/notes/b.jsonc", "document": {"note": 2}}"#,
    );
    let plan = crate::plan(root, &tx, &options).unwrap();
    plan.check_undo().unwrap();
    published(&plan);
    let undo = crate::plan(root, &reparsed("undo.jsonc", plan.inverse()), &options).unwrap();
    published(&undo);
    crate::plan(root, &reparsed("redo.jsonc", undo.inverse()), &options).unwrap();
}

#[test]
fn reading_goes_past_the_budget_only_for_a_base_that_can_resume() {
    let (tree, tx, whole) = swapping_sizes();
    let root = tree.path();
    let tests = transaction(
        r#"{"op": "test", "path": "defs/proto/aa.jsonc", "pointer": ["id"], "value": "aa"},
    {"op": "test", "path": "defs/proto/zz.jsonc", "pointer": ["id"], "value": "zz"}"#,
    );
    let cannot = reparsed("tests.jsonc", planned(root, &tests).sealed());
    let can = reparsed("sealed.jsonc", planned(root, &tx).sealed());
    let budget = whole - 1;
    let _lowered = Lowered::to(budget, limits::WORK);
    let source = SourceRoot::new(root);
    let on_disk = Overlay::new();
    let checked = |transaction: &Transaction| {
        let mut workspace = Workspace::new(&source, &on_disk, Names::OWN);
        let mut problems = Problems::new(transaction);
        check_base(&mut workspace, transaction, &mut problems);
        assert!(!problems.is_empty());
        workspace
    };
    // One that cannot resume stops reading at the budget.
    assert!(checked(&cannot).bytes_read() <= budget);
    // One that can reads on, to see which files are done, up to twice
    // the budget...
    let mut workspace = checked(&can);
    assert!(workspace.bytes_read() > budget && workspace.bytes_read() <= 2 * budget);
    // ...but only while its base is checked: what it reads next has the
    // one budget left, which is spent.
    assert!(workspace.exists("defs/proto/tree.jsonc").is_err());
}
