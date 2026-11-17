// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! `preview_transaction`: a transaction carried out in memory on the tree,
//! as `tilewright tx preview` carries it out, and reported within the byte
//! budget (docs/MCP.md, "Previewing a transaction").
//!
//! Planning reads the tree on disk, not the snapshot, which keeps no file
//! text. So after planning the server loads the tree again, with the files
//! the plan read laid over it as it read them, and requires that tree to
//! be at the snapshot's revision: then the plan was made on the snapshot's
//! files. Nothing is written, locked or published: the plan is dropped.

use crate::{
    budget::{self, Output, PAGE_BYTES},
    json::hex,
    snapshot::Valid,
};
use std::{fmt::Write as _, path::Path};
use tilewright_edit::{
    Budgets, FileChange, Limit, LimitOrigin, Options, Plan, Refusal, Transaction, digest,
};
use tilewright_source::{Diagnostic, LoadOptions, Overlay, SourceRoot, World};

/// The name a transaction's own diagnostics carry as their `path`. No
/// diagnostic of the edited tree has it: an entry of that name in the root
/// makes the tree fail to load, and one made after the snapshot is a change
/// the snapshot check refuses before any result.
pub const NAME: &str = "transaction";

/// Most operations a previewed transaction may have.
pub const MAXIMUM_OPERATIONS: usize = 256;
/// Most changed files a result lists; `files_changed` counts them all.
pub const MAXIMUM_FILES: usize = 200;
/// Most refusals a result lists; `refusals_total` counts them all, which
/// the library caps at 1,000, the last a note that there may be more.
pub const MAXIMUM_REFUSALS: usize = 500;

/// Most bytes a preview reads from the files a transaction touches: a
/// quarter of the library's 256 MiB (docs/MCP.md, "Previewing a
/// transaction", for the measurements behind it).
pub const READ_BYTES: u64 = 64 << 20;
/// Most work a preview does, in the library's units (bytes): an eighth
/// of the library's 4 GiB.
pub const WORK: u64 = 512 << 20;

/// The budgets a preview plans within, and its undo check carries out
/// each round within: tighter than the library's, which `tilewright tx
/// apply` uses, so a transaction refused here may still apply there.
pub fn budgets() -> Budgets {
    Budgets::default()
        .with_read_bytes(READ_BYTES)
        .with_work(WORK)
}

/// Refusals, and for each the limit the library says refused it, if any
/// (`tilewright_edit::Refusal::limit`).
pub(crate) struct Refused {
    diagnostics: Vec<Diagnostic>,
    limits: Vec<Option<Limit>>,
}

impl From<Refusal> for Refused {
    fn from(refusal: Refusal) -> Self {
        let limits = (0..refusal.diagnostics().len())
            .map(|index| refusal.limit(index))
            .collect();
        Self {
            diagnostics: refusal.into_diagnostics(),
            limits,
        }
    }
}

/// Diagnostics that name no limit, as a test gives them.
impl From<Vec<Diagnostic>> for Refused {
    fn from(diagnostics: Vec<Diagnostic>) -> Self {
        let limits = vec![None; diagnostics.len()];
        Self {
            diagnostics,
            limits,
        }
    }
}

/// Whether `limit` is one of the server's budgets: a budget the server
/// gave the library, tighter than the library's own, as the library says
/// in its structured limit, never read from a message.
pub(crate) fn at_server_budget(limit: Option<Limit>) -> bool {
    limit.is_some_and(|limit| matches!(limit.origin(), LimitOrigin::Budgets))
}

/// The tree on disk is not at the snapshot's revision, with the files the
/// plan read as it read them: the plan was not made on the snapshot.
#[derive(Debug, Eq, PartialEq)]
pub struct Drift;

/// Where a transaction was refused.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Stage {
    /// The transaction document: `jsonc.*` or `tx.shape`.
    Transaction,
    /// Carrying it out, its preconditions, or validating the result.
    Plan,
    /// The undo check.
    Undo,
}

impl Stage {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Transaction => "transaction",
            Self::Plan => "plan",
            Self::Undo => "undo",
        }
    }
}

/// What was found, before it is written.
struct Report<'p> {
    refused_at: Option<Stage>,
    operations: Option<usize>,
    plan: Option<&'p Plan>,
    undo: &'static str,
    refusals: &'p [Diagnostic],
    /// One for each refusal.
    limits: &'p [Option<Limit>],
    budgets: Budgets,
}

