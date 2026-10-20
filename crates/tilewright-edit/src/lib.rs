// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Transactions over Tilewright world sources (`docs/spec/TRANSACTIONS.md`).
//!
//! A transaction is a document of operations against a base revision of a
//! source tree. Operations address content by identity: tiles by plane,
//! layer and position, objects by their place among a tile's objects,
//! values by document path and pointer. Chunks are not named; files are
//! created and removed as content appears and disappears.
//!
//! [`plan`] carries a transaction out in memory: every edit is a lossless
//! span edit, the result is validated with the normal loader, and nothing is
//! written. The [`Plan`] holds the edited files, the validated world, the
//! transaction sealed with base and result digests, and its exact inverse.
//! [`Plan::publish`] writes it: it stages each file beside its target,
//! checks the base again, and renames the staged files into place.
//!
//! ```no_run
//! use std::path::Path;
//! use tilewright_edit::{Options, Transaction, plan};
//!
//! let text = std::fs::read_to_string("place-tree.jsonc").unwrap();
//! let transaction = Transaction::parse("place-tree.jsonc", text).map_err(|problems| problems.len())?;
//! let plan = plan(Path::new("content"), &transaction, &Options::default())
//!     .map_err(|problems| problems.len())?;
//! std::fs::write("undo.jsonc", plan.inverse()).unwrap();
//! plan.publish().map_err(|problems| problems.len())?;
//! # Ok::<(), usize>(())
//! ```

mod diagnostic;
mod engine;
mod operations;
mod publish;
mod render;
mod splice;
mod terrain;
mod transaction;

pub use transaction::{Digests, FORMAT, Transaction};

use engine::{Failure, Primitive, Workspace};
use render::{Node, json_string};
use sha2::{Digest as _, Sha256};
use splice::Step;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use tilewright_jsonc::{Document, Limits};
use tilewright_source::{Diagnostic, LoadOptions, Overlay, SourceRoot, World};

/// Bounds on a transaction, enforced while it is read and carried out.
pub mod limits {
    /// Most operations in one transaction.
    pub const OPERATIONS: usize = 100_000;
    /// Most files one transaction touches, and most entries of `base` or
    /// `result`.
    pub const FILES: usize = 10_000;
    /// Most steps in a pointer.
    pub const POINTER: usize = 64;
    /// Longest separator, colon or layout string, in bytes.
    pub const SEPARATOR: usize = 4_096;
    /// Most terrain cells in one transaction.
    pub const CELLS: usize = 1_000_000;
    /// Largest index in a pointer or address.
    pub const INDEX: usize = 250_000;
    /// Most bytes read from the files a transaction touches.
    pub const READ_BYTES: u64 = 1 << 30;
    /// Most work while editing, in bytes parsed or compared and items
    /// scanned: each edit parses its file again, and finding an object or
    /// an `{"id": ...}` entry scans its array.
    pub const WORK: u64 = 4 << 30;
    /// Most diagnostics reported for one transaction.
    pub const DIAGNOSTICS: usize = 1_000;
}

/// Stable diagnostic codes of transactions. Problems with the edited world
/// itself use the loader's codes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Code {
    /// The transaction document is malformed.
    Shape,
    /// A precondition failed: a base digest, an expected value, or the
    /// content an operation needs.
    Conflict,
    /// Replaying the transaction gives other bytes than its `result`.
    Result,
    /// A path a transaction may not touch, or a link in the way.
    Path,
    /// A bound of [`limits`] was exceeded.
    Limit,
    /// Reading or writing failed.
    Io,
}

impl Code {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Shape => "tx.shape",
            Self::Conflict => "tx.conflict",
            Self::Result => "tx.result",
            Self::Path => "tx.path",
            Self::Limit => "tx.limit",
            Self::Io => "tx.io",
        }
    }
}

/// How to carry out a transaction.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Options {
    /// Bounds of the validating load.
    pub load: LoadOptions,
    /// Replay the operations on the tree as it is, ignoring the recorded
    /// `base` and `result`: the sealed transaction then records the new
    /// base. Operation preconditions still apply.
    pub rebase: bool,
}

/// One file a transaction touched or consulted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileChange {
    pub path: String,
    /// The file before, `None` when absent.
    pub before: Option<String>,
    /// The file after, `None` when absent.
    pub after: Option<String>,
}

impl FileChange {
    #[must_use]
    pub fn is_changed(&self) -> bool {
        self.before != self.after
    }
}

