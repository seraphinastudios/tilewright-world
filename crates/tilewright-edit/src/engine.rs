// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! The files a transaction touches, in memory, and the primitive edits that
//! change them. Each primitive records its exact inverse.

use crate::{
    Code, limits,
    splice::{self, Insertion, Layout, Step},
};
use std::collections::{BTreeMap, BTreeSet};
use tilewright_jsonc::{Document, Limits, Span, Value};
use tilewright_source::{AccessError, SourceRoot};

/// Why an operation cannot be carried out.
#[derive(Debug)]
pub(crate) struct Failure {
    pub code: Code,
    pub message: String,
}

impl Failure {
    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            code: Code::Conflict,
            message: message.into(),
        }
    }

    pub fn limit(message: impl Into<String>) -> Self {
        Self {
            code: Code::Limit,
            message: message.into(),
        }
    }

    fn access(error: &AccessError) -> Self {
        Self {
            code: match error {
                AccessError::Io(..) => Code::Io,
                AccessError::TooLarge(_) => Code::Limit,
                _ => Code::Path,
            },
            message: error.to_string(),
        }
    }
}

pub(crate) type Outcome<T> = Result<T, Failure>;

/// A document-level edit of one file, as the inverse of what an operation
/// did. Texts are exact.
#[derive(Clone, Debug)]
pub(crate) enum Primitive {
    Set {
        pointer: Vec<Step>,
        value: String,
    },
    Insert {
        /// The container.
        pointer: Vec<Step>,
        index: usize,
        /// A member's key as JSON string text, and the text between key and
        /// value.
        key: Option<(String, String)>,
        value: String,
        layout: Layout,
    },
    Remove {
        pointer: Vec<Step>,
        separator: String,
    },
    Create {
        text: String,
    },
    Delete,
}

/// The current state of a touched file: absent, text, or text parsed. A
/// parsed document holds its text, so the text is kept once.
pub(crate) enum Current {
    Absent,
    Text(String),
    Parsed(Document),
}

impl Current {
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Absent => None,
            Self::Text(text) => Some(text),
            Self::Parsed(document) => Some(document.source()),
        }
    }
}

impl Primitive {
    /// The bytes of text it holds.
    fn size(&self) -> usize {
        let pointer = |pointer: &[Step]| {
            pointer
                .iter()
                .map(|step| match step {
                    Step::Key(key) => key.len() + 4,
                    Step::Index(_) => 8,
                })
                .fold(0, usize::saturating_add)
        };
        match self {
            Self::Set {
                pointer: steps,
                value,
            } => pointer(steps).saturating_add(value.len()),
            Self::Insert {
                pointer: steps,
                key,
                value,
                layout,
                ..
            } => pointer(steps)
                .saturating_add(value.len())
                .saturating_add(
                    key.as_ref()
                        .map_or(0, |(key, colon)| key.len() + colon.len()),
                )
                .saturating_add(layout.before.len() + layout.after.len()),
            Self::Remove {
                pointer: steps,
                separator,
            } => pointer(steps).saturating_add(separator.len()),
            Self::Create { text } => text.len(),
            Self::Delete => 0,
        }
    }
}

/// One touched file.
pub(crate) struct Entry {
    /// The file as read, `None` when absent.
    pub base: Option<String>,
    pub current: Current,
    /// Inverses of the edits made, in the order they were made.
    pub inverse: Vec<Primitive>,
}

impl Entry {
    pub fn is_changed(&self) -> bool {
        self.base.as_deref() != self.current.text()
    }
}

/// The files a transaction touches, and its budgets.
///
/// Work is metered coarsely, so that no lookup needs metering of its own:
/// an operation costs its own size, the whole size of each document it
/// reads, once per operation, and the size of each edit it makes. Whatever
/// an operation does within a document (finding values, scanning members
/// and objects, comparing values) is bounded by a constant times the size
/// of what it is charged.
pub(crate) struct Workspace<'r> {
    root: &'r SourceRoot,
    pub entries: BTreeMap<String, Entry>,
    read_bytes: u64,
    work: u64,
    /// The documents the current operation has been charged for.
    charged: BTreeSet<String>,
    /// Bytes of inverse text, and inverse operations, recorded so far.
    inverse_bytes: u64,
    inverse_operations: usize,
    /// A budget was exceeded, and the failure reported; nothing more is
    /// carried out.
    spent: bool,
    /// The state of each file before the current operation first changed
    /// it, and its inverse's length then, to undo a failed operation.
    journal: BTreeMap<String, (Current, usize)>,
    /// The inverse's size and operations before the current operation.
    journal_inverse_bytes: u64,
    journal_inverse_operations: usize,
    /// Files that were read but cannot be edited, with why.
    unreadable: BTreeMap<String, String>,
    /// Files created by renaming another, with the file each came from.
    pub renames: BTreeMap<String, String>,
}

