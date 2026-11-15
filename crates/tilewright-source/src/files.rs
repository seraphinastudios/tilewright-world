// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Bounded access to a source tree. Every path is relative to the root,
//! `/`-separated and validated; symbolic links are never followed. On Unix
//! every component is opened through its parent's directory handle without
//! following links (`beneath`); elsewhere components are checked by path
//! before use (`portable`), on Windows for any reparse point.

#[cfg(unix)]
pub(crate) mod beneath;
#[cfg(target_os = "linux")]
pub(crate) mod mounts;
#[cfg(any(not(unix), test))]
pub(crate) mod portable;

#[cfg(unix)]
pub(crate) use beneath as platform;
#[cfg(not(unix))]
pub(crate) use portable as platform;

use crate::{
    Code, Diagnostic, LoadOptions, Overlay,
    diagnostic::{Diagnostics, Positions},
};
use sha2::{Digest, Sha256};
#[cfg(all(test, target_os = "linux"))]
use std::path::Path;
use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    io::Read as _,
    rc::Rc,
};
use tilewright_jsonc::{Document, ErrorCode};

/// Why a path beneath the root could not be used.
#[derive(Debug)]
pub(crate) enum Refused {
    /// A symbolic link, at this `/`-joined prefix of the path.
    Link(String),
    Missing,
    /// The entry is not of the kind asked for.
    Unexpected,
    Io(std::io::Error),
}

/// What a path names, without following a link.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Found {
    File,
    Directory,
    Other,
    Missing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RawKind {
    File,
    Directory,
    Link,
    Other,
}

/// A directory entry; `name` is `None` when it is not UTF-8.
#[derive(Debug)]
pub(crate) struct RawEntry {
    pub name: Option<String>,
    pub kind: RawKind,
}

/// The components of a relative path.
pub(crate) fn components(path: &str) -> Vec<&str> {
    path.split('/')
        .filter(|component| !component.is_empty())
        .collect()
}

/// A parsed source file.
pub(crate) struct Source {
    pub path: String,
    pub document: Document,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EntryKind {
    File,
    Directory,
    /// Neither a regular file nor a directory, such as a FIFO.
    Other,
}

pub(crate) struct Entry {
    pub name: String,
    pub kind: EntryKind,
}

/// A file a load read: its length and SHA-256, as read.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SourceFile {
    length: u64,
    sha256: [u8; 32],
}

impl SourceFile {
    /// Its length in bytes.
    #[must_use]
    pub const fn length(&self) -> u64 {
        self.length
    }

    /// The SHA-256 of its bytes.
    #[must_use]
    pub const fn sha256(&self) -> [u8; 32] {
        self.sha256
    }
}

pub(crate) struct Tree<'o> {
    root: platform::Root,
    options: &'o LoadOptions,
    overlay: &'o Overlay,
    files: usize,
    entries: usize,
    bytes: u64,
    digests: BTreeMap<String, SourceFile>,
    diagnostics: Diagnostics,
    exhausted: bool,
}

/// The most components, and bytes, of a path a document may have
/// (`docs/spec/FORMAT.md` section 2). A directory deeper is not looked
/// into: it counts as a file.
const PATH_COMPONENTS: usize = 32;
const PATH_BYTES: usize = 1_024;

/// What a directory holds, as far as it is looked into: nothing, a file,
/// or something else that counts as one, described with its path.
pub(crate) type Holding = Result<(), Option<String>>;

/// Whether `path` is a safe relative path ending in `.jsonc`: at most 1,024
/// bytes and 32 `/`-separated components of at most 128 ASCII letters,
/// digits, `_`, `.` and `-`, none starting with `.` or `-`.
#[must_use]
pub fn is_jsonc_path(path: &str) -> bool {
    is_safe_relative(path) && path.as_bytes().ends_with(b".jsonc")
}

/// Whether `path` is a safe relative path: `/`-separated components of
/// ASCII letters, digits, `_`, `.` and `-`, none starting with `.` or `-`.
pub(crate) fn is_safe_relative(path: &str) -> bool {
    let components: Vec<&str> = path.split('/').collect();
    path.len() <= 1024
        && components.len() <= 32
        && components.iter().all(|component| {
            let bytes = component.as_bytes();
            !bytes.is_empty()
                && bytes.len() <= 128
                && (bytes[0].is_ascii_alphanumeric() || bytes[0] == b'_')
                && bytes
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(byte))
        })
}

/// The overlay of a load that edits nothing.
#[cfg(all(test, target_os = "linux"))]
static NO_OVERLAY: Overlay = BTreeMap::new();

impl<'o> Tree<'o> {
    #[cfg(all(test, target_os = "linux"))]
    pub fn new(root: &Path, options: &'o LoadOptions) -> Self {
        Self::in_root(
            platform::Root::new(root),
            options,
            &NO_OVERLAY,
            Rc::new(Cell::new(0)),
        )
    }

    /// A tree over an opened root whose files are those on disk with
    /// `overlay` applied: an entry's text replaces or creates the file at
    /// its path, and an entry of `None` removes it. Directories that hold
    /// an overlay file exist. `total` is the load's shared diagnostic
    /// count; once it reaches the maximum, nothing more is read.
    pub fn in_root(
        root: platform::Root,
        options: &'o LoadOptions,
        overlay: &'o Overlay,
        total: Rc<Cell<usize>>,
    ) -> Self {
        Self {
            root,
            options,
            overlay,
            files: 0,
            entries: 0,
            bytes: 0,
            digests: BTreeMap::new(),
            diagnostics: Diagnostics::sharing(options.maximum_diagnostics, total),
            exhausted: false,
        }
    }

    /// Reports the exhausted budget once.
    fn exhaust(&mut self, path: &str) {
        if !self.exhausted {
            self.exhausted = true;
            self.diagnostics.push(Diagnostic::file(
                path,
                Code::Budget,
                format!(
                    "the load exceeds {} files, {} directory entries or {} bytes",
                    self.options.maximum_files,
                    self.options.maximum_entries,
                    self.options.maximum_total_bytes
                ),
            ));
        }
    }

    /// Whether one more file of `length` bytes fits the load's budget.
    fn within_budget(&self, length: u64) -> bool {
        !self.exhausted
            && self.files < self.options.maximum_files
            && self.bytes.saturating_add(length) <= self.options.maximum_total_bytes
    }

    pub fn take_diagnostics(&mut self) -> Diagnostics {
        let total = self.diagnostics.total();
        std::mem::replace(
            &mut self.diagnostics,
            Diagnostics::sharing(self.options.maximum_diagnostics, total),
        )
    }

    /// Whether the load's diagnostics are full, so reading on is useless.
    fn stopped(&self) -> bool {
        self.diagnostics.is_full()
    }

    /// Reports a symbolic link at `path`.
    fn link(&mut self, path: &str) {
        self.diagnostics.push(Diagnostic::file(
            path,
            Code::Symlink,
            "symbolic links are not followed",
        ));
    }

    /// Whether a regular file or directory exists at `path`. A symbolic
    /// link anywhere in the path is reported and counts as neither.
    pub fn kind(&mut self, path: &str) -> Option<EntryKind> {
        match self.overlay.get(path) {
            Some(Some(_)) => return Some(EntryKind::File),
            Some(None) => return None,
            None => {}
        }
        match self.root.kind(&components(path)) {
            Ok(Found::File) => Some(EntryKind::File),
            Ok(Found::Directory) => Some(EntryKind::Directory),
            Ok(Found::Other) => Some(EntryKind::Other),
            Ok(Found::Missing) | Err(Refused::Missing | Refused::Unexpected) => self
                .holds_overlay_files(path)
                .then_some(EntryKind::Directory),
            Err(Refused::Link(link)) => {
                self.link(&link);
                None
            }
            Err(Refused::Io(error)) => {
                self.diagnostics.push(Diagnostic::file(
                    path,
                    Code::Io,
                    format!("cannot read file type: {error}"),
                ));
                None
            }
        }
    }

    /// Whether the overlay puts a file beneath the directory `path`.
    fn holds_overlay_files(&self, path: &str) -> bool {
        overlay_beneath(self.overlay, path).any(|(_, value)| value.is_some())
    }

    /// Whether the optional directory `path` is present. An entry of
    /// another kind there is reported, not treated as absent.
    pub fn container(&mut self, path: &str) -> bool {
        match self.kind(path) {
            Some(EntryKind::Directory) => true,
            Some(EntryKind::File | EntryKind::Other) => {
                self.diagnostics.push(Diagnostic::file(
                    path,
                    Code::UnexpectedFile,
                    "must be a directory",
                ));
                false
            }
            None => false,
        }
    }

    /// Lists a directory, sorted by name. Symbolic links, special files and
    /// non-UTF-8 names are reported and skipped.
    pub fn list(&mut self, path: &str) -> Vec<Entry> {
        self.listing(path, true).0
    }

