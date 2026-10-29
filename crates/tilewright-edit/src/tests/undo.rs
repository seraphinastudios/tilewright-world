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

// An inverse holds, for each file, only the edits since the file was last
// as it began, so undoing a step that passes through a state it ends or
// begins in gives a shorter inverse than the step's. The check goes on
// until the rounds settle.

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
    let rounds = cycled(root, &plan);
    assert_ne!(rounds[2], rounds[0]);
}

/// A random transaction of operations that are valid on the fixture, or
/// often are, many of which return a file to a state it was in: values
/// set back and forth, one of them large, which its redo holds; terrain
/// painted and cleared in a few cells, creating and deleting a chunk;
/// objects placed and then moved, or placed and removed again; anchors
/// added and removed; a definition deleted and made again.
fn revisiting_transaction(random: &mut Random) -> String {
    let coin = {
        let mut text = String::new();
        tilewright_jsonc::write_json_string(&read(&fixture(), "defs/proto/coin.jsonc"), &mut text);
        text
    };
    let pad = "p".repeat(4_000);
    let mut operations = Vec::new();
    for step in 0..random.between(1, 6) {
        let operation = match random.below(10) {
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
/// before, publishing each that plans. Returns the transactions of the
/// rounds that planned, and the round that did not with its problems.
fn rounds(root: &Path, plan: &Plan) -> (Vec<String>, Option<(usize, Vec<Diagnostic>)>) {
    let before = snapshot(root);
    published(plan);
    let after = snapshot(root);
    let mut text = plan.inverse().to_owned();
    let mut done = Vec::new();
    for round in 0..6 {
        match crate::plan(root, &reparsed("round.jsonc", &text), &Options::default()) {
            Ok(planned) => {
                published(&planned);
                let expected = if round % 2 == 0 { &before } else { &after };
                assert!(snapshot(root) == *expected, "round {round}");
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
        outcomes[0] > 10 && outcomes[1] > 40 && outcomes[2] > 4 && outcomes[3] > 10,
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