/// SHA-256 of a file's bytes, or `None` for an absent file.
#[must_use]
pub fn digest(text: Option<&str>) -> Option<[u8; 32]> {
    text.map(|text| Sha256::digest(text.as_bytes()).into())
}

/// A transaction carried out in memory.
#[derive(Debug)]
pub struct Plan {
    root: PathBuf,
    files: Vec<FileChange>,
    world: World,
    sealed: String,
    inverse: String,
}

impl Plan {
    /// Every file the transaction touched or consulted, by path.
    #[must_use]
    pub fn files(&self) -> &[FileChange] {
        &self.files
    }

    /// The validated world after the transaction.
    #[must_use]
    pub const fn world(&self) -> &World {
        &self.world
    }

    /// The transaction with `base` and `result` recording the digests of
    /// every file it touched or consulted, before and after.
    #[must_use]
    pub fn sealed(&self) -> &str {
        &self.sealed
    }

    /// The inverse transaction: applied after this one, it restores every
    /// file byte for byte.
    #[must_use]
    pub fn inverse(&self) -> &str {
        &self.inverse
    }

    /// The source root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// Writes `text` to a new file at `path` atomically: it is written beside
/// the target, synced and renamed into place, and an existing file is never
/// replaced.
///
/// # Errors
/// Fails if `path` exists or cannot be written.
pub fn write_new(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(text.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    #[cfg(unix)]
    std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}

/// The name of the file a file is staged in beside its target.
#[must_use]
pub fn staging_name(name: &str) -> String {
    format!(".{name}.tilewright-tmp")
}

/// The staging path of `path`.
fn staging_path(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((directory, name)) => format!("{directory}/{}", staging_name(name)),
        None => staging_name(path),
    }
}

/// The problems of one plan, at spans of the transaction.
struct Problems<'a> {
    name: &'a str,
    lines: diagnostic::Lines<'a>,
    found: Vec<Diagnostic>,
}

impl Problems<'_> {
    fn report(&mut self, span: tilewright_jsonc::Span, failure: Failure) {
        if self.found.len() < limits::DIAGNOSTICS {
            self.found.push(diagnostic::at(
                self.name,
                &self.lines,
                span.start,
                span.end,
                failure.code.as_str(),
                failure.message,
            ));
        }
    }
}

/// Checks the recorded base. Returns the files already at their result,
/// which are done when every operation names one file: an interrupted
/// apply of such a transaction can be applied again.
fn check_base(
    workspace: &mut Workspace<'_>,
    transaction: &Transaction,
    problems: &mut Problems<'_>,
) -> BTreeSet<String> {
    let mut done = BTreeSet::new();
    let Some((span, base)) = &transaction.base else {
        return done;
    };
    let resumable = transaction.documents_only();
    for (path, expected) in base {
        if let Err(failure) = workspace.exists(path) {
            problems.report(*span, failure);
            continue;
        }
        let found = digest(
            workspace
                .entries
                .get(path)
                .and_then(|entry| entry.current.as_deref()),
        );
        if found == *expected {
            continue;
        }
        let result = transaction
            .result
            .as_ref()
            .and_then(|result| result.get(path));
        if resumable && result == Some(&found) {
            done.insert(path.clone());
            continue;
        }
        problems.report(
            *span,
            Failure::conflict(format!(
                "{path} has changed since the transaction's base; rebase the transaction"
            )),
        );
    }
    done
}

/// Removes the chunks that tile operations left without content.
fn remove_empty_chunks(
    workspace: &mut Workspace<'_>,
    chunks: &BTreeSet<String>,
) -> Result<(), Failure> {
    for path in chunks {
        if workspace.exists(path)? && terrain::chunk_is_empty(workspace.document(path)?.root()) {
            workspace.delete(path)?;
        }
    }
    Ok(())
}

/// Checks that the transaction touched only files its base lists, and that
/// it gave the bytes its result records.
fn check_recorded(
    workspace: &Workspace<'_>,
    transaction: &Transaction,
    problems: &mut Problems<'_>,
) {
    if let Some((span, base)) = &transaction.base {
        for path in workspace.entries.keys() {
            if !base.contains_key(path) {
                problems.report(
                    *span,
                    Failure::conflict(format!(
                        "the transaction touches {path}, which its base does not list; rebase the transaction"
                    )),
                );
            }
        }
    }
    if let Some(result) = &transaction.result {
        let computed: Digests = workspace
            .entries
            .iter()
            .map(|(path, entry)| (path.clone(), digest(entry.current.as_deref())))
            .collect();
        let differing: BTreeSet<&String> = computed
            .keys()
            .chain(result.keys())
            .filter(|path| computed.get(*path) != result.get(*path))
            .collect();
        if !differing.is_empty() {
            let differing: Vec<&str> = differing.into_iter().map(String::as_str).collect();
            problems.report(
                transaction.document.root().span(),
                Failure {
                    code: Code::Result,
                    message: format!(
                        "replaying the transaction gives other bytes than its result records for {}",
                        differing.join(", ")
                    ),
                },
            );
        }
    }
}

