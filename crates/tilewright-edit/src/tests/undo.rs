// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! The undo check, [`Plan::check_undo`]: a plan whose inverse could not be
//! carried out on the tree it leaves, or whose inverse's own inverse could
//! not be carried out on the tree that leaves, is refused before anything
//! is written, and one it passes can be undone and redone.

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
    if let Err(error) = plan.publish() {
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
/// its undo and its redo: the undo restores every file, the redo gives
/// every file the plan did, and the redo's inverse is the undo again, byte
/// for byte. Returns the undo.
fn undone_and_redone(root: &Path, plan: &Plan) -> Plan {
    let before = snapshot(root);
    published(plan);
    let after = snapshot(root);
    let undo = apply(root, &reparsed("undo.jsonc", plan.inverse()));
    assert_eq!(snapshot(root), before);
    let redo = apply(root, &reparsed("redo.jsonc", undo.inverse()));
    assert_eq!(snapshot(root), after);
    assert_eq!(redo.inverse(), plan.inverse());
    undo
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
    let undo = undone_and_redone(root, &plan);
    let lines = undo.inverse().lines().count();
    assert!((249_000..=250_000).contains(&lines), "{lines}");
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
            Err(failed) => ("the transaction could not be undone", failed),
            Ok(undone) => {
                published(&undone);
                let redo = reparsed("redo.jsonc", undone.inverse());
                (
                    "the transaction could not be redone once undone",
                    problems(root, &redo),
                )
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
    undone_and_redone(at.path(), &plan);
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

/// A random transaction of operations that are valid on the fixture:
/// objects placed, and placed and removed again; terrain painted and
/// cleared, which creates and deletes chunks; definitions created, renamed
/// and grown; anchors added.
fn random_transaction(random: &mut Random) -> String {
    let mut operations = Vec::new();
    for step in 0..random.between(1, 6) {
        let (x, y) = (random.between(-40, 70), random.between(0, 40));
        let layer = random.between(0, 1);
        let operation = match random.below(8) {
            0 => format!(
                r#"{{"op": "object.insert", "plane": "overworld", "layer": {layer}, "at": [{x}, {y}], "object": {{"proto": "{}"}}}}"#,
                random.pick(&["tree", "wall", "coin"])
            ),
            1 => format!(
                r#"{{"op": "object.insert", "plane": "overworld", "layer": {layer}, "at": [{x}, {y}], "object": {{"uid": "random.{step}", "proto": "tree"}}}},
    {{"op": "object.remove", "uid": "random.{step}", "expect": {{"proto": "tree"}}}}"#
            ),
            2 | 3 => {
                let grid = if random.below(3) == 0 {
                    "height"
                } else {
                    "floor"
                };
                let mut seen = BTreeSet::new();
                let mut cells = Vec::new();
                for _ in 0..random.between(1, 40) {
                    let at = (x + random.between(0, 40), y + random.between(0, 8));
                    if !seen.insert(at) {
                        continue;
                    }
                    let value = if grid == "height" {
                        random.between(-1, 2).to_string()
                    } else {
                        let value = random.pick(&["grass", "sand", "cave_floor", "null"]);
                        if value == "null" {
                            value.to_owned()
                        } else {
                            format!("\"{value}\"")
                        }
                    };
                    cells.push(format!(
                        "{{\"at\": [{}, {}], \"value\": {value}}}",
                        at.0, at.1
                    ));
                }
                format!(
                    r#"{{"op": "terrain.set", "plane": "overworld", "layer": {layer}, "grid": "{grid}", "cells": [{}]}}"#,
                    cells.join(", ")
                )
            }
            4 => format!(
                r#"{{"op": "document.create", "path": "defs/proto/random{step}.jsonc", "text": "{{\"format\": \"tilewright.def/1\", \"type\": \"proto\", \"id\": \"random{step}\", \"fields\": {{\"face\": \"{}\", \"layer\": \"item\"}}}}\n"}}"#,
                "f".repeat(usize::try_from(random.below(120)).unwrap() + 1)
            ),
            5 => format!(
                r#"{{"op": "document.create", "path": "defs/proto/moved{step}.jsonc", "document": {{"format": "tilewright.def/1", "type": "proto", "id": "moved{step}", "fields": {{"face": "m.png", "layer": "item"}}}}}},
    {{"op": "document.rename", "path": "defs/proto/moved{step}.jsonc", "to": "defs/proto/renamed{step}.jsonc"}}"#
            ),
            6 => format!(
                r#"{{"op": "set", "path": "defs/proto/tree.jsonc", "pointer": ["custom"], "value": {{"example.org/pad": "{}"}}}}"#,
                "p".repeat(usize::try_from(random.below(6_000)).unwrap())
            ),
            _ => format!(
                r#"{{"op": "insert", "path": "planes/overworld/anchors.jsonc", "pointer": ["anchors", 0], "value": {{"id": "random.{step}", "layer": 0, "at": [{}, {}]}}}}"#,
                x.rem_euclid(32),
                y.rem_euclid(32)
            ),
        };
        operations.push(operation);
    }
    operations.join(",\n    ")
}

#[test]
fn the_check_passes_exactly_when_the_undo_and_the_redo_plan() {
    let mut random = Random(0x7e57_ab1e_5eed_0001);
    // Transactions refused themselves, checks passed, checks refused.
    let mut outcomes = [0_u32; 3];
    for case in 0..160 {
        let tree = copy();
        let root = tree.path();
        let text = random_transaction(&mut random);
        let tx = transaction(&text);
        // Budgets around what such transactions read and do, or the
        // defaults.
        let lowered = (random.below(4) != 0)
            .then(|| Lowered::to(random.below(24 << 10), random.below(160 << 10)));
        let Ok(plan) = crate::plan(root, &tx, &Options::default()) else {
            outcomes[0] += 1;
            continue;
        };
        let checked = plan.check_undo();
        outcomes[if checked.is_ok() { 1 } else { 2 }] += 1;
        published(&plan);
        let undo = reparsed("undo.jsonc", plan.inverse());
        let actual = crate::plan(root, &undo, &Options::default()).and_then(|undone| {
            published(&undone);
            let redo = reparsed("redo.jsonc", undone.inverse());
            crate::plan(root, &redo, &Options::default())
        });
        assert_eq!(
            checked.is_ok(),
            actual.is_ok(),
            "case {case}:\n{text}\n{checked:#?}\n{actual:#?}"
        );
        match (checked, actual) {
            (Ok(()), Ok(redone)) => {
                // The redo's inverse is the undo again, and the redo gives
                // what the transaction did.
                assert_eq!(redone.inverse(), plan.inverse(), "case {case}");
                published(&redone);
                for file in plan.files() {
                    assert_eq!(
                        fs::read_to_string(root.join(&file.path)).ok(),
                        file.after,
                        "case {case}: {}",
                        file.path
                    );
                }
            }
            (Err(found), Err(failed)) => {
                for problem in &failed {
                    assert!(
                        found.iter().any(|check| check.code == problem.code),
                        "case {case}:\n{found:#?}\n{failed:#?}"
                    );
                }
            }
            _ => unreachable!(),
        }
        drop(lowered);
    }
    // Each kind of outcome happens, the refusals in some number.
    assert!(
        outcomes[0] > 10 && outcomes[1] > 40 && outcomes[2] > 10,
        "{outcomes:?}"
    );
}