fn limits() -> Limits {
    Limits::default()
}

impl<'r> Workspace<'r> {
    pub fn new(root: &'r SourceRoot) -> Self {
        Self {
            root,
            entries: BTreeMap::new(),
            read_bytes: 0,
            work: 0,
            charged: BTreeSet::new(),
            inverse_bytes: 0,
            inverse_operations: 0,
            spent: false,
            journal: BTreeMap::new(),
            journal_inverse_bytes: 0,
            journal_inverse_operations: 0,
            unreadable: BTreeMap::new(),
            renames: BTreeMap::new(),
        }
    }

    /// Starts an operation of `size` bytes: it is charged its size, and
    /// each document it reads is charged again. What earlier operations
    /// did is kept.
    pub fn begin(&mut self, size: usize) -> Outcome<()> {
        self.charged.clear();
        self.journal.clear();
        self.journal_inverse_bytes = self.inverse_bytes;
        self.journal_inverse_operations = self.inverse_operations;
        self.charge(size)
    }

    /// Undoes everything the current operation did, so that a failed
    /// operation changes nothing and later ones see the files without it.
    pub fn roll_back(&mut self) {
        for (path, (before, length)) in std::mem::take(&mut self.journal) {
            if let Some(entry) = self.entries.get_mut(&path) {
                entry.current = before;
                entry.inverse.truncate(length);
            }
        }
        self.inverse_bytes = self.journal_inverse_bytes;
        self.inverse_operations = self.journal_inverse_operations;
    }

    /// Keeps a file's state from before the current operation's first
    /// change to it.
    fn keep(&mut self, path: &str, before: Current, inverse_length: usize) {
        self.journal
            .entry(path.to_owned())
            .or_insert((before, inverse_length));
    }

    /// The entry of `path`, reading the file the first time.
    fn entry(&mut self, path: &str) -> Outcome<&mut Entry> {
        if let Some(reason) = self.unreadable.get(path) {
            return Err(Failure::conflict(reason.clone()));
        }
        let count = self.entries.len();
        let root = self.root;
        match self.entries.entry(path.to_owned()) {
            std::collections::btree_map::Entry::Occupied(entry) => Ok(entry.into_mut()),
            std::collections::btree_map::Entry::Vacant(vacant) => {
                if count >= limits::FILES {
                    return Err(Failure::limit(format!(
                        "the transaction touches more than {} files",
                        limits::FILES
                    )));
                }
                // A file is read only as far as the allowance left, and
                // once that is spent nothing more is read.
                let spent = || {
                    Failure::limit(format!(
                        "the touched files hold more than {} bytes",
                        limits::READ_BYTES
                    ))
                };
                let file_limit = u64::try_from(limits().max_input_bytes).unwrap_or(u64::MAX);
                let left = limits::READ_BYTES.saturating_sub(self.read_bytes);
                let bytes = match root.read(path, file_limit.min(left)) {
                    Ok(bytes) => bytes,
                    Err(AccessError::TooLarge(_)) if left < file_limit => {
                        self.spent = true;
                        return Err(spent());
                    }
                    Err(error) => return Err(Failure::access(&error)),
                };
                let base = match bytes {
                    Some(bytes) => {
                        self.read_bytes += u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                        let Ok(text) = String::from_utf8(bytes) else {
                            // Read once: later operations get the same
                            // answer without reading it again.
                            let reason = format!("{path} is not UTF-8 and cannot be edited");
                            self.unreadable.insert(path.to_owned(), reason.clone());
                            return Err(Failure::conflict(reason));
                        };
                        Some(text)
                    }
                    None => None,
                };
                let current = base.clone().map_or(Current::Absent, Current::Text);
                Ok(vacant.insert(Entry {
                    base,
                    current,
                    inverse: Vec::new(),
                }))
            }
        }
    }