/// Previews the transaction whose text is `text` (the argument as the
/// request holds it) on the tree at `root`, which must be the snapshot's.
///
/// # Errors
/// [`Drift`] when the tree on disk is not the snapshot's.
pub fn preview(valid: &Valid, root: &Path, text: &str, excerpts: bool) -> Result<String, Drift> {
    preview_with(valid, root, text, excerpts, budgets(), |plan| {
        plan.check_undo_detailed().map_err(Refused::from)
    })
}

/// [`preview`] within `budgets`, with `undo` as the undo check, so that a
/// test can plan at budgets of its choosing and give a refusal of its
/// choosing (long, many or hostile messages) to check how it is written;
/// [`preview`] gives the server's budgets and the library's own check.
pub(crate) fn preview_with(
    valid: &Valid,
    root: &Path,
    text: &str,
    excerpts: bool,
    budgets: Budgets,
    undo: impl FnOnce(&Plan) -> Result<(), Refused>,
) -> Result<String, Drift> {
    let transaction = match Transaction::parse_detailed(NAME, text) {
        Ok(transaction) => transaction,
        // The document alone is wrong: the tree was not read.
        Err(problems) => {
            let problems = Refused::from(problems);
            return Ok(write(
                valid,
                &Report {
                    refused_at: Some(Stage::Transaction),
                    operations: None,
                    plan: None,
                    undo: "not_run",
                    refusals: &problems.diagnostics,
                    limits: &problems.limits,
                    budgets,
                },
                excerpts,
            ));
        }
    };
    // One handle for planning and the check after it, opened afresh as
    // every load of the snapshot is.
    let source = SourceRoot::new(root);
    let planned = tilewright_edit::plan_in_within_detailed(
        &source,
        &transaction,
        &Options::default(),
        budgets,
    )
    .map_err(Refused::from);
    let overlay: Overlay = match &planned {
        Ok(plan) => plan
            .files()
            .iter()
            .map(|file| (file.path.clone(), file.before.clone()))
            .collect(),
        // A refusal may come from a file that changed since the snapshot,
        // so the tree as it is now must be the snapshot's.
        Err(_) => Overlay::new(),
    };
    let unchanged = tilewright_source::load_edited_in(&source, &LoadOptions::default(), &overlay)
        .is_ok_and(|world| hex(&world.source_digest()) == valid.revision);
    if !unchanged {
        return Err(Drift);
    }
    let operations = Some(transaction.len());
    Ok(match &planned {
        Err(problems) => write(
            valid,
            &Report {
                refused_at: Some(Stage::Plan),
                operations,
                plan: None,
                undo: "not_run",
                refusals: &problems.diagnostics,
                limits: &problems.limits,
                budgets,
            },
            excerpts,
        ),
        Ok(plan) => {
            let undone = undo(plan);
            let (refused_at, undo, refusals, limits) = match &undone {
                Ok(()) => (None, "passed", &[][..], &[][..]),
                Err(problems) => (
                    Some(Stage::Undo),
                    "refused",
                    problems.diagnostics.as_slice(),
                    problems.limits.as_slice(),
                ),
            };
            write(
                valid,
                &Report {
                    refused_at,
                    operations,
                    plan: Some(plan),
                    undo,
                    refusals,
                    limits,
                    budgets,
                },
                excerpts,
            )
        }
    })
}

/// `"null"` or the JSON of a lowercase hex SHA-256 of `text`.
fn sha256(text: Option<&str>) -> String {
    digest(text).map_or_else(
        || "null".to_owned(),
        |digest| format!("\"{}\"", hex(&digest)),
    )
}

/// `"null"` or the byte length of `text`.
fn size(text: Option<&str>) -> String {
    text.map_or_else(|| "null".to_owned(), |text| text.len().to_string())
}

/// The smallest byte range of `after` that holds every change from
/// `before`, both ends on character boundaries, and how many bytes of
/// `before` it replaces: `(start, end, removed)`.
pub fn changed_range(before: &str, after: &str) -> (usize, usize, usize) {
    let (old, new) = (before.as_bytes(), after.as_bytes());
    let mut prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    // The same bytes before `prefix` in both, so a boundary of one is a
    // boundary of the other.
    while !after.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let room = old.len().min(new.len()) - prefix;
    let mut suffix = old
        .iter()
        .rev()
        .zip(new.iter().rev())
        .take(room)
        .take_while(|(a, b)| a == b)
        .count();
    while !after.is_char_boundary(new.len() - suffix) {
        suffix -= 1;
    }
    (prefix, new.len() - suffix, old.len() - suffix - prefix)
}

/// Whether a file of a plan is part of a revision: a file the tree had is
/// when the snapshot's load read it, and a file the transaction creates
/// is when the load of the tree it would leave reads it.
fn in_revision(file: &FileChange, snapshot: &World, after: &World) -> bool {
    match file.before {
        Some(_) => snapshot.files().contains_key(&file.path),
        None => after.files().contains_key(&file.path),
    }
}

