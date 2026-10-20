// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! The files a transaction touches, in memory, and the primitive edits that
//! change them. Each primitive records its exact inverse.

use crate::{
    Code, limits,
    splice::{self, Insertion, Layout, Step},
};
use std::collections::BTreeMap;
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

pub(crate) struct Workspace<'r> {
    root: &'r SourceRoot,
    pub entries: BTreeMap<String, Entry>,
    read_bytes: u64,
    work: u64,
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
        }
    }

    /// The entry of `path`, reading the file the first time.
    fn entry(&mut self, path: &str) -> Outcome<&mut Entry> {
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
                if self.read_bytes >= limits::READ_BYTES {
                    return Err(spent());
                }
                let file_limit = u64::try_from(limits().max_input_bytes).unwrap_or(u64::MAX);
                let left = limits::READ_BYTES - self.read_bytes;
                let bytes = match root.read(path, file_limit.min(left)) {
                    Ok(bytes) => bytes,
                    Err(AccessError::TooLarge(_)) if left < file_limit => {
                        self.read_bytes = limits::READ_BYTES;
                        return Err(spent());
                    }
                    Err(error) => return Err(Failure::access(&error)),
                };
                let base = match bytes {
                    Some(bytes) => {
                        self.read_bytes += u64::try_from(bytes.len()).unwrap_or(u64::MAX);
                        Some(String::from_utf8(bytes).map_err(|_| {
                            Failure::conflict(format!("{path} is not UTF-8 and cannot be edited"))
                        })?)
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

    /// Whether the work or read budget is spent; nothing more can be
    /// carried out.
    pub fn exhausted(&self) -> bool {
        self.work > limits::WORK || self.read_bytes >= limits::READ_BYTES
    }

    pub fn exists(&mut self, path: &str) -> Outcome<bool> {
        Ok(self.entry(path)?.current.text().is_some())
    }

    /// Counts work against the editing budget: bytes parsed or compared,
    /// and items scanned.
    pub fn charge(&mut self, units: usize) -> Outcome<()> {
        // Every charge costs at least one unit, so even operations on a
        // document already parsed use up the budget.
        self.work = self
            .work
            .saturating_add(u64::try_from(units.max(1)).unwrap_or(u64::MAX));
        if self.work > limits::WORK {
            return Err(Failure::limit(format!(
                "the transaction needs more than {} units of work: bytes parsed or compared and items scanned",
                limits::WORK
            )));
        }
        Ok(())
    }

    /// The parsed current document of `path`.
    pub fn document(&mut self, path: &str) -> Outcome<&Document> {
        let length = match &self.entry(path)?.current {
            Current::Absent => return Err(Failure::conflict(format!("{path} does not exist"))),
            Current::Text(text) => text.len(),
            Current::Parsed(_) => 0,
        };
        self.charge(length)?;
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
        let document = Document::parse(text, &limits()).map_err(|error| {
            Failure::conflict(format!("the edit leaves {path} invalid ({error})"))
        })?;
        let entry = self.entry(path)?;
        entry.current = Current::Parsed(document);
        entry.inverse.extend(inverse);
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
        let text = match std::mem::replace(&mut entry.current, Current::Absent) {
            Current::Absent => return Err(Failure::conflict(format!("{path} does not exist"))),
            Current::Text(text) => text,
            Current::Parsed(document) => document.source().to_owned(),
        };
        entry.inverse.push(Primitive::Create { text: text.clone() });
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
                    .ok_or_else(|| Failure::conflict(format!("{path} has no member \"{key}\"")))?,
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