    /// Whether a budget was exceeded, which was reported as the failure of
    /// the operation that exceeded it; nothing more can be carried out.
    pub fn exhausted(&self) -> bool {
        self.spent
    }

    pub fn exists(&mut self, path: &str) -> Outcome<bool> {
        Ok(self.entry(path)?.current.text().is_some())
    }

    /// Counts work against the budget. Every charge costs at least one
    /// unit.
    fn charge(&mut self, units: usize) -> Outcome<()> {
        self.work = self
            .work
            .saturating_add(u64::try_from(units.max(1)).unwrap_or(u64::MAX));
        if self.work > limits::WORK {
            self.spent = true;
            return Err(Failure::limit(format!(
                "the transaction needs more than {} bytes of work: its operations, the documents each reads, and its edits",
                limits::WORK
            )));
        }
        Ok(())
    }

    /// Records inverse text; the inverse must fit in a transaction.
    fn record(&mut self, inverse: &[Primitive]) -> Outcome<()> {
        let bytes: usize = inverse
            .iter()
            .map(Primitive::size)
            .fold(0, usize::saturating_add);
        self.inverse_bytes = self
            .inverse_bytes
            .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
        self.inverse_operations = self.inverse_operations.saturating_add(inverse.len());
        if self.inverse_operations > limits::OPERATIONS {
            self.spent = true;
            return Err(Failure::limit(format!(
                "the inverse would hold more than {} operations; split the transaction",
                limits::OPERATIONS
            )));
        }
        let limit = u64::try_from(limits().max_input_bytes).unwrap_or(u64::MAX);
        if self.inverse_bytes > limit {
            self.spent = true;
            return Err(Failure::limit(format!(
                "the inverse would hold more than {limit} bytes; split the transaction"
            )));
        }
        Ok(())
    }

    /// The parsed current document of `path`.
    pub fn document(&mut self, path: &str) -> Outcome<&Document> {
        let length = match &self.entry(path)?.current {
            Current::Absent => return Err(Failure::conflict(format!("{path} does not exist"))),
            Current::Text(text) => text.len(),
            Current::Parsed(document) => document.source().len(),
        };
        if self.charged.insert(path.to_owned()) {
            self.charge(length)?;
        }
        let entry = self.entry(path)?;
        if let Current::Text(text) = &entry.current {
            let document = Document::parse(text.as_str(), &limits()).map_err(|error| {
                Failure::conflict(format!(
                    "{path} is not valid JSONC ({error}); it cannot be edited"
                ))
            })?;
            entry.current = Current::Parsed(document);
        }
        match &entry.current {
            Current::Parsed(document) => Ok(document),
            Current::Absent | Current::Text(_) => {
                Err(Failure::conflict(format!("{path} cannot be edited")))
            }
        }
    }

    /// Replaces the text of `path`, which must parse, and records the
    /// inverses of the edit.
    fn commit(&mut self, path: &str, text: String, inverse: Vec<Primitive>) -> Outcome<()> {
        self.charge(text.len())?;
        // A file the transaction created is undone by deleting it, so the
        // inverses of its edits are neither kept nor counted.
        let created = self.entry(path)?.base.is_none();
        if !created {
            self.record(&inverse)?;
        }
        let document = Document::parse(text, &limits()).map_err(|error| {
            Failure::conflict(format!("the edit leaves {path} invalid ({error})"))
        })?;
        let entry = self.entry(path)?;
        let before = std::mem::replace(&mut entry.current, Current::Parsed(document));
        let length = entry.inverse.len();
        if !created {
            entry.inverse.extend(inverse);
        }
        self.keep(path, before, length);
        Ok(())
    }

    /// Creates `path` with `text`, which must parse as JSONC.
    pub fn create(&mut self, path: &str, text: String) -> Outcome<()> {
        if self.exists(path)? {
            return Err(Failure::conflict(format!("{path} already exists")));
        }
        self.commit(path, text, vec![Primitive::Delete])
    }