/// One changed file.
fn file_item(output: &mut Output, file: &FileChange, in_revision: bool, excerpts: bool) {
    let change = match (&file.before, &file.after) {
        (None, _) => "created",
        (_, None) => "deleted",
        _ => "modified",
    };
    output.raw("{\"path\":");
    output.string(&file.path);
    let (before, after) = (file.before.as_deref(), file.after.as_deref());
    let _ = write!(
        output.text,
        ",\"in_revision\":{in_revision},\"change\":\"{change}\",\"before_bytes\":{},\"after_bytes\":{},\"before_sha256\":{},\"after_sha256\":{},\"changed\":",
        size(before),
        size(after),
        sha256(before),
        sha256(after),
    );
    let Some(after) = after else {
        output.raw("null,\"excerpt\":null,\"excerpt_truncated\":false}");
        return;
    };
    let (start, end, removed) = changed_range(before.unwrap_or(""), after);
    let (start_line, start_column) = crate::inspect::position(after, start);
    let (end_line, end_column) = crate::inspect::position(after, end);
    let _ = write!(
        output.text,
        "{{\"start\":{start},\"end\":{end},\"start_line\":{start_line},\"start_column\":{start_column},\"end_line\":{end_line},\"end_column\":{end_column},\"removed_bytes\":{removed}}},\"excerpt\":"
    );
    let cut = if excerpts {
        output.string(&after[start..end])
    } else {
        output.raw("null");
        false
    };
    let _ = write!(output.text, ",\"excerpt_truncated\":{cut}}}");
}

/// One refusal, in the shape of a `validate` diagnostic, and for a refusal
/// at a limit, which limit.
fn refusal_item(output: &mut Output, (diagnostic, limit): (&Diagnostic, &Option<Limit>)) {
    output.raw("{\"path\":");
    let path = output.string(&diagnostic.path);
    let _ = write!(
        output.text,
        ",\"on_transaction\":{},\"start\":{},\"end\":{},\"line\":{},\"column\":{},\"code\":",
        diagnostic.path == NAME,
        diagnostic.start,
        diagnostic.end,
        diagnostic.line,
        diagnostic.column
    );
    output.string(diagnostic.code);
    output.raw(",\"message\":");
    let message = output.string(&diagnostic.message);
    // Only a refusal at a limit has the member, so that every other
    // refusal costs what it did.
    if let Some(limit) = limit {
        let _ = write!(
            output.text,
            ",\"limit\":{{\"kind\":\"{}\",\"bound\":{},\"server_budget\":{}}}",
            limit.kind().as_str(),
            limit
                .bound()
                .map_or_else(|| "null".to_owned(), |bound| bound.to_string()),
            at_server_budget(Some(*limit))
        );
    }
    let _ = write!(output.text, ",\"truncated\":{}}}", path || message);
}

/// The structured result.
fn write(valid: &Valid, report: &Report<'_>, excerpts: bool) -> String {
    let changed: Vec<&FileChange> = report
        .plan
        .map(|plan| {
            plan.files()
                .iter()
                .filter(|file| file.is_changed())
                .collect()
        })
        .unwrap_or_default();
    let consulted = report
        .plan
        .map_or(0, |plan| plan.files().len() - changed.len());
    let covered = |file: &FileChange| {
        report
            .plan
            .is_some_and(|plan| in_revision(file, &valid.world, plan.world()))
    };
    // Unknown without a plan: a refused plan does not say what it read.
    let outside = report.plan.map_or("null", |plan| {
        if plan.files().iter().all(&covered) {
            "false"
        } else {
            "true"
        }
    });
    let at_budget = report.limits.iter().copied().any(at_server_budget);
    let mut output = Output::default();
    output.raw("{\"revision\":");
    output.string(&valid.revision);
    let _ = write!(
        output.text,
        ",\"would_apply\":{},\"refused_at\":",
        report.refused_at.is_none()
    );
    match report.refused_at {
        Some(stage) => {
            output.string(stage.as_str());
        }
        None => output.raw("null"),
    }
    let _ = write!(
        output.text,
        ",\"operations\":{},\"resulting_revision\":",
        report
            .operations
            .map_or_else(|| "null".to_owned(), |count| count.to_string())
    );
    match report.plan {
        Some(plan) => {
            output.string(&hex(&plan.world().source_digest()));
        }
        None => output.raw("null"),
    }
    let _ = write!(
        output.text,
        ",\"undo_check\":\"{}\",\"server_budgets\":{{\"read_bytes\":{},\"work\":{}}},\"server_budget_reached\":{at_budget},\"outside_revision\":{outside},\"files_changed\":{},\"files_consulted\":{consulted}",
        report.undo,
        report.budgets.read_bytes(),
        report.budgets.work(),
        changed.len()
    );
    // The two lists share the byte budget, files first; the fields after
    // them are small.
    let budget = PAGE_BYTES.saturating_sub(budget::cost(&output.text));
    let files = budget::page_within(changed.iter(), 0, MAXIMUM_FILES, budget, |output, file| {
        file_item(output, file, covered(file), excerpts);
    });
    let refusals = budget::page_within(
        report.refusals.iter().zip(report.limits),
        0,
        MAXIMUM_REFUSALS,
        budget.saturating_sub(files.cost),
        refusal_item,
    );
    let _ = write!(
        output.text,
        ",\"files\":{},\"files_truncated\":{},\"refusals_total\":{},\"refusals\":{},\"refusals_truncated\":{},\"byte_budget_reached\":{},\"truncated_strings\":{}}}",
        files.items,
        files.next.is_some(),
        report.refusals.len(),
        refusals.items,
        refusals.next.is_some(),
        files.stopped_by_bytes || refusals.stopped_by_bytes,
        output.cut_strings + files.cut_strings + refusals.cut_strings
    );
    output.text
}

