// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Bounded access to a source tree. Every path is relative to the root,
//! `/`-separated and validated; symbolic links are never followed.

use crate::{Code, Diagnostic, LoadOptions, diagnostic::Positions};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Read as _,
    path::{Path, PathBuf},
};
use tilewright_jsonc::{Document, ErrorCode};

/// A parsed source file.
pub(crate) struct Source {
    pub path: String,
    pub document: Document,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EntryKind {
    File,
    Directory,
}

pub(crate) struct Entry {
    pub name: String,
    pub kind: EntryKind,
}

pub(crate) struct Tree<'o> {
    root: PathBuf,
    options: &'o LoadOptions,
    files: usize,
    entries: usize,
    bytes: u64,
    digests: BTreeMap<String, (u64, [u8; 32])>,
    diagnostics: Vec<Diagnostic>,
    exhausted: bool,
}

/// Whether `path` is a safe relative path ending in `.jsonc`.
pub(crate) fn is_jsonc_path(path: &str) -> bool {
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

impl<'o> Tree<'o> {
    pub fn new(root: &Path, options: &'o LoadOptions) -> Self {
        Self {
            root: root.to_path_buf(),
            options,
            files: 0,
            entries: 0,
            bytes: 0,
            digests: BTreeMap::new(),
            diagnostics: Vec::new(),
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

    pub fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }

    fn absolute(&self, path: &str) -> PathBuf {
        path.split('/')
            .filter(|component| !component.is_empty())
            .fold(self.root.clone(), |absolute, component| {
                absolute.join(component)
            })
    }

    /// Checks that no component of `path` is a symbolic link. Reports and
    /// returns `false` otherwise.
    fn no_links(&mut self, path: &str) -> bool {
        let mut current = self.root.clone();
        let mut walked = String::new();
        for component in path.split('/').filter(|component| !component.is_empty()) {
            current.push(component);
            if !walked.is_empty() {
                walked.push('/');
            }
            walked.push_str(component);
            if fs::symlink_metadata(&current)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
            {
                self.diagnostics.push(Diagnostic::file(
                    &walked,
                    Code::Symlink,
                    "symbolic links are not followed",
                ));
                return false;
            }
        }
        true
    }

    /// Whether a regular file or directory exists at `path`. A symbolic
    /// link anywhere in the path is reported and counts as neither.
    pub fn kind(&mut self, path: &str) -> Option<EntryKind> {
        if !self.no_links(path) {
            return None;
        }
        let metadata = fs::symlink_metadata(self.absolute(path)).ok()?;
        if metadata.is_file() {
            Some(EntryKind::File)
        } else if metadata.is_dir() {
            Some(EntryKind::Directory)
        } else {
            None
        }
    }

    /// Lists a directory, sorted by name. Symbolic links, special files and
    /// non-UTF-8 names are reported and skipped.
    pub fn list(&mut self, path: &str) -> Vec<Entry> {
        if !self.no_links(path) {
            return Vec::new();
        }
        let directory = match fs::read_dir(self.absolute(path)) {
            Ok(directory) => directory,
            Err(error) => {
                self.diagnostics.push(Diagnostic::file(
                    path,
                    Code::Io,
                    format!("cannot list directory: {error}"),
                ));
                return Vec::new();
            }
        };
        let mut entries = Vec::new();
        for entry in directory {
            if self.exhausted {
                break;
            }
            self.entries += 1;
            if self.entries > self.options.maximum_entries {
                self.exhaust(path);
                break;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    self.diagnostics.push(Diagnostic::file(
                        path,
                        Code::Io,
                        format!("cannot list directory: {error}"),
                    ));
                    continue;
                }
            };
            let Ok(name) = entry.file_name().into_string() else {
                self.diagnostics.push(Diagnostic::file(
                    path,
                    Code::UnexpectedFile,
                    "file name is not UTF-8",
                ));
                continue;
            };
            let child = join(path, &name);
            match entry.file_type() {
                Ok(kind) if kind.is_symlink() => self.diagnostics.push(Diagnostic::file(
                    &child,
                    Code::Symlink,
                    "symbolic links are not followed",
                )),
                Ok(kind) if kind.is_dir() => entries.push(Entry {
                    name,
                    kind: EntryKind::Directory,
                }),
                Ok(kind) if kind.is_file() => entries.push(Entry {
                    name,
                    kind: EntryKind::File,
                }),
                Ok(_) => self.diagnostics.push(Diagnostic::file(
                    &child,
                    Code::UnexpectedFile,
                    "not a regular file or directory",
                )),
                Err(error) => self.diagnostics.push(Diagnostic::file(
                    &child,
                    Code::Io,
                    format!("cannot read file type: {error}"),
                )),
            }
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        entries
    }

    /// Reads and parses a JSONC document. Every problem is reported.
    #[allow(clippy::too_many_lines)]
    pub fn document(&mut self, path: &str) -> Option<Source> {
        if !self.no_links(path) {
            return None;
        }
        let absolute = self.absolute(path);
        let metadata = match fs::symlink_metadata(&absolute) {
            Ok(metadata) if metadata.is_file() => metadata,
            Ok(_) => {
                self.diagnostics.push(Diagnostic::file(
                    path,
                    Code::UnexpectedFile,
                    "expected a regular file",
                ));
                return None;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.diagnostics
                    .push(Diagnostic::file(path, Code::MissingFile, "file is missing"));
                return None;
            }
            Err(error) => {
                self.diagnostics.push(Diagnostic::file(
                    path,
                    Code::Io,
                    format!("cannot read file: {error}"),
                ));
                return None;
            }
        };
        let limit = u64::try_from(self.options.limits.max_input_bytes).unwrap_or(u64::MAX);
        if metadata.len() > limit {
            self.diagnostics.push(Diagnostic::file(
                path,
                ErrorCode::InputTooLarge,
                format!("file is larger than {limit} bytes"),
            ));
            return None;
        }
        if !self.within_budget(metadata.len()) {
            self.exhaust(path);
            return None;
        }
        // Read at most one byte past the file limit or the remaining budget,
        // so a file that grows after the size check cannot force an
        // unbounded read; its actual length is checked again below.
        let remaining = self.options.maximum_total_bytes.saturating_sub(self.bytes);
        let bytes = match fs::File::open(&absolute).and_then(|file| {
            let mut bytes = Vec::new();
            file.take(limit.min(remaining).saturating_add(1))
                .read_to_end(&mut bytes)?;
            Ok(bytes)
        }) {
            Ok(bytes) => bytes,
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
        self.files += 1;
        self.bytes = self.bytes.saturating_add(length);
        self.digests
            .insert(path.to_owned(), (length, Sha256::digest(&bytes).into()));
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
        for (path, (length, digest)) in &self.digests {
            hasher.update(path.as_bytes());
            hasher.update([0]);
            hasher.update(length.to_le_bytes());
            hasher.update(digest);
        }
        hasher.finalize().into()
    }
}

pub(crate) fn join(directory: &str, name: &str) -> String {
    if directory.is_empty() {
        name.to_owned()
    } else {
        format!("{directory}/{name}")
    }
}