    /// Deletes `path` and returns its text.
    pub fn delete(&mut self, path: &str) -> Outcome<String> {
        let entry = self.entry(path)?;
        let before = std::mem::replace(&mut entry.current, Current::Absent);
        let text = match &before {
            Current::Absent => return Err(Failure::conflict(format!("{path} does not exist"))),
            Current::Text(text) => text.clone(),
            Current::Parsed(document) => document.source().to_owned(),
        };
        let length = entry.inverse.len();
        let created = entry.base.is_none();
        let inverse = Primitive::Create { text: text.clone() };
        if !created {
            entry.inverse.push(inverse.clone());
        }
        self.keep(path, before, length);
        if !created {
            self.record(std::slice::from_ref(&inverse))?;
        }
        Ok(text)
    }

    /// Replaces the value at `pointer`.
    pub fn set(&mut self, path: &str, pointer: &[Step], value: &str) -> Outcome<()> {
        let (text, old) = {
            let document = self.document(path)?;
            let target = splice::find(document.root(), pointer)
                .ok_or_else(|| Failure::conflict(format!("{path} has no value there")))?;
            (
                splice::splice(document.source(), target.span(), value),
                document.text(target.span()).to_owned(),
            )
        };
        self.commit(
            path,
            text,
            vec![Primitive::Set {
                pointer: pointer.to_vec(),
                value: old,
            }],
        )
    }

    /// Inserts an item at `index` of the container at `container`.
    pub fn insert(
        &mut self,
        path: &str,
        container: &[Step],
        index: usize,
        insertion: &Insertion<'_>,
    ) -> Outcome<()> {
        let (text, inverse) = {
            let document = self.document(path)?;
            let value = splice::find(document.root(), container)
                .ok_or_else(|| Failure::conflict(format!("{path} has no container there")))?;
            let inserted = splice::insert(document.source(), value, index, insertion)
                .map_err(|message| Failure::conflict(format!("{path}: {message}")))?;
            let mut pointer = container.to_vec();
            pointer.push(match insertion.key {
                Some((key, _)) => Step::Key(key.to_owned()),
                None => Step::Index(index),
            });
            (
                inserted.text,
                Primitive::Remove {
                    pointer,
                    separator: inserted.region,
                },
            )
        };
        self.commit(path, text, vec![inverse])
    }

    /// Removes the value at `pointer`: an array item or an object member.
    pub fn remove(&mut self, path: &str, pointer: &[Step], separator: Option<&str>) -> Outcome<()> {
        let (text, inverse) = {
            let document = self.document(path)?;
            let Some((last, container_pointer)) = pointer.split_last() else {
                return Err(Failure::conflict("the root value cannot be removed"));
            };
            let container = splice::find(document.root(), container_pointer)
                .ok_or_else(|| Failure::conflict(format!("{path} has no value there")))?;
            let index = match (last, container) {
                (Step::Index(index), Value::Array { items, .. }) if *index < items.len() => *index,
                (Step::Key(key), Value::Object { members, .. }) => members
                    .iter()
                    .position(|member| &member.key == key)
                    .ok_or_else(|| {
                        Failure::conflict(format!(
                            "{path} has no member {}",
                            crate::diagnostic::quoted(key)
                        ))
                    })?,
                _ => return Err(Failure::conflict(format!("{path} has no value there"))),
            };
            let removed = splice::remove(document.source(), container, index, separator)
                .map_err(|message| Failure::conflict(format!("{path}: {message}")))?;
            (
                removed.text,
                Primitive::Insert {
                    pointer: container_pointer.to_vec(),
                    index,
                    key: removed.key,
                    value: removed.value,
                    layout: removed.layout,
                },
            )
        };
        self.commit(path, text, vec![inverse])
    }

    /// Replaces several disjoint spans of `path` at once; `inverse` undoes
    /// the whole edit.
    pub fn replace_spans(
        &mut self,
        path: &str,
        mut edits: Vec<(Span, String)>,
        inverse: Vec<Primitive>,
    ) -> Outcome<()> {
        if edits.is_empty() {
            return Ok(());
        }
        let text = splice::splice_all(self.document(path)?.source(), &mut edits);
        self.commit(path, text, inverse)
    }
}