#[cfg(test)]
mod tests {
    use super::{at_server_budget, budgets, changed_range};
    use tilewright_source::SourceRoot;

    #[test]
    #[allow(clippy::disallowed_methods)]
    fn only_the_servers_budgets_are_the_servers() {
        use tilewright_edit::{LimitKind, Options, Transaction, limits};
        assert!(!at_server_budget(None));
        // A budget the server gave the library is the server's.
        let tree = tempfile::tempdir().unwrap();
        std::fs::write(tree.path().join("world.jsonc"), "{}").unwrap();
        let test = Transaction::parse(
            "transaction",
            r#"{"format": "tilewright.transaction/1", "operations": [{"op": "test", "path": "world.jsonc", "pointer": [], "value": {}}]}"#,
        )
        .unwrap();
        let source = SourceRoot::new(tree.path());
        for (budgets, kind) in [
            (budgets().with_read_bytes(1), LimitKind::ReadBytes),
            (budgets().with_work(1), LimitKind::Work),
        ] {
            let refusal = tilewright_edit::plan_in_within_detailed(
                &source,
                &test,
                &Options::default(),
                budgets,
            )
            .unwrap_err();
            let limit = refusal.limit(0).unwrap();
            assert_eq!(limit.kind(), kind);
            assert!(at_server_budget(Some(limit)));
        }
        // A limit no budget widens is not, whatever its number.
        let steps = vec!["\"x\""; limits::POINTER + 1].join(",");
        let refusal = Transaction::parse_detailed(
            "transaction",
            format!(
                r#"{{"format": "tilewright.transaction/1", "operations": [{{"op": "set", "path": "world.jsonc", "pointer": [{steps}], "value": 1}}]}}"#
            ),
        )
        .err()
        .unwrap();
        let limit = refusal.limit(0).unwrap();
        assert_eq!(limit.kind(), LimitKind::PointerSteps);
        assert!(!at_server_budget(Some(limit)));
    }

    #[test]
    fn the_changed_range_is_the_smallest_on_character_boundaries() {
        for (before, after, expected) in [
            ("abc", "abc", (3, 3, 0)),
            ("", "abc", (0, 3, 0)),
            ("abc", "", (0, 0, 3)),
            ("abcdef", "abXYef", (2, 4, 2)),
            ("abcdef", "abef", (2, 2, 2)),
            ("abef", "abcdef", (2, 4, 0)),
            // A repeat can be read as inserted at either end; the range is
            // still as small as can be.
            ("aa", "aaa", (2, 3, 0)),
            ("xay", "xaay", (2, 3, 0)),
            // Multibyte characters that share leading or trailing bytes:
            // é is C3 A9, è C3 A8; ä is C3 A4, ¤ C2 A4.
            ("aé", "aè", (1, 3, 2)),
            ("äb", "¤b", (0, 2, 2)),
            ("日本", "日木", (3, 6, 3)),
        ] {
            assert_eq!(
                changed_range(before, after),
                expected,
                "{before:?} -> {after:?}"
            );
            let (start, end, removed) = expected;
            // The range, put back, gives the new text, and the old text
            // around it is the same.
            let suffix = after.len() - end;
            assert_eq!(
                format!(
                    "{}{}{}",
                    &before[..start],
                    &after[start..end],
                    &before[start + removed..]
                ),
                after
            );
            assert_eq!(before.len() - start - removed, suffix);
        }
    }
}