    /// Whether the directory `path` holds no file at any depth, with the
    /// overlay applied: nothing, or only directories that hold none, which
    /// the layout ignores (`docs/spec/FORMAT.md` section 2). A symbolic
    /// link, a special file, a name that is not UTF-8, a directory that
    /// cannot be listed, and a directory deeper than a document's path may
    /// be, count as files; for those, what was found, and where. Nothing is
    /// reported. Directories are looked into depth first and in name order,
    /// each listed in full and counted against the budget as any listing
    /// is, so neither the answer nor what it costs depends on the order a
    /// file system lists entries in, and the walk goes no deeper than a
    /// path may: what it holds at once is the names left at each level.
    pub fn holds_no_file(&mut self, path: &str) -> Holding {
        let mut current = path.to_owned();
        // For each directory on the way down: the names of those in it left
        // to look into, the last first, and the length of its path.
        let mut levels: Vec<(Vec<String>, usize)> = Vec::new();
        loop {
            if current.len() > PATH_BYTES || current.split('/').count() > PATH_COMPONENTS {
                return Err(Some(format!(
                    "a directory deeper than a document's path may be, at {current}"
                )));
            }
            let (entries, other) = self.listing(&current, false);
            if let Some(other) = other {
                return Err(Some(other));
            }
            let mut names = Vec::with_capacity(entries.len());
            for entry in entries {
                if entry.kind != EntryKind::Directory {
                    return Err(None);
                }
                names.push(entry.name);
            }
            names.reverse();
            levels.push((names, current.len()));
            // The next directory to look into, depth first, in name order.
            loop {
                let Some((names, length)) = levels.last_mut() else {
                    return Ok(());
                };
                if let Some(name) = names.pop() {
                    current.truncate(*length);
                    current.push('/');
                    current.push_str(&name);
                    break;
                }
                levels.pop();
            }
        }
    }

    /// Lists a directory, sorted by name, and says what it held that is
    /// neither a file nor a directory, or why it could not be listed in
    /// full, as a description with its path: of all there were, the first
    /// by that description, so that it does not depend on listing order.
    /// That is reported as well only when `report`.
    fn listing(&mut self, path: &str, report: bool) -> (Vec<Entry>, Option<String>) {
        let cut_short = || Some(format!("a directory not listed in full, at {path}"));
        if self.stopped() {
            if report {
                self.diagnostics.skip();
            }
            return (Vec::new(), cut_short());
        }
        // Overlay files replace, add or remove entries; a directory that
        // holds an overlay file is listed. Names are looked up in sets, so
        // merging costs a logarithm per entry.
        let mut added: BTreeMap<&str, EntryKind> = BTreeMap::new();
        let remaining = self.options.maximum_entries.saturating_sub(self.entries);
        for (rest, value) in overlay_beneath(self.overlay, path) {
            match (rest.split_once('/'), value) {
                (None, Some(_)) => {
                    added.insert(rest, EntryKind::File);
                }
                (Some((directory, _)), Some(_)) => {
                    added.entry(directory).or_insert(EntryKind::Directory);
                }
                (_, None) => {}
            }
            // Each name added is counted once, here or on disk, so more
            // names than the budget has left exhaust it whatever is there.
            if added.len() > remaining {
                self.exhaust(path);
                return (Vec::new(), cut_short());
            }
        }
        // An entry the overlay replaces or removes is not looked at at
        // all, so what is there on disk, a link, a special file or a
        // directory, is not reported.
        let (mut entries, mut other) = self.list_disk(path, report);
        let present: BTreeSet<String> = entries.iter().map(|entry| entry.name.clone()).collect();
        for (name, kind) in added {
            if !present.contains(name) {
                self.entries += 1;
                if self.entries > self.options.maximum_entries {
                    self.exhaust(path);
                    other = cut_short();
                    break;
                }
                entries.push(Entry {
                    name: name.to_owned(),
                    kind,
                });
            }
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        (entries, other)
    }

    /// Opens the directory `path` on disk for listing: `None` when it is
    /// missing but holds overlay files, so is empty there; otherwise what
    /// stops it being listed, reported when `report`.
    fn open_listing(
        &mut self,
        path: &str,
        report: bool,
    ) -> Result<Option<platform::Listing>, String> {
        match self.root.list(&components(path)) {
            Ok(directory) => Ok(Some(directory)),
            Err(Refused::Link(link)) => {
                if report {
                    self.link(&link);
                }
                Err(format!("a symbolic link, at {link}"))
            }
            Err(Refused::Missing) if self.holds_overlay_files(path) => Ok(None),
            Err(refused) => {
                let reason = match refused {
                    Refused::Io(error) => error.to_string(),
                    Refused::Missing => "it is missing".to_owned(),
                    Refused::Unexpected | Refused::Link(_) => "not a directory".to_owned(),
                };
                if report {
                    self.diagnostics.push(Diagnostic::file(
                        path,
                        Code::Io,
                        format!("cannot list directory: {reason}"),
                    ));
                }
                Err(format!("a directory that cannot be listed, at {path}"))
            }
        }
    }

    /// Lists a directory on disk, and says what it held that is neither a
    /// file nor a directory, or why it could not be listed in full, as
    /// [`Self::listing`] does, reporting it when `report`. A directory that
    /// is missing on disk but holds overlay files is empty there. An entry
    /// the overlay replaces or removes is counted then, or not at all.
    fn list_disk(&mut self, path: &str, report: bool) -> (Vec<Entry>, Option<String>) {
        /// Keeps the first of the descriptions found.
        fn first(other: &mut Option<String>, found: String) {
            if other.as_ref().is_none_or(|other| found < *other) {
                *other = Some(found);
            }
        }
        let overlay = self.overlay;
        let directory = match self.open_listing(path, report) {
            Ok(Some(directory)) => directory,
            Ok(None) => return (Vec::new(), None),
            Err(found) => return (Vec::new(), Some(found)),
        };
        let mut entries = Vec::new();
        let mut other = None;
        for entry in directory {
            if self.exhausted {
                first(
                    &mut other,
                    format!("a directory not listed in full, at {path}"),
                );
                break;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    first(
                        &mut other,
                        format!("a directory that cannot be listed in full, at {path}"),
                    );
                    if report {
                        self.diagnostics.push(Diagnostic::file(
                            path,
                            Code::Io,
                            format!("cannot list directory: {error}"),
                        ));
                    }
                    continue;
                }
            };
            // The overlay's entry takes its place, and is counted then, if
            // it adds one: so the count does not depend on where in the
            // listing this one came.
            if let Some(name) = &entry.name
                && overlay.contains_key(&join(path, name))
            {
                continue;
            }
            self.entries += 1;
            if self.entries > self.options.maximum_entries {
                self.exhaust(path);
                first(
                    &mut other,
                    format!("a directory not listed in full, at {path}"),
                );
                break;
            }
            let Some(name) = entry.name else {
                first(
                    &mut other,
                    format!("an entry whose name is not UTF-8, in {path}"),
                );
                if report {
                    self.diagnostics.push(Diagnostic::file(
                        path,
                        Code::UnexpectedFile,
                        "file name is not UTF-8",
                    ));
                }
                continue;
            };
            let child = join(path, &name);
            match entry.kind {
                RawKind::Link => {
                    first(&mut other, format!("a symbolic link, at {child}"));
                    if report {
                        self.link(&child);
                    }
                }
                RawKind::Directory => entries.push(Entry {
                    name,
                    kind: EntryKind::Directory,
                }),
                RawKind::File => entries.push(Entry {
                    name,
                    kind: EntryKind::File,
                }),
                RawKind::Other => {
                    first(&mut other, format!("a special file, at {child}"));
                    if report {
                        self.diagnostics.push(Diagnostic::file(
                            &child,
                            Code::UnexpectedFile,
                            "not a regular file or directory",
                        ));
                    }
                }
            }
        }
        (entries, other)
    }

    /// Reads and parses a JSONC document. Every problem is reported.
    pub fn document(&mut self, path: &str) -> Option<Source> {
        if self.stopped() {
            self.diagnostics.skip();
            return None;
        }
        match self.overlay.get(path) {
            Some(Some(text)) => return self.overlay_document(path, text),
            Some(None) => {
                self.diagnostics
                    .push(Diagnostic::file(path, Code::MissingFile, "file is missing"));
                return None;
            }
            None => {}
        }
        // The file is opened first and its opened handle checked, so what is
        // checked is what is read.
        let (file, length) = match self.root.file(&components(path)) {
            Ok(opened) => opened,
            Err(Refused::Link(link)) => {
                self.link(&link);
                return None;
            }
            Err(Refused::Unexpected) => {
                self.diagnostics.push(Diagnostic::file(
                    path,
                    Code::UnexpectedFile,
                    "expected a regular file",
                ));
                return None;
            }
            Err(Refused::Missing) => {
                self.diagnostics
                    .push(Diagnostic::file(path, Code::MissingFile, "file is missing"));
                return None;
            }
            Err(Refused::Io(error)) => {
                self.diagnostics.push(Diagnostic::file(
                    path,
                    Code::Io,
                    format!("cannot read file: {error}"),
                ));
                return None;
            }
        };
        let limit = u64::try_from(self.options.limits.max_input_bytes).unwrap_or(u64::MAX);
        if length > limit {
            self.diagnostics.push(Diagnostic::file(
                path,
                ErrorCode::InputTooLarge,
                format!("file is larger than {limit} bytes"),
            ));
            return None;
        }
        if !self.within_budget(length) {
            self.exhaust(path);
            return None;
        }
        // Read at most one byte past the file limit or the remaining budget,
        // so a file that grows after the size check cannot force an
        // unbounded read; its actual length is checked again below.
        let remaining = self.options.maximum_total_bytes.saturating_sub(self.bytes);
        let mut bytes = Vec::new();
        let bytes = match file
            .take(limit.min(remaining).saturating_add(1))
            .read_to_end(&mut bytes)
        {
            Ok(_) => bytes,
            Err(error) => {
                self.diagnostics.push(Diagnostic::file(
                    path,
                    Code::Io,
                    format!("cannot read file: {error}"),
                ));
                return None;
            }
        };
        let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        if length > limit {
            self.diagnostics.push(Diagnostic::file(
                path,
                ErrorCode::InputTooLarge,
                format!("file is larger than {limit} bytes"),
            ));
            return None;
        }
        if !self.within_budget(length) {
            self.exhaust(path);
            return None;
        }
        self.parse(path, bytes)
    }

