// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Which limit refused a transaction ([`Refusal::limit`]): every limit the
//! library enforces is named with its bound and origin, beside the same
//! diagnostics the functions without limits give.

use super::*;
use std::cell::RefCell;

thread_local! {
    /// The refusal a helper kept last on this thread.
    static LAST: RefCell<Option<Refusal>> = const { RefCell::new(None) };
}

/// Keeps `refusal` for [`limit`] and returns its diagnostics, after
/// checking that each `tx.limit` diagnostic, and only each, has a limit.
pub(super) fn kept(refusal: Refusal) -> Vec<Diagnostic> {
    for (index, diagnostic) in refusal.diagnostics().iter().enumerate() {
        assert_eq!(
            refusal.limit(index).is_some(),
            diagnostic.code == "tx.limit",
            "{diagnostic:#?}: {:?}",
            refusal.limit(index)
        );
    }
    assert_eq!(refusal.limit(refusal.diagnostics().len()), None);
    dump(refusal.diagnostics());
    let diagnostics = refusal.diagnostics().to_vec();
    LAST.set(Some(refusal));
    diagnostics
}

/// Appends `found` to a file per test under `TILEWRIGHT_DIAGNOSTICS_DUMP`,
/// when it is set, so that two builds can be compared diagnostic for
/// diagnostic.
fn dump(found: &[Diagnostic]) {
    let Some(directory) = std::env::var_os("TILEWRIGHT_DIAGNOSTICS_DUMP") else {
        return;
    };
    let name = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .replace("::", "-");
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(Path::new(&directory).join(name))
        .unwrap();
    std::io::Write::write_all(&mut file, format!("{found:#?}\n").as_bytes()).unwrap();
}

/// The limit of the one `tx.limit` diagnostic, among those kept last, whose
/// message holds `text`.
pub(super) fn limit(text: &str) -> Limit {
    LAST.with_borrow(|last| {
        let refusal = last.as_ref().expect("a refusal was kept");
        let found: Vec<Limit> = refusal
            .diagnostics()
            .iter()
            .enumerate()
            .filter(|(_, diagnostic)| {
                diagnostic.code == "tx.limit" && diagnostic.message.contains(text)
            })
            .filter_map(|(index, _)| refusal.limit(index))
            .collect();
        let mut distinct = found.clone();
        distinct.dedup();
        assert_eq!(distinct.len(), 1, "{text}: {refusal:#?}");
        found[0]
    })
}

/// Asserts the limit of [`limit`]`(text)`.
#[track_caller]
pub(super) fn assert_limit(text: &str, kind: LimitKind, bound: Option<u64>, origin: LimitOrigin) {
    let found = limit(text);
    assert_eq!(
        (found.kind(), found.bound(), found.origin()),
        (kind, bound, origin),
        "{text}"
    );
}

/// Plans `transaction` on the tree at `root` within `budgets`, which must
/// refuse it, and keeps the refusal.
fn refused_within(root: &Path, transaction: &Transaction, budgets: Budgets) -> Vec<Diagnostic> {
    match plan_in_within_detailed(
        &SourceRoot::new(root),
        transaction,
        &Options::default(),
        budgets,
    ) {
        Ok(_) => panic!("the transaction should be refused"),
        Err(refusal) => kept(refusal),
    }
}

/// A read budget below the format's limit on a file.
fn read_budget(bytes: u64) -> Budgets {
    Budgets::default().with_read_bytes(bytes)
}

const TWO_TESTS: &str = r#"{"op": "test", "path": "defs/proto/coin.jsonc", "pointer": ["id"], "value": "coin"},
    {"op": "test", "path": "defs/proto/huge.jsonc", "pointer": [], "absent": true}"#;

#[test]
fn a_file_past_the_file_limit_read_after_another_is_the_file_limit() {
    let tree = copy();
    fs::write(
        tree.path().join("defs/proto/huge.jsonc"),
        vec![b' '; (64 << 20) + 1],
    )
    .unwrap();
    // A read budget of the format's 64 MiB, as a caller may set it: once
    // the first file is read, less than 64 MiB is left, so the second file
    // is read only that far, and the message names the budget. The limit
    // that refused it is the format's, which no budget widens.
    let found = refused_within(tree.path(), &transaction(TWO_TESTS), read_budget(64 << 20));
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_eq!(
        found[0].message,
        format!("the touched files hold more than {} bytes", 64 << 20)
    );
    assert_limit(
        "hold more than",
        LimitKind::FileSize,
        Some(64 << 20),
        LimitOrigin::Library,
    );
    // Read first, the same file is reported in the file limit's words.
    let alone = transaction(
        r#"{"op": "test", "path": "defs/proto/huge.jsonc", "pointer": [], "absent": true}"#,
    );
    refused_within(tree.path(), &alone, read_budget(64 << 20));
    assert_limit(
        "file is too large",
        LimitKind::FileSize,
        Some(64 << 20),
        LimitOrigin::Library,
    );
}

#[test]
fn a_file_within_the_file_limit_read_past_the_budget_is_the_budget() {
    let tree = copy();
    // Exactly the format's limit: within it.
    fs::write(
        tree.path().join("defs/proto/huge.jsonc"),
        vec![b' '; 64 << 20],
    )
    .unwrap();
    let found = refused_within(tree.path(), &transaction(TWO_TESTS), read_budget(64 << 20));
    assert_eq!(found.len(), 1, "{found:#?}");
    assert_limit(
        "hold more than",
        LimitKind::ReadBytes,
        Some(64 << 20),
        LimitOrigin::Budgets,
    );
    fs::write(
        tree.path().join("defs/proto/huge.jsonc"),
        vec![b' '; 2 << 20],
    )
    .unwrap();
    for budgets in [read_budget(1 << 20), read_budget((2 << 20) + 10)] {
        let found = refused_within(tree.path(), &transaction(TWO_TESTS), budgets);
        assert_eq!(found.len(), 1, "{found:#?}");
        assert_limit(
            "hold more than",
            LimitKind::ReadBytes,
            Some(budgets.read_bytes()),
            LimitOrigin::Budgets,
        );
    }
}

