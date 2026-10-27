// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Transactions over Tilewright world sources (`docs/spec/TRANSACTIONS.md`).
//!
//! A transaction is a document of operations against a base revision of a
//! source tree. Operations address content by identity: tiles by plane,
//! layer and position, objects by their place among a tile's objects or by
//! their uid, values by document path and pointer. Chunks are not named;
//! files are created and removed as content appears and disappears.
//!
//! [`plan`] carries a transaction out in memory: every edit is a lossless
//! span edit, the result is validated with the normal loader, and nothing is
//! written. The [`Plan`] holds the edited files, the validated world, the
//! transaction sealed with base and result digests, and its exact inverse.
//! [`Plan::publish`] writes it: under the root's lock it stages each file
//! beside its target, checks the files and validates the result again, and
//! renames the staged files into place, restoring what it published if a
//! later step fails. Each file changes atomically; the set of files does
//! not, so the inverse is also the way to repair an interrupted apply.
//! Where publishing does not check everything, as [`Plan::unchecked_access`]
//! says (on macOS and Windows, and on Linux on any file system but the
//! local ones whose access control lists it reads), it is refused unless
//! the caller accepts that with [`Plan::publish_with`] and
//! [`PublishOptions::unchecked_access`]. Ask before writing the inverse.
//!
//! ```no_run
//! use std::path::Path;
//! use tilewright_edit::{Options, Transaction, plan};
//!
//! let text = std::fs::read_to_string("place-tree.jsonc").unwrap();
//! let transaction = Transaction::parse("place-tree.jsonc", text).map_err(|problems| problems.len())?;
//! let plan = plan(Path::new("content"), &transaction, &Options::default())
//!     .map_err(|problems| problems.len())?;
//! // Publishing that does not check everything is refused by default, so
//! // ask first, before writing anything.
//! if let Some(unchecked) = plan.unchecked_access().map_err(|problems| problems.len())? {
//!     eprintln!("not applied: publishing here does not check everything: {unchecked}");
//!     return Ok(());
//! }
//! // The inverse is the recovery record: written atomically and synced,
//! // before anything is published.
//! let durable = tilewright_edit::write_new(Path::new("undo.jsonc"), plan.inverse()).unwrap();
//! if !durable {
//!     eprintln!("the inverse's name may not be durable yet");
//! }
//! plan.publish().map_err(|error| error.problems.len())?;
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
mod uids;

pub use publish::{
    PUBLISH_CONFINED, PUBLISH_SUPPORTED, PublishError, PublishOptions, Published, UNCHECKED_ACCESS,
};
pub use transaction::{Digests, FORMAT, Transaction};

use engine::{Failure, Primitive, Workspace};
use render::json_string;
use sha2::{Digest as _, Sha256};
use splice::Step;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use tilewright_jsonc::{Document, Limits, write_json_string};
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
    /// Most terrain cells in one transaction.
    pub const CELLS: usize = 1_000_000;
    /// Largest index in a pointer or address.
    pub const INDEX: usize = 250_000;
    /// Most bytes read from the files a transaction touches.
    pub const READ_BYTES: u64 = 256 << 20;
    /// Most work, in bytes: each operation costs its own size, the size of
    /// each document it reads, once per operation, and the size of each
    /// edit it makes, which parses its file again. Whatever an operation
    /// does within a document is bounded by a constant times these sizes.
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
    /// Reading or writing failed, or publishing is refused on this
    /// platform or for the access of the files.
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
    /// The plane and grid of each terrain operation, checked again when
    /// publishing, with a conflict at the operation to report.
    terrain: Vec<(String, String, Diagnostic)>,
    /// The root opened while planning; publishing and validating again
    /// use it, not the path.
    source: SourceRoot,
    load: LoadOptions,
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
/// replaced. Returns whether the new name is durable too: `false` when its
/// directory could not be synced, as on platforms that cannot sync a
/// directory, where only the file's bytes are.
///
/// # Errors
/// Fails if `path` exists or cannot be written.
pub fn write_new(path: &Path, text: &str) -> std::io::Result<bool> {
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
    // The file exists from here on; failing to sync its directory only
    // means its name may not be durable yet.
    #[cfg(unix)]
    return Ok(std::fs::File::open(directory)
        .and_then(|directory| directory.sync_all())
        .is_ok());
    #[cfg(not(unix))]
    Ok(false)
}

/// The name of the file a file is staged in beside its target.
#[must_use]
pub fn staging_name(name: &str) -> String {
    format!(".{name}.tilewright-tmp")
}