/// Carries out `transaction` on the tree at `root` in memory and validates
/// the result. Nothing is written.
///
/// # Errors
/// Returns the transaction's problems, each at its operation, or the
/// diagnostics of the edited world.
pub fn plan(
    root: &Path,
    transaction: &Transaction,
    options: &Options,
) -> Result<Plan, Vec<Diagnostic>> {
    let source = SourceRoot::new(root);
    let mut workspace = Workspace::new(&source);
    let mut problems = Problems {
        name: &transaction.name,
        lines: diagnostic::Lines::new(transaction.text()),
        found: Vec::new(),
    };
    let done = if options.rebase {
        BTreeSet::new()
    } else {
        check_base(&mut workspace, transaction, &mut problems)
    };
    let mut chunks = BTreeSet::new();
    if problems.found.is_empty() {
        for operation in &transaction.operations {
            let finished = operation
                .action
                .document_path()
                .is_some_and(|path| done.contains(path));
            if finished {
                continue;
            }
            if let Err(failures) =
                operations::run(&mut workspace, transaction, operation, &mut chunks)
            {
                for (span, failure) in failures {
                    problems.report(span, failure);
                }
            }
        }
    }
    if problems.found.is_empty()
        && let Err(failure) = remove_empty_chunks(&mut workspace, &chunks)
    {
        problems.report(transaction.document.root().span(), failure);
    }
    if !options.rebase && problems.found.is_empty() {
        check_recorded(&workspace, transaction, &mut problems);
    }
    if !problems.found.is_empty() {
        let mut found = problems.found;
        found.sort();
        found.dedup();
        return Err(found);
    }

    let files: Vec<FileChange> = workspace
        .entries
        .iter()
        .map(|(path, entry)| FileChange {
            path: path.clone(),
            before: entry.base.clone(),
            after: entry.current.clone(),
        })
        .collect();
    let mut overlay = Overlay::new();
    for file in files.iter().filter(|file| file.is_changed()) {
        overlay.insert(file.path.clone(), file.after.clone());
        overlay.insert(staging_path(&file.path), None);
    }
    let world = tilewright_source::load_edited(root, &options.load, &overlay)?;
    let sealed = seal(transaction, &files);
    let inverse = inverse(&workspace);
    for (text, what) in [(&sealed, "sealed transaction"), (&inverse, "inverse")] {
        if let Err(found) = Transaction::parse(&transaction.name, text.as_str()) {
            let detail = found
                .first()
                .map_or_else(String::new, |problem| format!(": {}", problem.message));
            return Err(vec![diagnostic::file(
                &transaction.name,
                Code::Limit.as_str(),
                format!(
                    "the {what} would exceed the limits of a transaction{detail}; split the transaction"
                ),
            )]);
        }
    }
    Ok(Plan {
        root: root.to_path_buf(),
        files,
        world,
        sealed,
        inverse,
    })
}

fn digests_node<'a>(entries: impl Iterator<Item = (&'a str, Option<[u8; 32]>)>) -> Node {
    Node::Object(
        entries
            .map(|(path, digest)| {
                (
                    path.to_owned(),
                    digest.map_or_else(
                        || Node::Scalar("null".to_owned()),
                        |digest| Node::String(transaction::hex(&digest)),
                    ),
                )
            })
            .collect(),
    )
}