    /// Reads an overlay file under the same limits and budget as a file on
    /// disk.
    fn overlay_document(&mut self, path: &str, text: &str) -> Option<Source> {
        let limit = u64::try_from(self.options.limits.max_input_bytes).unwrap_or(u64::MAX);
        let length = u64::try_from(text.len()).unwrap_or(u64::MAX);
        if length > limit {
            self.diagnostics.push(Diagnostic::file(
                path,
                ErrorCode::InputTooLarge,
                format!("file is larger than {limit} bytes"),
            ));
            return None;
        }
        if !self.within_budget(length) {
            self.exhaust(path);
            return None;
        }
        self.parse(path, text.as_bytes().to_vec())
    }

    /// Counts, digests and parses the bytes of a file that fits the budget.
    fn parse(&mut self, path: &str, bytes: Vec<u8>) -> Option<Source> {
        let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        self.files += 1;
        self.bytes = self.bytes.saturating_add(length);
        self.digests.insert(
            path.to_owned(),
            SourceFile {
                length,
                sha256: Sha256::digest(&bytes).into(),
            },
        );
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) => {
                self.diagnostics.push(Diagnostic::file(
                    path,
                    Code::Encoding,
                    format!("not UTF-8 at byte {}", error.utf8_error().valid_up_to()),
                ));
                return None;
            }
        };
        match Document::parse(text.as_str(), &self.options.limits) {
            Ok(document) => Some(Source {
                path: path.to_owned(),
                document,
            }),
            Err(error) => {
                self.diagnostics.push(Diagnostic::at(
                    path,
                    &Positions::new(&text),
                    error.offset,
                    error.offset,
                    error.code,
                    "invalid JSONC",
                ));
                None
            }
        }
    }

    /// SHA-256 over every file read: for each path in byte order, the UTF-8
    /// path, a zero byte, the length as little-endian `u64`, and the SHA-256
    /// of the file's bytes.
    pub fn digest(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        for (path, file) in &self.digests {
            hasher.update(path.as_bytes());
            hasher.update([0]);
            hasher.update(file.length.to_le_bytes());
            hasher.update(file.sha256);
        }
        hasher.finalize().into()
    }

    /// The files read, which [`Tree::digest`] covers.
    pub fn take_files(&mut self) -> BTreeMap<String, SourceFile> {
        std::mem::take(&mut self.digests)
    }
}

/// The overlay entries beneath the directory `path`, with the rest of
/// their paths.
fn overlay_beneath<'a>(
    overlay: &'a Overlay,
    path: &str,
) -> impl Iterator<Item = (&'a str, &'a Option<String>)> + 'a {
    let prefix = if path.is_empty() {
        String::new()
    } else {
        format!("{path}/")
    };
    overlay
        .range(prefix.clone()..)
        .map_while(move |(key, value)| key.strip_prefix(prefix.as_str()).map(|rest| (rest, value)))
}