/// The tree as the plan leaves it, over the files on disk: every file it
/// touched or consulted, as it read or left it, and no staging file beside
/// any of them.
fn overlay(files: &[FileChange]) -> Overlay {
    // Every file the transaction read is validated as it read it, changed
    // or not, so that one snapshot is carried out and validated.
    let mut overlay = Overlay::new();
    for file in files {
        overlay.insert(file.path.clone(), file.after.clone());
        overlay.insert(staging_path(&file.path), None);
    }
    overlay
}

impl Plan {
    pub(crate) fn overlay(&self) -> Overlay {
        overlay(&self.files)
    }
}

/// The staging path of `path`.
pub(crate) fn staging_path(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((directory, name)) => format!("{directory}/{}", staging_name(name)),
        None => staging_name(path),
    }
}

/// The problems of one plan, at spans of the transaction. The last place
/// of the diagnostic limit is kept for a note that there may be more.
struct Problems<'a> {
    name: &'a str,
    lines: diagnostic::Lines<'a>,
    found: Vec<Diagnostic>,
    /// Whether a problem was dropped, or checking stopped, at the limit.
    more: bool,
}

impl<'a> Problems<'a> {
    fn new(transaction: &'a Transaction) -> Self {
        Self {
            name: &transaction.name,
            lines: diagnostic::Lines::new(transaction.text()),
            found: Vec::new(),
            more: false,
        }
    }

    fn report(&mut self, span: tilewright_jsonc::Span, failure: Failure) {
        if self.full() {
            return;
        }
        self.found.push(diagnostic::at(
            self.name,
            &self.lines,
            span.start,
            span.end,
            failure.code.as_str(),
            failure.message,
        ));
    }

    /// Whether no more problems are kept; checking should stop, and a note
    /// will say there may be more.
    fn full(&mut self) -> bool {
        let full = self.found.len() >= limits::DIAGNOSTICS - 1;
        self.more |= full;
        full
    }

    fn is_empty(&self) -> bool {
        self.found.is_empty()
    }