/// The transaction's text with `base` and `result` set to the digests of
/// `files` before and after; every other byte is kept.
fn seal(transaction: &Transaction, files: &[FileChange]) -> String {
    let mut text = transaction.text().to_owned();
    for (key, node) in [
        (
            "base",
            digests_node(
                files
                    .iter()
                    .map(|file| (file.path.as_str(), digest(file.before.as_deref()))),
            ),
        ),
        (
            "result",
            digests_node(
                files
                    .iter()
                    .map(|file| (file.path.as_str(), digest(file.after.as_deref()))),
            ),
        ),
    ] {
        let Ok(document) = Document::parse(text.as_str(), &Limits::default()) else {
            break;
        };
        let root = document.root();
        let members = root.as_object().unwrap_or_default();
        let position = members
            .iter()
            .position(|member| member.key == key)
            .or_else(|| members.iter().position(|member| member.key == "operations"))
            .unwrap_or(members.len());
        let value = node.layout(&splice::item_indentation(document.source(), root, position));
        text = if let Some(current) = root.get(key) {
            splice::splice(document.source(), current.span(), &value)
        } else {
            let key_text = json_string(key);
            let insertion = splice::Insertion {
                key: Some((key, &key_text)),
                value: &value,
                layout: None,
                colon: None,
            };
            match splice::insert(document.source(), root, position, &insertion) {
                Ok(inserted) => inserted.text,
                Err(_) => text,
            }
        };
    }
    text
}

fn pointer_text(pointer: &[Step]) -> String {
    let steps: Vec<String> = pointer
        .iter()
        .map(|step| match step {
            Step::Key(key) => json_string(key),
            Step::Index(index) => index.to_string(),
        })
        .collect();
    format!("[{}]", steps.join(", "))
}

/// One inverse operation as a line of the inverse transaction.
fn operation_text(path: &str, primitive: &Primitive) -> String {
    let path = json_string(path);
    match primitive {
        Primitive::Set { pointer, value } => format!(
            "{{\"op\": \"set\", \"path\": {path}, \"pointer\": {}, \"value\": {value}}}",
            pointer_text(pointer)
        ),
        Primitive::Insert {
            pointer,
            index,
            key,
            value,
            layout,
        } => {
            let mut full = pointer.clone();
            full.push(Step::Index(*index));
            let key = key.as_ref().map_or_else(String::new, |(key, colon)| {
                format!(", \"key\": {key}, \"colon\": {}", json_string(colon))
            });
            format!(
                "{{\"op\": \"insert\", \"path\": {path}, \"pointer\": {}{key}, \"value\": {value}, \"before\": {}, \"after\": {}}}",
                pointer_text(&full),
                json_string(&layout.before),
                json_string(&layout.after)
            )
        }
        Primitive::Remove { pointer, separator } => format!(
            "{{\"op\": \"remove\", \"path\": {path}, \"pointer\": {}, \"separator\": {}}}",
            pointer_text(pointer),
            json_string(separator)
        ),
        Primitive::Create { text } => format!(
            "{{\"op\": \"document.create\", \"path\": {path}, \"text\": {}}}",
            json_string(text)
        ),
        Primitive::Delete => format!("{{\"op\": \"document.delete\", \"path\": {path}}}"),
    }
}

/// The exact inverse of the workspace's edits: per changed file, in path
/// order, its edits undone last to first. A file the transaction created is
/// deleted, and one it deleted is created again with its text.
fn inverse(workspace: &Workspace<'_>) -> String {
    let changed: Vec<(&String, &engine::Entry)> = workspace
        .entries
        .iter()
        .filter(|(_, entry)| entry.base != entry.current)
        .collect();
    let mut operations = Vec::new();
    for (path, entry) in &changed {
        match (&entry.base, &entry.current) {
            (None, _) => operations.push(operation_text(path, &Primitive::Delete)),
            (Some(text), None) => {
                operations.push(operation_text(
                    path,
                    &Primitive::Create { text: text.clone() },
                ));
            }
            (Some(_), Some(_)) => operations.extend(
                entry
                    .inverse
                    .iter()
                    .rev()
                    .map(|primitive| operation_text(path, primitive)),
            ),
        }
    }
    let base = digests_node(
        changed
            .iter()
            .map(|(path, entry)| (path.as_str(), digest(entry.current.as_deref()))),
    );
    let result = digests_node(
        changed
            .iter()
            .map(|(path, entry)| (path.as_str(), digest(entry.base.as_deref()))),
    );
    let mut text = String::from("{\n  \"format\": ");
    text.push_str(&json_string(FORMAT));
    text.push_str(",\n  \"base\": ");
    text.push_str(&base.layout("  "));
    text.push_str(",\n  \"result\": ");
    text.push_str(&result.layout("  "));
    if operations.is_empty() {
        text.push_str(",\n  \"operations\": []\n}\n");
    } else {
        text.push_str(",\n  \"operations\": [\n    ");
        text.push_str(&operations.join(",\n    "));
        text.push_str("\n  ]\n}\n");
    }
    text
}

#[cfg(test)]
mod tests;