#[test]
fn an_inverse_past_the_operation_limit_is_refused() {
    // Each chunk's stroke adds 32 palette entries and sets a row; the
    // inverse removes each entry and sets the row back.
    let chunks = 3_100;
    let tree = super::undo::painted(chunks, 32);
    let found = problems(tree.path(), &super::undo::stroke(chunks, 32));
    assert!(
        has(
            &found,
            "tx.limit",
            "the inverse would hold more than 100000 operations"
        ),
        "{found:#?}"
    );
    assert_limit(
        "100000 operations",
        LimitKind::InverseOperations,
        Some(100_000),
        LimitOrigin::Library,
    );
}

#[test]
fn every_kind_has_a_distinct_stable_name() {
    let kinds = [
        (LimitKind::Files, "files"),
        (LimitKind::TerrainChunks, "terrain_chunks"),
        (LimitKind::ReadBytes, "read_bytes"),
        (LimitKind::FileSize, "file_size"),
        (LimitKind::Work, "work"),
        (LimitKind::InverseOperations, "inverse_operations"),
        (LimitKind::InverseBytes, "inverse_bytes"),
        (LimitKind::Document, "document"),
        (LimitKind::Records, "records"),
        (LimitKind::Diagnostics, "diagnostics"),
        (LimitKind::Operations, "operations"),
        (LimitKind::PointerSteps, "pointer_steps"),
        (LimitKind::Cells, "cells"),
    ];
    let names: std::collections::BTreeSet<&str> =
        kinds.iter().map(|(kind, _)| kind.as_str()).collect();
    assert_eq!(names.len(), kinds.len());
    for (kind, name) in kinds {
        assert_eq!(kind.as_str(), name);
    }
}

#[test]
fn a_budget_is_the_callers_only_below_the_library_limit() {
    for (budgets, origin) in [
        (Budgets::default(), LimitOrigin::Library),
        (
            Budgets::default()
                .with_read_bytes(u64::MAX)
                .with_work(u64::MAX),
            LimitOrigin::Library,
        ),
        (
            Budgets::default().with_read_bytes(limits::READ_BYTES - 1),
            LimitOrigin::Budgets,
        ),
    ] {
        let limit = Limit::read_bytes(budgets);
        assert_eq!(
            (limit.kind(), limit.origin()),
            (LimitKind::ReadBytes, origin)
        );
        assert_eq!(limit.bound(), Some(budgets.read_bytes()));
    }
    for (budgets, origin) in [
        (Budgets::default(), LimitOrigin::Library),
        (
            Budgets::default().with_work(limits::WORK - 1),
            LimitOrigin::Budgets,
        ),
    ] {
        let limit = Limit::work(budgets);
        assert_eq!(
            (limit.kind(), limit.origin(), limit.bound()),
            (LimitKind::Work, origin, Some(budgets.work()))
        );
    }
}

#[test]
fn each_parser_bound_is_named() {
    use tilewright_jsonc::ErrorCode as E;
    let defaults = tilewright_jsonc::Limits::default();
    for (code, bound) in [
        (E::InputTooLarge, Some(defaults.max_input_bytes)),
        (E::TooManyLines, Some(defaults.max_lines)),
        (E::LineTooLong, Some(defaults.max_line_bytes)),
        (E::NumberTooLong, Some(defaults.max_number_bytes)),
        (E::StringTooLong, Some(defaults.max_string_bytes)),
        (E::DepthLimit, Some(defaults.max_depth)),
        (E::ValueLimit, Some(defaults.max_values)),
        (E::ArrayLimit, Some(defaults.max_array_items)),
        (E::ObjectLimit, Some(defaults.max_object_keys)),
        (E::TooManyComments, None),
    ] {
        let limit = Limit::parser(code).unwrap();
        assert_eq!(limit.kind(), LimitKind::Document);
        assert_eq!(
            limit.bound(),
            bound.map(|bound| u64::try_from(bound).unwrap()),
            "{code:?}"
        );
    }
    assert_eq!(Limit::parser(E::UnexpectedEnd), None);
}

#[test]
fn an_inverse_too_large_to_write_is_the_size_of_a_transaction() {
    // Deleting a file makes an inverse that holds its text, escaped: a
    // file of backslashes doubles, past the size of a transaction.
    let tree = copy();
    let line = format!("\"{}\"", "\\\\".repeat(500_000));
    let text = format!("{{\"a\": [\n{}\n]}}\n", vec![line.as_str(); 40].join(",\n"));
    assert!(text.len() < 64 << 20 && 2 * text.len() > 64 << 20);
    fs::write(tree.path().join("profile/big.jsonc"), text).unwrap();
    let found = problems(
        tree.path(),
        &transaction(r#"{"op": "document.delete", "path": "profile/big.jsonc"}"#),
    );
    assert!(
        has(
            &found,
            "tx.limit",
            "the inverse would exceed the limits of a transaction; split"
        ),
        "{found:#?}"
    );
    assert_limit(
        "exceed the limits",
        LimitKind::Records,
        Some(64 << 20),
        LimitOrigin::Library,
    );
}