    /// The problems in order, with the note if there may be more.
    fn finish(self) -> Vec<Diagnostic> {
        let mut found = self.found;
        found.sort();
        found.dedup();
        if self.more {
            found.push(diagnostic::file(
                self.name,
                Code::Limit.as_str(),
                format!(
                    "{} problems are reported, as many as may be; there may be more",
                    limits::DIAGNOSTICS - 1
                ),
            ));
        }
        found
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
        if workspace.exhausted() || problems.full() {
            break;
        }
        if let Err(failure) = workspace.exists(path) {
            problems.report(*span, failure);
            continue;
        }
        let found = digest(
            workspace
                .entries
                .get(path)
                .and_then(|entry| entry.current.text()),
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
            if problems.full() {
                return;
            }
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
    // Nothing more could be reported, so the result is not worth hashing.
    if problems.full() {
        return;
    }
    if let Some(result) = &transaction.result {
        let computed: Digests = workspace
            .entries
            .iter()
            .map(|(path, entry)| (path.clone(), digest(entry.current.text())))
            .collect();
        let differing: BTreeSet<&String> = computed
            .keys()
            .chain(result.keys())
            .filter(|path| computed.get(*path) != result.get(*path))
            .collect();
        if !differing.is_empty() {
            problems.report(
                transaction.document.root().span(),
                Failure {
                    code: Code::Result,
                    message: format!(
                        "replaying the transaction gives other bytes than its result records for {}",
                        diagnostic::listed(differing.iter().map(|path| path.as_str()))
                    ),
                },
            );
        }
    }
}

/// The terrain operations whose address `problem` finds wrong, reported
/// at the operation.
fn check_terrain_addresses(
    transaction: &Transaction,
    problem: impl Fn(&str, &str) -> Option<String>,
) -> Vec<Diagnostic> {
    let mut problems = Problems::new(transaction);
    for operation in &transaction.operations {
        if problems.full() {
            break;
        }
        if let transaction::Action::TerrainSet { plane, grid, .. } = &operation.action
            && let Some(message) = problem(plane, grid)
        {
            problems.report(operation.span, Failure::conflict(message));
        }
    }
    if problems.is_empty() {
        Vec::new()
    } else {
        problems.finish()
    }
}

/// Why a terrain address does not name a grid of `world`, if it does not.
pub(crate) fn terrain_problem(world: &World, plane: &str, grid: &str) -> Option<String> {
    if !world.planes().contains_key(plane) {
        Some(format!("plane \"{plane}\" does not exist"))
    } else if grid != "height"
        && grid != "zone"
        && !world
            .profile()
            .ground_layers
            .iter()
            .any(|layer| layer == grid)
    {
        Some(format!(
            "\"{grid}\" is not a grid: name a ground layer of the profile, height or zone"
        ))
    } else {
        None
    }
}

/// The edited tree, loaded and with every terrain address checked against
/// it. A tree that does not load is reported with the loader's diagnostics;
/// a terrain operation that wrote cells where the tree has no such grid is
/// one reason it may not.
fn load_result(
    source: &SourceRoot,
    load: &LoadOptions,
    transaction: &Transaction,
    files: &[FileChange],
) -> Result<World, Vec<Diagnostic>> {
    let world = tilewright_source::load_edited_in(source, load, &overlay(files))?;
    let problems = check_terrain_addresses(transaction, |plane, grid| {
        terrain_problem(&world, plane, grid)
    });
    if problems.is_empty() {
        Ok(world)
    } else {
        Err(problems)
    }
}

/// Carries out the operations in order. A failed one changes nothing and
/// is reported, and the rest are still carried out, until a budget is
/// spent or the diagnostic limit reached.
fn run_operations(
    workspace: &mut Workspace<'_>,
    transaction: &Transaction,
    done: &BTreeSet<String>,
    chunks: &mut BTreeSet<String>,
    problems: &mut Problems<'_>,
) {
    for operation in &transaction.operations {
        // Past a budget or the diagnostic limit, carrying on could only
        // cost more; what is reported already rejects the transaction.
        if workspace.exhausted() || problems.full() {
            break;
        }
        let finished = operation
            .action
            .document_path()
            .is_some_and(|path| done.contains(path));
        if finished {
            continue;
        }
        if let Err(failure) = workspace.begin(operation.span.len()) {
            problems.report(operation.span, failure);
            break;
        }
        if let Err(failures) = operations::run(workspace, transaction, operation, chunks) {
            workspace.roll_back();
            for (span, failure) in failures {
                problems.report(span, failure);
            }
        }
    }
}

/// The terrain addresses publishing checks again, each with a conflict at
/// its operation to report.
fn terrain_addresses(transaction: &Transaction) -> Vec<(String, String, Diagnostic)> {
    let lines = diagnostic::Lines::new(transaction.text());
    transaction
        .operations
        .iter()
        .filter_map(|operation| match &operation.action {
            transaction::Action::TerrainSet { plane, grid, .. } => Some((
                plane.clone(),
                grid.clone(),
                diagnostic::at(
                    &transaction.name,
                    &lines,
                    operation.span.start,
                    operation.span.end,
                    Code::Conflict.as_str(),
                    "",
                ),
            )),
            _ => None,
        })
        .collect()
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
    // The edited world's diagnostics are a transaction's too, and the
    // loader's note of those it dropped takes the last place.
    let options = &Options {
        load: LoadOptions {
            maximum_diagnostics: options
                .load
                .maximum_diagnostics
                .min(limits::DIAGNOSTICS - 1),
            ..options.load
        },
        ..*options
    };
    let source = SourceRoot::new(root);
    let mut workspace = Workspace::new(&source);
    let mut problems = Problems::new(transaction);
    let done = if options.rebase {
        BTreeSet::new()
    } else {
        check_base(&mut workspace, transaction, &mut problems)
    };
    // Uid addresses resolve through the world as loaded, and as the
    // operations go on to edit it.
    if problems.is_empty() && transaction.addresses_uids() {
        workspace.uids = Some(uids::Index::load(&source, &options.load));
    }
    let mut chunks = BTreeSet::new();
    if problems.is_empty() {
        run_operations(
            &mut workspace,
            transaction,
            &done,
            &mut chunks,
            &mut problems,
        );
    }
    if problems.is_empty()
        && let Err(failure) = remove_empty_chunks(&mut workspace, &chunks)
    {
        problems.report(transaction.document.root().span(), failure);
    }
    if !options.rebase && problems.is_empty() {
        check_recorded(&workspace, transaction, &mut problems);
    }
    if !problems.is_empty() {
        return Err(problems.finish());
    }

    let files: Vec<FileChange> = workspace
        .entries
        .iter()
        .map(|(path, entry)| FileChange {
            path: path.clone(),
            before: entry.base.clone(),
            after: entry.current.text().map(str::to_owned),
        })
        .collect();
    let world = load_result(&source, &options.load, transaction, &files)?;
    let sealed = seal(transaction, &files);
    let too_large = |what: &str, detail: &str| {
        vec![diagnostic::file(
            &transaction.name,
            Code::Limit.as_str(),
            format!(
                "the {what} would exceed the limits of a transaction{detail}; split the transaction"
            ),
        )]
    };
    let inverse = inverse(&workspace).map_err(|TooLarge| too_large("inverse", ""))?;
    for (text, what) in [(&sealed, "sealed transaction"), (&inverse, "inverse")] {
        if let Err(found) = Transaction::parse(&transaction.name, text.as_str()) {
            let detail = found
                .first()
                .map_or_else(String::new, |problem| format!(": {}", problem.message));
            return Err(too_large(what, &detail));
        }
    }
    let terrain = terrain_addresses(transaction);
    // The plan keeps the very root it was carried out on.
    drop(workspace);
    Ok(Plan {
        root: root.to_path_buf(),
        terrain,
        source,
        load: options.load,
        files,
        world,
        sealed,
        inverse,
    })
}

/// A digest list: one `{"path", "sha256"}` entry per line, in path order,
/// with continuation lines at `indent`.
fn digests_text<'a>(
    entries: impl Iterator<Item = (&'a str, Option<[u8; 32]>)>,
    indent: &str,
) -> String {
    let lines: Vec<String> = entries
        .map(|(path, digest)| {
            let digest = digest.map_or_else(
                || "null".to_owned(),
                |digest| format!("\"{}\"", transaction::hex(&digest)),
            );
            format!(
                "{indent}  {{\"path\": {}, \"sha256\": {digest}}}",
                json_string(path)
            )
        })
        .collect();
    if lines.is_empty() {
        "[]".to_owned()
    } else {
        format!("[\n{}\n{indent}]", lines.join(",\n"))
    }
}

/// The transaction's text with `base` and `result` set to the digests of
/// `files` before and after; every other byte is kept.
fn seal(transaction: &Transaction, files: &[FileChange]) -> String {
    let mut text = transaction.text().to_owned();
    for key in ["base", "result"] {
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
        let indent = splice::item_indentation(document.source(), root, position);
        let value = digests_text(
            files.iter().map(|file| {
                let text = if key == "base" {
                    &file.before
                } else {
                    &file.after
                };
                (file.path.as_str(), digest(text.as_deref()))
            }),
            &indent,
        );
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

/// Most bytes of file text in one string of an inverse: escaped, even a
/// piece of control characters stays within the JSONC line limit.
const TEXT_PIECE: usize = 128 << 10;

/// Text built up to a limit, so that output an untrusted transaction makes
/// can never grow past the size of a transaction.
struct Bounded {
    text: String,
    limit: usize,
}

/// The output went past its limit.
struct TooLarge;

impl Bounded {
    fn push(&mut self, text: &str) -> Result<(), TooLarge> {
        if self.text.len().saturating_add(text.len()) > self.limit {
            return Err(TooLarge);
        }
        self.text.push_str(text);
        Ok(())
    }

    /// Appends `text` as a JSON string, checking the limit as it goes.
    fn push_json(&mut self, text: &str) -> Result<(), TooLarge> {
        self.push("\"")?;
        let mut escaped = String::new();
        for (index, character) in text.char_indices() {
            escaped.clear();
            write_json_string(&text[index..index + character.len_utf8()], &mut escaped);
            self.push(&escaped[1..escaped.len() - 1])?;
        }
        self.push("\"")
    }

    /// Appends a value's exact text on lines of its own, so that the
    /// operation's own text never lengthens one of the value's lines.
    fn push_value(&mut self, value: &str) -> Result<(), TooLarge> {
        self.push("\n")?;
        self.push(value)?;
        self.push("\n")
    }

    /// Appends exact file text as an array of strings, each at most
    /// [`TEXT_PIECE`] bytes of the text and on its own line, so that no
    /// string or line passes the limits of a transaction.
    fn push_text(&mut self, text: &str) -> Result<(), TooLarge> {
        self.push("[")?;
        let mut rest = text;
        let mut first = true;
        while !rest.is_empty() || first {
            let mut end = rest.len().min(TEXT_PIECE);
            while !rest.is_char_boundary(end) {
                end -= 1;
            }
            self.push(if first { "\n" } else { ",\n" })?;
            first = false;
            self.push_json(&rest[..end])?;
            rest = &rest[end..];
        }
        self.push("\n]")
    }

    /// Appends a pointer with each step on a line of its own, so that long
    /// keys never share a line.
    fn pointer(&mut self, pointer: &[Step]) -> Result<(), TooLarge> {
        self.push("[")?;
        for (index, step) in pointer.iter().enumerate() {
            self.push(if index > 0 { ",\n" } else { "\n" })?;
            match step {
                Step::Key(key) => self.push_json(key)?,
                Step::Index(index) => self.push(&index.to_string())?,
            }
        }
        self.push("\n]")
    }
}

/// Writes one inverse operation of the file at `path`.
fn write_operation(
    output: &mut Bounded,
    path: &str,
    primitive: &Primitive,
) -> Result<(), TooLarge> {
    output.push("{\"op\": ")?;
    let kind = match primitive {
        Primitive::Set { .. } => "set",
        Primitive::Insert { .. } => "insert",
        Primitive::Remove { .. } => "remove",
        Primitive::Create { .. } => "document.create",
        Primitive::Delete => "document.delete",
    };
    output.push_json(kind)?;
    output.push(", \"path\": ")?;
    output.push_json(path)?;
    match primitive {
        Primitive::Set { pointer, value } => {
            output.push(", \"pointer\": ")?;
            output.pointer(pointer)?;
            output.push(", \"value\": ")?;
            output.push_value(value)?;
        }
        Primitive::Insert {
            pointer,
            index,
            key,
            value,
            layout,
        } => {
            let mut full = pointer.clone();
            full.push(Step::Index(*index));
            output.push(", \"pointer\": ")?;
            output.pointer(&full)?;
            if let Some((key, colon)) = key {
                output.push(", \"key\": ")?;
                output.push_value(key)?;
                output.push(", \"colon\": ")?;
                output.push_text(colon)?;
            }
            output.push(", \"value\": ")?;
            output.push_value(value)?;
            output.push(", \"before\": ")?;
            output.push_text(&layout.before)?;
            output.push(", \"after\": ")?;
            output.push_text(&layout.after)?;
        }
        Primitive::Remove { pointer, separator } => {
            output.push(", \"pointer\": ")?;
            output.pointer(pointer)?;
            output.push(", \"separator\": ")?;
            output.push_text(separator)?;
        }
        Primitive::Create { text } => {
            output.push(", \"text\": ")?;
            output.push_text(text)?;
        }
        Primitive::Delete => {}
    }
    output.push("}")
}

/// The exact inverse of the workspace's edits: per changed file, in path
/// order, its edits undone last to first. A file the transaction created is
/// deleted, and one it deleted is created again with its text. Building it
/// stops as soon as it would be larger than a transaction may be.
fn inverse(workspace: &Workspace<'_>) -> Result<String, TooLarge> {
    let changed: Vec<(&String, &engine::Entry)> = workspace
        .entries
        .iter()
        .filter(|(_, entry)| entry.is_changed())
        .collect();
    let base = digests_text(
        changed
            .iter()
            .map(|(path, entry)| (path.as_str(), digest(entry.current.text()))),
        "  ",
    );
    let result = digests_text(
        changed
            .iter()
            .map(|(path, entry)| (path.as_str(), digest(entry.base.as_deref()))),
        "  ",
    );
    let mut output = Bounded {
        text: String::new(),
        limit: Limits::default().max_input_bytes,
    };
    output.push("{\n  \"format\": ")?;
    output.push_json(FORMAT)?;
    output.push(",\n  \"base\": ")?;
    output.push(&base)?;
    output.push(",\n  \"result\": ")?;
    output.push(&result)?;
    output.push(",\n  \"operations\": [")?;
    let mut first = true;
    for (path, entry) in &changed {
        let mut write = |primitive: &Primitive| -> Result<(), TooLarge> {
            output.push(if first { "\n    " } else { ",\n    " })?;
            first = false;
            write_operation(&mut output, path, primitive)
        };
        match (&entry.base, entry.current.text()) {
            (None, _) => write(&Primitive::Delete)?,
            (Some(text), None) => {
                // The text is written straight from the entry, not copied.
                output.push(if first { "\n    " } else { ",\n    " })?;
                first = false;
                output.push("{\"op\": \"document.create\", \"path\": ")?;
                output.push_json(path)?;
                output.push(", \"text\": ")?;
                output.push_text(text)?;
                output.push("}")?;
            }
            (Some(_), Some(_)) => {
                for primitive in entry.inverse.iter().rev() {
                    write(primitive)?;
                }
            }
        }
    }
    output.push(if first { "]\n}\n" } else { "\n  ]\n}\n" })?;
    Ok(output.text)
}

// Most tests publish, which is supported on Linux, macOS and Windows.
#[cfg(all(test, any(target_os = "linux", target_os = "macos", windows)))]
mod tests;