/// Whether an entry at `path`, which the layout has no place for, is
/// ignored rather than an error (`docs/spec/FORMAT.md` section 2): a
/// directory that holds no file at any depth. Among `documents`, those of
/// a plane, a layer or a definition type, a directory named like one,
/// ending in `.jsonc` in any case, is not ignored: it is where the layout
/// has a file, or one a file system that ignores case takes for it. Nor,
/// at any level the layout lists, is one at a name reserved for staging,
/// the form `.<name>.tilewright-tmp` in any case, that publishing removes
/// as a file: no identifier starts with `.`, so no plane's or type's
/// directory has such a name. When it is not ignored, `Err` holds what to
/// add to the problem's message: why, when what it holds is not a file.
pub(crate) fn ignored(
    tree: &mut Tree<'_>,
    path: &str,
    entry: &Entry,
    documents: bool,
) -> Result<(), String> {
    let ends = |suffix: &str| {
        let (name, suffix) = (entry.name.as_bytes(), suffix.as_bytes());
        name.len() >= suffix.len() && name[name.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
    };
    let staging = entry.name.starts_with('.') && ends(".tilewright-tmp");
    if entry.kind != EntryKind::Directory || (documents && ends(".jsonc")) || staging {
        return Err(String::new());
    }
    tree.holds_no_file(path).map_err(|found| {
        found.map_or_else(String::new, |found| {
            format!("; it is not ignored as a directory without files, since it holds {found}")
        })
    })
}

pub(crate) fn join(directory: &str, name: &str) -> String {
    if directory.is_empty() {
        name.to_owned()
    } else {
        format!("{directory}/{name}")
    }
}

/// A step a test runs inside a create.
#[cfg(test)]
pub(crate) type Hook = Box<dyn FnMut() -> std::io::Result<()>>;

#[cfg(test)]
thread_local! {
    /// What a create on this thread does right after it made its file,
    /// before writing it, for tests: fail, or change the directory as
    /// another process could meanwhile.
    pub(crate) static AFTER_CREATE: std::cell::RefCell<Option<Hook>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
thread_local! {
    /// The error a portable create's open fails with on this thread, for
    /// tests, instead of opening anything: Windows has errors other than
    /// "already exists" for a name that is taken.
    pub(crate) static OPEN_FAILS: std::cell::Cell<Option<std::io::ErrorKind>> =
        const { std::cell::Cell::new(None) };
}

/// This thread's [`OPEN_FAILS`].
#[cfg(test)]
pub(crate) fn open_fails() -> Option<std::io::ErrorKind> {
    OPEN_FAILS.with(std::cell::Cell::get)
}

/// Runs this thread's [`AFTER_CREATE`], if it has one.
#[cfg(test)]
pub(crate) fn after_create() -> std::io::Result<()> {
    AFTER_CREATE.with(|hook| hook.borrow_mut().as_mut().map_or(Ok(()), |hook| hook()))
}

/// Both access implementations refuse links themselves, at the component
/// where they occur, rather than relying on an earlier check.
#[cfg(all(test, unix))]
mod tests {
    use super::{Found, RawKind, Refused};
    use std::{fs, os::unix::fs::symlink};

    #[test]
    fn an_entry_the_overlay_removes_is_never_counted() {
        use super::{LoadOptions, Overlay, Tree, platform};
        use std::{cell::Cell, rc::Rc};
        let root = tempfile::tempdir().unwrap();
        for index in 0..10 {
            fs::write(root.path().join(format!("{index}_0.jsonc")), "{}").unwrap();
        }
        // The overlay removes every entry the file system lists after the
        // first, which is all the budget holds.
        let mut listing = fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap());
        let first = listing.next().unwrap();
        let overlay: Overlay = listing.map(|name| (name, None)).collect();
        let options = LoadOptions {
            maximum_entries: 1,
            ..LoadOptions::default()
        };
        let total = Rc::new(Cell::new(0));
        let mut tree = Tree::in_root(platform::Root::new(root.path()), &options, &overlay, total);
        let listed = tree.list("");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, first);
        assert!(!tree.exhausted);
    }

    fn tree() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("dir")).unwrap();
        fs::write(root.path().join("dir/file.jsonc"), "{}").unwrap();
        symlink(root.path().join("dir"), root.path().join("link")).unwrap();
        symlink(
            root.path().join("dir/file.jsonc"),
            root.path().join("dir/file-link.jsonc"),
        )
        .unwrap();
        root
    }

    /// The link prefix a refusal names.
    fn link<T>(result: Result<T, Refused>) -> String {
        match result {
            Err(Refused::Link(prefix)) => prefix,
            Err(other) => panic!("expected a link, got {other:?}"),
            Ok(_) => panic!("expected a link, got success"),
        }
    }

    macro_rules! access_tests {
        ($name:ident, $module:ident) => {
            #[test]
            fn $name() {
                let directory = tree();
                let root = super::$module::Root::new(directory.path());
                assert_eq!(root.file(&["dir", "file.jsonc"]).unwrap().1, 2);
                assert_eq!(link(root.file(&["link", "file.jsonc"])), "link");
                assert_eq!(
                    link(root.file(&["dir", "file-link.jsonc"])),
                    "dir/file-link.jsonc"
                );
                assert!(matches!(root.file(&["dir"]), Err(Refused::Unexpected)));
                assert!(matches!(root.file(&["dir", "nope"]), Err(Refused::Missing)));
                assert!(matches!(
                    root.file(&["dir", "file.jsonc", "x"]),
                    Err(Refused::Unexpected)
                ));
                assert_eq!(root.kind(&["dir"]).unwrap(), Found::Directory);
                assert_eq!(root.kind(&["dir", "file.jsonc"]).unwrap(), Found::File);
                assert_eq!(root.kind(&["nope"]).unwrap(), Found::Missing);
                assert_eq!(link(root.kind(&["link"])), "link");
                assert_eq!(link(root.list(&["link"])), "link");
                let mut entries: Vec<(Option<String>, RawKind)> = root
                    .list(&["dir"])
                    .unwrap()
                    .map(|entry| entry.map(|entry| (entry.name, entry.kind)))
                    .collect::<Result<_, _>>()
                    .unwrap();
                entries.sort_by(|left, right| left.0.cmp(&right.0));
                assert_eq!(
                    entries,
                    [
                        (Some("file-link.jsonc".to_owned()), RawKind::Link),
                        (Some("file.jsonc".to_owned()), RawKind::File),
                    ]
                );
            }
        };
    }

    access_tests!(beneath_refuses_links, beneath);
    access_tests!(portable_refuses_links, portable);

    macro_rules! write_tests {
        ($name:ident, $module:ident) => {
            #[test]
            fn $name() {
                let directory = tree();
                let root = super::$module::Root::new(directory.path());
                // Directories are created as asked, never through a link.
                assert!(matches!(
                    root.directory(&["new", "deeper"], false),
                    Err(Refused::Missing)
                ));
                let made = root.directory(&["new", "deeper"], true).unwrap();
                assert!(directory.path().join("new/deeper").is_dir());
                assert_eq!(link(root.directory(&["link", "inside"], true)), "link");
                assert!(!directory.path().join("dir/inside").exists());
                assert!(matches!(
                    root.directory(&["dir", "file.jsonc"], true),
                    Err(Refused::Unexpected)
                ));

                // A write replaces a link or hard link instead of writing
                // through it, and takes the access it is given.
                let dir = root.directory(&["dir"], false).unwrap();
                let outside = tempfile::tempdir().unwrap();
                let target = outside.path().join("target");
                fs::write(&target, "outside").unwrap();
                symlink(&target, directory.path().join("dir/staged")).unwrap();
                dir.write("staged", None, b"new").unwrap();
                assert_eq!(fs::read_to_string(&target).unwrap(), "outside");
                fs::hard_link(&target, directory.path().join("dir/hard")).unwrap();
                dir.write("hard", None, b"new").unwrap();
                assert_eq!(fs::read_to_string(&target).unwrap(), "outside");
                {
                    use std::os::unix::fs::PermissionsExt as _;
                    let file = directory.path().join("dir/file.jsonc");
                    fs::set_permissions(&file, fs::Permissions::from_mode(0o640)).unwrap();
                    let like = dir.access("file.jsonc").unwrap();
                    assert!(dir.access("nope").unwrap().is_none());
                    dir.write("moded", like.as_ref(), b"x").unwrap();
                    let mode = fs::metadata(directory.path().join("dir/moded"))
                        .unwrap()
                        .permissions()
                        .mode();
                    assert_eq!(mode & 0o777, 0o640);
                    // A directory's own access can be read and set.
                    let mut own = dir.own_access().unwrap();
                    own.mode = 0o750;
                    dir.set_own_access(&own).unwrap();
                    assert_eq!(dir.own_access().unwrap(), own);
                }

                dir.rename("staged", "file.jsonc").unwrap();
                assert_eq!(
                    fs::read_to_string(directory.path().join("dir/file.jsonc")).unwrap(),
                    "new"
                );
                assert!(dir.remove_file("hard").unwrap());
                assert!(!dir.remove_file("hard").unwrap());
                assert!(dir.remove_file("file-link.jsonc").unwrap());
                assert!(target.exists());
                dir.sync().unwrap();
                made.sync().unwrap();

                let top = root.directory(&["new"], false).unwrap();
                fs::write(directory.path().join("new/deeper/x"), "").unwrap();
                assert!(!top.remove_directory("deeper").unwrap());
                fs::remove_file(directory.path().join("new/deeper/x")).unwrap();
                assert!(top.remove_directory("deeper").unwrap());
                assert!(!top.remove_directory("deeper").unwrap());

                assert!(root.lock().unwrap().is_some());
                // Making a directory says whether this call made it.
                assert!(root.make_directory(&["made"]).unwrap());
                assert!(!root.make_directory(&["made"]).unwrap());
                assert!(!root.make_directory(&["dir", "file.jsonc"]).unwrap());
                assert!(matches!(
                    root.make_directory(&["nope", "deeper"]),
                    Err(Refused::Missing)
                ));
                // A clone is a handle on the same directory.
                let clone = root.try_clone();
                assert_eq!(clone.kind(&["dir"]).unwrap(), Found::Directory);
            }
        };
    }

    write_tests!(beneath_writes_beneath_the_root, beneath);
    write_tests!(portable_writes_beneath_the_root, portable);

    /// Everything about an entry a create must leave as it was, not
    /// following it: its kind, identity, links, mode, size, times, and its
    /// bytes, link target or entries.
    fn snapshot(path: &std::path::Path) -> String {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = fs::symlink_metadata(path).unwrap();
        let content = if metadata.file_type().is_symlink() {
            format!("-> {}", fs::read_link(path).unwrap().display())
        } else if metadata.is_dir() {
            let mut names: Vec<_> = fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            names.sort();
            format!("{names:?}")
        } else if metadata.is_file() {
            format!("{:?}", fs::read(path).unwrap())
        } else {
            String::new()
        };
        format!(
            "{:?} {} {} {} {:o} {} {} {} {} {content}",
            metadata.file_type(),
            metadata.dev(),
            metadata.ino(),
            metadata.nlink(),
            metadata.mode(),
            metadata.len(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime_nsec(),
        )
    }

    /// Whether a create refused because something was at the name.
    fn already_there<T: std::fmt::Debug>(result: Result<T, Refused>) -> bool {
        type Inner = dyn std::error::Error + Send + Sync;
        match result {
            Err(Refused::Io(error)) => {
                error.kind() == std::io::ErrorKind::AlreadyExists
                    && error
                        .get_ref()
                        .is_some_and(Inner::is::<crate::access::AlreadyThere>)
            }
            other => panic!("expected something already there, got {other:?}"),
        }
    }

    /// Whether a create's error says its file may be left.
    fn left(error: &std::io::Error) -> bool {
        type Inner = dyn std::error::Error + Send + Sync;
        error
            .get_ref()
            .is_some_and(Inner::is::<crate::access::LeftBehind>)
    }

    /// Sets this thread's hook inside creates until it is dropped.
    struct Hooked;

    impl Hooked {
        fn new(hook: impl FnMut() -> std::io::Result<()> + 'static) -> Self {
            super::AFTER_CREATE.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
            Self
        }
    }

    impl Drop for Hooked {
        fn drop(&mut self) {
            super::AFTER_CREATE.with(|slot| *slot.borrow_mut() = None);
        }
    }

    macro_rules! create_tests {
        ($name:ident, $module:ident) => {
            #[test]
            fn $name() {
                use std::os::unix::fs::PermissionsExt as _;
                let directory = tree();
                let root = super::$module::Root::new(directory.path());
                let dir = root.directory(&["dir"], false).unwrap();
                let at = |name: &str| directory.path().join("dir").join(name);

                // A new file has exactly the bytes and the access asked for.
                dir.create("new", None, b"bytes").unwrap();
                assert_eq!(fs::read(at("new")).unwrap(), b"bytes");
                fs::set_permissions(at("file.jsonc"), fs::Permissions::from_mode(0o640)).unwrap();
                let like = dir.access("file.jsonc").unwrap().unwrap();
                dir.create("moded", Some(&like), b"").unwrap();
                assert_eq!(fs::read(at("moded")).unwrap(), b"");
                assert_eq!(
                    fs::metadata(at("moded")).unwrap().permissions().mode() & 0o7777,
                    0o640
                );

                // Nothing already at the name is removed, replaced, followed
                // or written through: a file, a hard link to one outside,
                // directories empty or not, links to a file, a directory or
                // nothing (outside the tree), and a FIFO.
                let outside = tempfile::tempdir().unwrap();
                let target = outside.path().join("target");
                fs::write(&target, "outside").unwrap();
                fs::create_dir(outside.path().join("elsewhere")).unwrap();
                fs::write(at("file"), "keep").unwrap();
                fs::hard_link(&target, at("hard")).unwrap();
                fs::create_dir(at("empty")).unwrap();
                fs::create_dir(at("full")).unwrap();
                fs::write(at("full/inside"), "kept").unwrap();
                symlink(&target, at("to-file")).unwrap();
                symlink(outside.path().join("elsewhere"), at("to-directory")).unwrap();
                symlink(outside.path().join("nothing"), at("dangling")).unwrap();
                symlink("file", at("relative")).unwrap();
                let fifo = std::process::Command::new("mkfifo")
                    .arg(at("fifo"))
                    .status()
                    .unwrap();
                assert!(fifo.success());
                let names = [
                    "new",
                    "file",
                    "hard",
                    "empty",
                    "full",
                    "to-file",
                    "to-directory",
                    "dangling",
                    "relative",
                    "fifo",
                    "file-link.jsonc",
                ];
                let before: Vec<String> = names.iter().map(|name| snapshot(&at(name))).collect();
                let outside_before = snapshot(outside.path());
                for name in names {
                    assert!(already_there(dir.create(name, None, b"x")), "{name}");
                    assert!(already_there(dir.create(name, Some(&like), b"x")), "{name}");
                }
                let after: Vec<String> = names.iter().map(|name| snapshot(&at(name))).collect();
                assert_eq!(before, after);
                assert_eq!(snapshot(outside.path()), outside_before);
                assert_eq!(fs::read_to_string(&target).unwrap(), "outside");
                assert!(!outside.path().join("nothing").exists());

                // Through a link to a directory nothing is made either.
                assert_eq!(link(root.directory(&["link"], false)), "link");
            }
        };
    }

    macro_rules! create_failure_tests {
        ($name:ident, $module:ident, $removes:expr) => {
            #[test]
            fn $name() {
                use std::os::unix::fs::PermissionsExt as _;
                let directory = tree();
                let root = super::$module::Root::new(directory.path());
                let dir = root.directory(&["dir"], false).unwrap();
                let at = |name: &str| directory.path().join("dir").join(name);
                fs::set_permissions(at("file.jsonc"), fs::Permissions::from_mode(0o640)).unwrap();
                let like = dir.access("file.jsonc").unwrap().unwrap();

                // A create that fails after making its file removes the
                // file only if that is still what is at the name (Unix,
                // beneath), or never (portable), and says when it may be
                // left.
                {
                    let _hooked = Hooked::new(|| Err(std::io::ErrorKind::StorageFull.into()));
                    let failed = dir.create("failed", None, b"x");
                    let Err(Refused::Io(error)) = failed else {
                        panic!("expected a failure, got {failed:?}");
                    };
                    assert_eq!(error.kind(), std::io::ErrorKind::StorageFull);
                    let removes: bool = $removes;
                    assert_eq!(at("failed").exists(), !removes);
                    assert_eq!(error.to_string().contains("may be left"), !removes);
                    assert_eq!(left(&error), !removes);
                }
                {
                    // Another process renames the new file away and puts its
                    // own at the name, then the create fails; that file
                    // survives the cleanup.
                    let (moved, name) = (at("moved"), at("swapped"));
                    let _hooked = Hooked::new(move || {
                        fs::rename(&name, &moved)?;
                        fs::write(&name, "someone else's")?;
                        Err(std::io::ErrorKind::StorageFull.into())
                    });
                    let failed = dir.create("swapped", None, b"x");
                    let Err(Refused::Io(error)) = failed else {
                        panic!("expected a failure, got {failed:?}");
                    };
                    assert!(error.to_string().contains("may be left"), "{error}");
                    assert!(left(&error));
                    assert_eq!(fs::read_to_string(at("swapped")).unwrap(), "someone else's");
                    assert!(at("moved").exists());
                }
                {
                    // So does a link it puts there to the new file, now
                    // moved: the cleanup does not follow it.
                    let (moved, name) = (at("moved-ours"), at("linked"));
                    let _hooked = Hooked::new(move || {
                        fs::rename(&name, &moved)?;
                        symlink("moved-ours", &name)?;
                        Err(std::io::ErrorKind::StorageFull.into())
                    });
                    let failed = dir.create("linked", None, b"x");
                    let Err(Refused::Io(error)) = failed else {
                        panic!("expected a failure, got {failed:?}");
                    };
                    assert!(left(&error), "{error}");
                    let linked = fs::symlink_metadata(at("linked")).unwrap();
                    assert!(linked.file_type().is_symlink());
                    assert!(at("moved-ours").is_file());
                }

                // The file is made with its final mode only when it has no
                // access of its own: with access, it is its owner's alone
                // until written, as the hook, before any byte, sees.
                fs::File::create(at("reference")).unwrap();
                let default = fs::metadata(at("reference")).unwrap().permissions().mode() & 0o7777;
                assert_ne!(default, 0o600, "a umask that hides the difference");
                for (name, access, expected) in [
                    ("mode-default", None, default),
                    ("mode-owner", Some(like), 0o600),
                ] {
                    let seen = std::rc::Rc::new(std::cell::Cell::new(None));
                    let (path, found) = (at(name), seen.clone());
                    let _hooked = Hooked::new(move || {
                        let metadata = fs::symlink_metadata(&path)?;
                        found.set(Some((
                            metadata.permissions().mode() & 0o7777,
                            metadata.len(),
                        )));
                        Ok(())
                    });
                    dir.create(name, access.as_ref(), b"x").unwrap();
                    assert_eq!(seen.get(), Some((expected, 0)), "{name}");
                }
            }
        };
    }

    create_tests!(beneath_creates_only_new_files, beneath);
    create_tests!(portable_creates_only_new_files, portable);
    create_failure_tests!(beneath_removes_only_the_file_it_made, beneath, true);
    create_failure_tests!(portable_never_removes_the_file_it_made, portable, false);

    /// Whether a result is a create's refusal of a taken name.
    fn taken(result: &Result<(), Refused>) -> bool {
        type Inner = dyn std::error::Error + Send + Sync;
        matches!(result, Err(Refused::Io(error))
            if error.get_ref().is_some_and(Inner::is::<crate::access::AlreadyThere>))
    }

    /// A portable create whose open fails says the name is taken only when
    /// something is at it.
    #[test]
    fn a_portable_create_that_cannot_open_is_taken_only_if_something_is_there() {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tree();
        let root = super::portable::Root::new(directory.path());
        let dir = root.directory(&["dir"], false).unwrap();
        // An error other than "already exists", as Windows gives for some
        // taken names.
        super::OPEN_FAILS.with(|fails| fails.set(Some(std::io::ErrorKind::PermissionDenied)));
        let free = dir.create("free", None, b"x");
        let held = dir.create("file.jsonc", None, b"x");
        super::OPEN_FAILS.with(|fails| fails.set(None));
        assert!(
            matches!(&free, Err(Refused::Io(error)) if error.kind() == std::io::ErrorKind::PermissionDenied),
            "{free:?}"
        );
        assert!(!taken(&free));
        assert!(taken(&held), "{held:?}");
        // Real failures with nothing at the name: a directory removed
        // since it was opened, and one that cannot be written.
        fs::create_dir(directory.path().join("gone")).unwrap();
        let gone = root.directory(&["gone"], false).unwrap();
        fs::remove_dir(directory.path().join("gone")).unwrap();
        let missing = gone.create("x", None, b"x");
        assert!(missing.is_err() && !taken(&missing), "{missing:?}");
        if rustix::process::geteuid().as_raw() != 0 {
            let locked = directory.path().join("locked");
            fs::create_dir(&locked).unwrap();
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
            let refused = root
                .directory(&["locked"], false)
                .unwrap()
                .create("x", None, b"x");
            fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
            assert!(refused.is_err() && !taken(&refused), "{refused:?}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_written_file_with_access_has_no_inherited_access_control_list() {
        use std::os::unix::fs::PermissionsExt as _;
        let directory = tree();
        // A default list for new files: user::rwx, user:nobody:r--,
        // group::r-x, mask::r-x, other::---.
        let mut acl = 2_u32.to_le_bytes().to_vec();
        for (tag, permissions, id) in [
            (0x01_u16, 7_u16, u32::MAX),
            (0x02, 4, 65534),
            (0x04, 5, u32::MAX),
            (0x10, 5, u32::MAX),
            (0x20, 0, u32::MAX),
        ] {
            acl.extend(tag.to_le_bytes());
            acl.extend(permissions.to_le_bytes());
            acl.extend(id.to_le_bytes());
        }
        let dir_path = directory.path().join("dir");
        if rustix::fs::setxattr(
            &dir_path,
            "system.posix_acl_default",
            &acl,
            rustix::fs::XattrFlags::empty(),
        )
        .is_err()
        {
            // No ACL support here.
            return;
        }
        let root = super::beneath::Root::new(directory.path());
        let dir = root.directory(&["dir"], false).unwrap();
        // The directory's own list is refused.
        assert!(dir.own_access().is_err());
        let mut access = dir.access("file.jsonc").unwrap().unwrap();
        access.mode = 0o640;
        dir.write("new", Some(&access), b"x").unwrap();
        let written = dir_path.join("new");
        assert_eq!(
            rustix::fs::getxattr(&written, "system.posix_acl_access", &mut [0_u8; 0][..]),
            Err(rustix::io::Errno::NODATA)
        );
        assert_eq!(
            fs::metadata(&written).unwrap().permissions().mode() & 0o777,
            0o640
        );
        // So has a created one.
        dir.create("created", Some(&access), b"x").unwrap();
        assert_eq!(
            rustix::fs::getxattr(
                dir_path.join("created"),
                "system.posix_acl_access",
                &mut [0_u8; 0][..]
            ),
            Err(rustix::io::Errno::NODATA)
        );
        // Without access, the file has what the directory gives it.
        dir.write("default", None, b"x").unwrap();
        assert!(
            rustix::fs::getxattr(
                dir_path.join("default"),
                "system.posix_acl_access",
                &mut [0_u8; 0][..]
            )
            .is_ok()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn labels_are_compared_and_never_given() {
        use std::os::unix::fs::MetadataExt as _;
        let label = |path: &std::path::Path, value: &[u8]| {
            let metadata = fs::metadata(path).unwrap();
            super::beneath::GIVEN_LABELS.with(|labels| {
                labels
                    .borrow_mut()
                    .insert((metadata.dev(), metadata.ino()), value.to_vec())
            });
        };
        let directory = tree();
        let root = super::beneath::Root::new(directory.path());
        let dir = root.directory(&["dir"], false).unwrap();
        let unlabelled = dir.access("file.jsonc").unwrap().unwrap();
        label(
            &directory.path().join("dir/file.jsonc"),
            b"system_u:object_r:secret_t:s0\0",
        );
        let labelled = dir.access("file.jsonc").unwrap().unwrap();
        assert_eq!(labelled.mode, unlabelled.mode);
        assert_ne!(labelled.control, unlabelled.control);
        assert_ne!(labelled.audience(false), unlabelled.audience(false));
        assert_eq!(labelled.new_file().control, labelled.control);
        assert_eq!(
            labelled.new_file().user,
            rustix::process::geteuid().as_raw()
        );
        // A new file the system labels otherwise is refused before anything
        // is written, and none is left.
        assert!(dir.write("new", Some(&labelled), b"secret").is_err());
        assert!(!directory.path().join("dir/new").exists());
        dir.write("new", Some(&unlabelled), b"x").unwrap();
        // So is a created one, which is removed, being still the file made.
        let refused = dir.create("created", Some(&labelled), b"secret");
        assert!(
            matches!(&refused, Err(Refused::Io(error)) if !error.to_string().contains("may be left"))
        );
        assert!(!directory.path().join("dir/created").exists());
        dir.create("created", Some(&unlabelled), b"x").unwrap();
        // The labels are checked before a byte is written: a file moved
        // away as it is made, so that it is left, is still empty.
        let (made, moved) = (
            directory.path().join("dir/labelled"),
            directory.path().join("dir/moved-labelled"),
        );
        let moving = moved.clone();
        super::AFTER_CREATE.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || fs::rename(&made, &moving)));
        });
        let refused = dir.create("labelled", Some(&labelled), b"secret");
        super::AFTER_CREATE.with(|slot| *slot.borrow_mut() = None);
        assert!(
            matches!(&refused, Err(Refused::Io(error)) if left(error)),
            "{refused:?}"
        );
        assert_eq!(fs::read(&moved).unwrap(), b"");
        // So is a directory.
        label(
            &directory.path().join("dir"),
            b"system_u:object_r:secret_t:s0\0",
        );
        let own = dir.own_access().unwrap();
        assert_ne!(own.control, [0; 32]);
        let made = root.directory(&["made"], true).unwrap();
        assert!(made.set_own_access(&own).is_err());
        made.set_own_access(&made.own_access().unwrap()).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn only_local_file_systems_that_show_their_access_control_lists_are_checked() {
        let directory = tree();
        let root = super::beneath::Root::new(directory.path());
        let dir = root.directory(&["dir"], false).unwrap();
        // A test's own directory is found by its mount among this process's
        // mounts, and told as its type and magic number say: checked on a
        // local file system, not on a container's overlay root.
        let mounts = super::beneath::Mounts::read();
        let told = dir.unchecked_file_system(&mounts).unwrap();
        if fs::File::open("/proc/self/mountinfo").is_err() {
            // Only without a readable /proc may it not be told; in a chroot
            // whose root is not a mount point, where the tree's mount is not
            // listed, this test fails, as publishing there needs consent.
            assert!(told.unwrap().starts_with("one that could not be told: "));
        } else {
            let id = dir.mount_id().unwrap().unwrap();
            let found = mounts.type_of(id).unwrap();
            let expected = super::beneath::unchecked_file_system(found, dir.magic().unwrap());
            assert_eq!(told, expected);
            // On a local file system, as CI's ext4 or a tmpfs, it is checked.
            if matches!(found, b"ext4" | b"tmpfs" | b"xfs" | b"btrfs") {
                assert_eq!(told, None);
            }
        }
        // Both the type of the mount and the magic number must be a
        // checked file system's: a 9p share passes on its host's magic
        // number, and an overlay's upper layer may be remote.
        for (found, magic, name) in [
            ("ext4", 0xEF53, None),
            ("xfs", 0x5846_5342, None),
            ("btrfs", 0x9123_683E, None),
            ("tmpfs", 0x0102_1994, None),
            ("zfs", 0x2FC1_2FC1, None),
            ("9p", 0xEF53, Some("9p")),
            ("overlay", 0x794C_7630, Some("overlay")),
            ("cifs", 0xFF53_4D42, Some("cifs")),
            ("vboxsf", 0x786F_4256, Some("vboxsf")),
            ("fuse.a\tb", 0x6573_5546, Some("fuse.a\\tb")),
            (
                "ext4",
                0x0102_1997,
                Some("ext4, though statfs gives type 0x1021997"),
            ),
        ] {
            super::beneath::GIVEN_FILE_SYSTEM.with(|given| given.set(Some((found, magic))));
            let told = dir.unchecked_file_system(&mounts).unwrap();
            super::beneath::GIVEN_FILE_SYSTEM.with(|given| given.set(None));
            assert_eq!(told.as_deref(), name, "{found} {magic:x}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_file_system_that_cannot_be_told_is_not_checked() {
        use super::mounts::Mounts;
        let directory = tree();
        let root = super::beneath::Root::new(directory.path());
        let dir = root.directory(&["dir"], false).unwrap();
        let told = |mounts: &Mounts| dir.unchecked_file_system(mounts).unwrap();
        let real = Mounts::read();
        // Only without a readable /proc may the mount not be known; in a
        // chroot whose root is not a mount point, where the tree's mount is
        // not listed, this test fails, as publishing there needs consent.
        if fs::File::open("/proc/self/mountinfo").is_err() {
            assert!(
                told(&real)
                    .unwrap()
                    .starts_with("one that could not be told: ")
            );
            return;
        }
        let id = dir.mount_id().unwrap().unwrap();
        let line = |id: u64, rest: &str| format!("{id} 1 8:1 / /mnt rw - {rest}\n");
        // Mounts that could not be read, a table without the mount, and a
        // line for it that is malformed or too long each leave its file
        // system untold, and so not checked.
        for (mounts, reason) in [
            (
                Mounts::failed("it is not there"),
                "it is not there".to_owned(),
            ),
            (
                Mounts::parsed(b""),
                format!("its mount, {id}, is not in /proc/self/mountinfo"),
            ),
            (
                Mounts::parsed(line(id, "ext\\9 /dev/sda1 rw").as_bytes()),
                "its line in /proc/self/mountinfo is malformed".to_owned(),
            ),
            (
                Mounts::parsed(
                    format!("{id} 1 8:1 / /{} rw - ext4 x rw\n", "a".repeat(70_000)).as_bytes(),
                ),
                "its line in /proc/self/mountinfo is longer than 65536 bytes".to_owned(),
            ),
            (
                Mounts::parsed((line(id, "ext4 a rw") + &line(id, "ext4 b rw")).as_bytes()),
                format!("its mount, {id}, is listed twice in /proc/self/mountinfo"),
            ),
        ] {
            assert_eq!(
                told(&mounts),
                Some(format!("one that could not be told: {reason}"))
            );
        }
        // Without /proc, mounts that could not be read are named as the
        // cause, before a mount ID that fdinfo, in /proc too, cannot give;
        // and where only fdinfo cannot be read, that is.
        super::beneath::NO_STATX_MOUNT_ID.with(|no| no.set(Some("/nonexistent/fdinfo")));
        let without_proc = told(&Mounts::failed("it is not there"));
        let without_fdinfo = told(&real);
        super::beneath::NO_STATX_MOUNT_ID.with(|no| no.set(None));
        assert_eq!(
            without_proc.as_deref(),
            Some("one that could not be told: it is not there")
        );
        let without_fdinfo = without_fdinfo.unwrap();
        assert!(
            without_fdinfo.starts_with(
                "one that could not be told: statx gives no mount ID, and /proc/self/fdinfo could not be read ("
            ),
            "{without_fdinfo}"
        );
        // A hostile line for another mount changes nothing for this one.
        let found = real.type_of(id).unwrap().escape_ascii().to_string();
        let mut table = line(id + 1, "fuse.\u{7f} x rw").into_bytes();
        table.extend(format!("{} 1 8:1 / /mnt rw - fuse.", id + 2).as_bytes());
        table.extend([0xFF, b' ', b'x', b'\n']);
        table.extend(line(id, &format!("{found} x rw")).as_bytes());
        table.extend(line(id + 3, &"\\040".repeat(400)).as_bytes());
        assert_eq!(told(&Mounts::parsed(&table)), told(&real));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mountinfo_is_read_a_line_at_a_time() {
        use super::mounts::scan;
        let text = b"22 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw\n\
36 22 0:32 / /mnt/with\\040space rw - 9p host rw,trans=virtio\n\
37 22 0:33 / /mnt/\xff\xfe rw master:2 shared:3 - fuse.s\\134sh\xff host rw\n\
38 22 8:1 / / rw ext4 /dev/sda1 rw\n\
x 1 8:1 / / rw - ext4 /dev/sda1 rw\n\
39 22 8:1 / / rw -\n\
40 22 8:1 / / rw - ext\\9 /dev/sda1 rw\n\
41 22 8:1 / / rw - xfs a rw";
        // A small buffer, so lines are read across its edges.
        let mounts = scan(std::io::BufReader::with_capacity(7, &text[..])).unwrap();
        assert_eq!(mounts[&22].as_deref(), Ok(&b"ext4"[..]));
        assert_eq!(mounts[&36].as_deref(), Ok(&b"9p"[..]));
        assert_eq!(mounts[&37].as_deref(), Ok(&b"fuse.s\\sh\xff"[..]));
        assert_eq!(mounts[&41].as_deref(), Ok(&b"xfs"[..]));
        for id in [38, 39, 40] {
            assert!(mounts[&id].is_err(), "{id}");
        }
        // A line without a mount ID is left out.
        assert_eq!(mounts.len(), 7);
        // What follows the first 64 KiB of a long line is skipped to its
        // newline, even where it reads as a line of its own, right at the
        // cut or a buffer later, wherever the reader's buffer ends:
        // Mounts::read reads through an 8 KiB one (BufReader::new).
        for gap in [0, 8 << 10] {
            let mut long = b"22 1 8:1 / /".to_vec();
            long.resize((64 << 10) + gap, b'a');
            long.extend(b"99 1 8:1 / / rw - ext4 x rw\n23 1 8:1 / / rw - xfs a rw\n");
            for capacity in [4 << 10, 8 << 10, 64 << 10, 1 << 20] {
                let reader = std::io::BufReader::with_capacity(capacity, &long[..]);
                let mounts = scan(reader).unwrap();
                let at = format!("gap {gap}, buffer {capacity}");
                assert!(
                    mounts[&22].as_ref().unwrap_err().contains("longer than"),
                    "{at}"
                );
                assert_eq!(mounts[&23].as_deref(), Ok(&b"xfs"[..]), "{at}");
                assert_eq!(mounts.keys().copied().collect::<Vec<_>>(), [22, 23], "{at}");
            }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn fdinfo_gives_the_mount_statx_does() {
        use rustix::fs::{AtFlags, StatxFlags, statx};
        use std::os::fd::AsRawFd as _;
        if !std::path::Path::new("/proc/self/fdinfo").is_dir() {
            return;
        }
        let directory = tree();
        let file = fs::File::open(directory.path().join("dir")).unwrap();
        // Before Linux 4.11, or where seccomp blocks it, there is no statx,
        // and fdinfo is the only source.
        let from_statx = match statx(&file, "", AtFlags::EMPTY_PATH, StatxFlags::MNT_ID) {
            Ok(found) if found.stx_mask & StatxFlags::MNT_ID.bits() != 0 => Some(found.stx_mnt_id),
            Ok(_) | Err(rustix::io::Errno::NOSYS) => None,
            Err(errno) => panic!("{errno}"),
        };
        let fdinfo = super::mounts::fdinfo_mount_id("/proc/self/fdinfo", file.as_raw_fd()).unwrap();
        assert!(fdinfo.is_some());
        if from_statx.is_some() {
            assert_eq!(fdinfo, from_statx);
        }
        // A directory's mount ID comes from fdinfo where statx gives none,
        // as before Linux 5.8.
        let root = super::beneath::Root::new(directory.path());
        let dir = root.directory(&["dir"], false).unwrap();
        super::beneath::NO_STATX_MOUNT_ID.with(|no| no.set(Some("/proc/self/fdinfo")));
        let fallback = dir.mount_id();
        super::beneath::NO_STATX_MOUNT_ID.with(|no| no.set(None));
        let fallback = fallback.unwrap().unwrap();
        assert_eq!(Some(fallback), fdinfo);
        if let Some(id) = from_statx {
            assert_eq!(fallback, id);
        }
    }

    #[test]
    fn the_lock_excludes_a_second_holder() {
        let directory = tree();
        let root = super::beneath::Root::new(directory.path());
        let other = super::beneath::Root::new(directory.path());
        let held = root.lock().unwrap();
        assert!(held.is_some());
        assert!(other.lock().unwrap().is_none());
        drop(held);
        assert!(other.lock().unwrap().is_some());
    }
}

#[cfg(all(test, target_os = "linux"))]
mod selinux_tests {
    use super::beneath::selinux::{request, trimmed};

    #[test]
    fn label_questions_are_one_line_with_the_name_encoded() {
        let query = request(
            b"unconfined_u:unconfined_r:unconfined_t:s0-s0:c0.c1023\0",
            b"unconfined_u:object_r:user_home_t:s0\0",
            7,
            "0_0.jsonc",
        )
        .unwrap();
        assert_eq!(
            query,
            b"unconfined_u:unconfined_r:unconfined_t:s0-s0:c0.c1023 unconfined_u:object_r:user_home_t:s0 7 0_0.jsonc"
        );
        let odd = request(b"a", b"b", 1, "x y%+\u{e9}").unwrap();
        assert_eq!(odd, b"a b 1 x%20y%25%2b%c3%a9");
        assert!(request(b"a b", b"c", 1, "x").is_err());
        assert!(request(b"", b"c", 1, "x").is_err());
        assert_eq!(trimmed(b"label\0\n"), b"label");
        assert_eq!(trimmed(b"\0"), b"");
    }
}

/// On Windows the path-based implementation refuses every reparse point,
/// junctions included, at the component where it occurs, removes one
/// without touching what it names, and holds its lock in a file.
#[cfg(all(test, windows))]
mod windows_tests {
    use super::{Found, RawKind, Refused, portable::Root};
    use std::{
        fs,
        path::{Path, PathBuf},
        process::{Command, Stdio},
    };

    /// Makes `link` a junction to the directory `target`.
    fn junction(link: &Path, target: &Path) {
        // `mklink` takes `/` for an option, so the paths use `\\`.
        let native = |path: &Path| path.components().collect::<PathBuf>();
        let status = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(native(link))
            .arg(native(target))
            .stdout(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn link<T>(result: Result<T, Refused>) -> String {
        match result {
            Err(Refused::Link(prefix)) => prefix,
            Err(other) => panic!("expected a link, got {other:?}"),
            Ok(_) => panic!("expected a link, got success"),
        }
    }

    #[test]
    fn junctions_are_links() {
        let directory = tempfile::tempdir().unwrap();
        fs::create_dir(directory.path().join("dir")).unwrap();
        fs::write(directory.path().join("dir/file.jsonc"), "{}").unwrap();
        junction(
            &directory.path().join("link"),
            &directory.path().join("dir"),
        );
        let root = Root::new(directory.path());
        assert_eq!(root.file(&["dir", "file.jsonc"]).unwrap().1, 2);
        assert_eq!(link(root.file(&["link", "file.jsonc"])), "link");
        assert_eq!(link(root.kind(&["link"])), "link");
        assert_eq!(link(root.directory(&["link"], false)), "link");
        assert_eq!(link(root.directory(&["link", "inside"], true)), "link");
        assert!(!directory.path().join("dir/inside").exists());
        let kinds: Vec<(Option<String>, RawKind)> = root
            .list(&[])
            .unwrap()
            .map(|entry| entry.map(|entry| (entry.name, entry.kind)))
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(kinds.contains(&(Some("link".to_owned()), RawKind::Link)));
        assert!(kinds.contains(&(Some("dir".to_owned()), RawKind::Directory)));

        // A write over a junction replaces it, not what it names.
        let base = root.directory(&[], false).unwrap();
        base.write("link", None, b"x").unwrap();
        assert!(directory.path().join("dir/file.jsonc").exists());
        assert_eq!(root.kind(&["link"]).unwrap(), Found::File);
    }

    #[test]
    fn read_only_files_are_refused() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("file.jsonc");
        fs::write(&file, "{}").unwrap();
        let base = Root::new(directory.path()).directory(&[], false).unwrap();
        assert!(base.access("file.jsonc").unwrap().is_some());
        let mut permissions = fs::metadata(&file).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&file, permissions).unwrap();
        assert!(matches!(base.access("file.jsonc"), Err(Refused::Io(_))));
    }

    #[test]
    fn encrypted_files_and_directories_are_refused() {
        let directory = tempfile::tempdir().unwrap();
        let inside = directory.path().join("inside");
        fs::create_dir(&inside).unwrap();
        fs::write(inside.join("file.jsonc"), "{}").unwrap();
        // EFS is not on every volume; where it is not, there is nothing to
        // refuse.
        let encrypted = Command::new("cipher")
            .arg("/e")
            .arg("/s:".to_owned() + &inside.display().to_string())
            .stdout(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        let root = Root::new(directory.path());
        let dir = root.directory(&["inside"], false).unwrap();
        // The hosted runners have it, so there the test must not pass
        // without it.
        assert!(
            encrypted || std::env::var_os("CI").is_none(),
            "cipher /e failed on CI"
        );
        if encrypted {
            assert!(matches!(dir.access("file.jsonc"), Err(Refused::Io(_))));
            assert!(matches!(dir.own_access(), Err(Refused::Io(_))));
        } else {
            assert!(dir.access("file.jsonc").unwrap().is_some());
        }
    }

    #[test]
    fn the_lock_is_a_file_held_alone() {
        let directory = tempfile::tempdir().unwrap();
        let lock_file = directory.path().join(crate::access::LOCK_NAME);
        let root = Root::new(directory.path());
        let other = Root::new(directory.path());
        let held = root.lock().unwrap();
        assert!(held.is_some());
        assert!(other.lock().unwrap().is_none());
        // A file renamed into the root may replace another meanwhile.
        let base = root.directory(&[], false).unwrap();
        base.write(".world.jsonc.tilewright-tmp", None, b"{}")
            .unwrap();
        base.write("world.jsonc", None, b"old").unwrap();
        base.rename(".world.jsonc.tilewright-tmp", "world.jsonc")
            .unwrap();
        // The lock file goes with the lock.
        drop(held);
        assert!(!lock_file.exists());
        assert!(other.lock().unwrap().is_some());
        // One that no edit holds is not taken over.
        fs::write(&lock_file, "someone's").unwrap();
        assert!(matches!(root.lock(), Err(Refused::Io(_))));
        assert_eq!(fs::read_to_string(&lock_file).unwrap(), "someone's");
    }

    /// Everything about an entry a create must leave as it was, not
    /// following it: attributes, size, times, and a file's bytes or a
    /// directory's entries (a reparse point's metadata alone).
    fn snapshot(path: &Path) -> String {
        use std::os::windows::fs::MetadataExt as _;
        let metadata = fs::symlink_metadata(path).unwrap();
        let reparse = metadata.file_attributes() & 0x400 != 0;
        let content = if reparse {
            String::new()
        } else if metadata.is_dir() {
            let mut names: Vec<_> = fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect();
            names.sort();
            format!("{names:?}")
        } else {
            format!("{:?}", fs::read(path).unwrap())
        };
        format!(
            "{:x} {} {} {} {content}",
            metadata.file_attributes(),
            metadata.file_size(),
            metadata.creation_time(),
            metadata.last_write_time(),
        )
    }

    fn already_there(result: Result<(), Refused>) -> bool {
        type Inner = dyn std::error::Error + Send + Sync;
        match result {
            Err(Refused::Io(error)) => {
                error.kind() == std::io::ErrorKind::AlreadyExists
                    && error
                        .get_ref()
                        .is_some_and(Inner::is::<crate::access::AlreadyThere>)
            }
            other => panic!("expected something already there, got {other:?}"),
        }
    }

    fn hook(hook: impl FnMut() -> std::io::Result<()> + 'static) {
        super::AFTER_CREATE.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    }

    fn unhook() {
        super::AFTER_CREATE.with(|slot| *slot.borrow_mut() = None);
    }

    #[test]
    fn a_create_never_replaces_or_removes_an_entry() {
        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let at = |name: &str| directory.path().join(name);
        let root = Root::new(directory.path());
        let base = root.directory(&[], false).unwrap();

        // A new file has exactly the bytes asked for, and read-only access
        // makes it read-only.
        base.create("new", None, b"bytes").unwrap();
        assert_eq!(fs::read(at("new")).unwrap(), b"bytes");
        let read_only = crate::Access {
            mode: 0o444,
            user: 0,
            group: 0,
            control: [0; 32],
        };
        base.create("read-only", Some(&read_only), b"x").unwrap();
        assert!(
            fs::metadata(at("read-only"))
                .unwrap()
                .permissions()
                .readonly()
        );

        // Nothing at the name is removed, replaced or followed: a file,
        // directories empty or not, a junction to a directory outside, a
        // junction to nothing, and a symbolic link to a file outside.
        fs::write(at("file"), "keep").unwrap();
        fs::create_dir(at("empty")).unwrap();
        fs::create_dir(at("full")).unwrap();
        fs::write(at("full/inside"), "kept").unwrap();
        fs::create_dir(outside.path().join("elsewhere")).unwrap();
        junction(&at("junction"), &outside.path().join("elsewhere"));
        fs::create_dir(outside.path().join("gone")).unwrap();
        junction(&at("dangling"), &outside.path().join("gone"));
        fs::remove_dir(outside.path().join("gone")).unwrap();
        let target = outside.path().join("target");
        fs::write(&target, "outside").unwrap();
        let mut names = vec![
            "new",
            "read-only",
            "file",
            "empty",
            "full",
            "junction",
            "dangling",
        ];
        // Symbolic links need a privilege the hosted runners have.
        let linked = std::os::windows::fs::symlink_file(&target, at("to-file")).is_ok();
        assert!(
            linked || std::env::var_os("CI").is_none(),
            "symlink_file failed on CI"
        );
        if linked {
            names.push("to-file");
        }
        let before: Vec<String> = names.iter().map(|name| snapshot(&at(name))).collect();
        for name in &names {
            assert!(already_there(base.create(name, None, b"x")), "{name}");
            assert!(
                already_there(base.create(name, Some(&read_only), b"x")),
                "{name}"
            );
        }
        let after: Vec<String> = names.iter().map(|name| snapshot(&at(name))).collect();
        assert_eq!(before, after);
        assert_eq!(fs::read_to_string(&target).unwrap(), "outside");
        assert!(!outside.path().join("gone").exists());
        assert_eq!(
            fs::read_dir(outside.path().join("elsewhere"))
                .unwrap()
                .count(),
            0
        );

        // A create that fails after making its file never removes it, and
        // says it may be left.
        hook(|| Err(std::io::ErrorKind::StorageFull.into()));
        let failed = base.create("failed", None, b"x");
        unhook();
        let Err(Refused::Io(error)) = failed else {
            panic!("expected a failure, got {failed:?}");
        };
        assert_eq!(error.kind(), std::io::ErrorKind::StorageFull);
        assert!(error.to_string().contains("may be left"), "{error}");
        assert!(at("failed").exists());
        // Nor what another process put at the name meanwhile.
        let (moved, name) = (at("moved"), at("swapped"));
        hook(move || {
            fs::rename(&name, &moved)?;
            fs::write(&name, "someone else's")?;
            Err(std::io::ErrorKind::StorageFull.into())
        });
        let failed = base.create("swapped", None, b"x");
        unhook();
        assert!(matches!(failed, Err(Refused::Io(_))), "{failed:?}");
        assert_eq!(fs::read_to_string(at("swapped")).unwrap(), "someone else's");
        assert!(at("moved").exists());
    }

    /// An open that fails says the name is taken only when something is at
    /// it: with an error other than "already exists", and with the directory
    /// removed since it was opened.
    #[test]
    fn a_create_that_cannot_open_is_taken_only_if_something_is_there() {
        type Inner = dyn std::error::Error + Send + Sync;
        let directory = tempfile::tempdir().unwrap();
        let at = |name: &str| directory.path().join(name);
        let root = Root::new(directory.path());
        let base = root.directory(&[], false).unwrap();
        fs::write(at("file"), "keep").unwrap();
        super::OPEN_FAILS.with(|fails| fails.set(Some(std::io::ErrorKind::PermissionDenied)));
        let free = base.create("free", None, b"x");
        let held = base.create("file", None, b"x");
        super::OPEN_FAILS.with(|fails| fails.set(None));
        let taken = |result: &Result<(), Refused>| {
            matches!(result, Err(Refused::Io(error))
                if error.get_ref().is_some_and(Inner::is::<crate::access::AlreadyThere>))
        };
        assert!(
            matches!(&free, Err(Refused::Io(_))) && !taken(&free),
            "{free:?}"
        );
        assert!(taken(&held), "{held:?}");
        fs::create_dir(at("gone")).unwrap();
        let gone = root.directory(&["gone"], false).unwrap();
        fs::remove_dir(at("gone")).unwrap();
        let missing = gone.create("x", None, b"x");
        assert!(missing.is_err() && !taken(&missing), "{missing:?}");
    }
}
