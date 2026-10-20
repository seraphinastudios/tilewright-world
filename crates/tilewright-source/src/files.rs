// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Bounded access to a source tree. Every path is relative to the root,
//! `/`-separated and validated; symbolic links are never followed. On Unix
//! every component is opened through its parent's directory handle without
//! following links (`beneath`); elsewhere components are checked by path
//! before use (`portable`), on Windows for any reparse point.

#[cfg(unix)]
pub(crate) mod beneath;
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

pub(crate) struct Tree<'o> {
    root: platform::Root,
    options: &'o LoadOptions,
    overlay: &'o Overlay,
    files: usize,
    entries: usize,
    bytes: u64,
    digests: BTreeMap<String, (u64, [u8; 32])>,
    diagnostics: Diagnostics,
    exhausted: bool,
}

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
        if self.stopped() {
            self.diagnostics.skip();
            return Vec::new();
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
                return Vec::new();
            }
        }
        // An entry the overlay replaces or removes is not looked at at
        // all, so what is there on disk, a link, a special file or a
        // directory, is not reported.
        let mut entries = self.list_disk(path);
        let present: BTreeSet<String> = entries.iter().map(|entry| entry.name.clone()).collect();
        for (name, kind) in added {
            if !present.contains(name) {
                self.entries += 1;
                if self.entries > self.options.maximum_entries {
                    self.exhaust(path);
                    break;
                }
                entries.push(Entry {
                    name: name.to_owned(),
                    kind,
                });
            }
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        entries
    }

    /// Lists a directory on disk. A directory that is missing on disk but
    /// holds overlay files is empty there.
    fn list_disk(&mut self, path: &str) -> Vec<Entry> {
        let overlay = self.overlay;
        let directory = match self.root.list(&components(path)) {
            Ok(directory) => directory,
            Err(Refused::Link(link)) => {
                self.link(&link);
                return Vec::new();
            }
            Err(Refused::Missing) if self.holds_overlay_files(path) => return Vec::new(),
            Err(refused) => {
                let reason = match refused {
                    Refused::Io(error) => error.to_string(),
                    Refused::Missing => "it is missing".to_owned(),
                    Refused::Unexpected | Refused::Link(_) => "not a directory".to_owned(),
                };
                self.diagnostics.push(Diagnostic::file(
                    path,
                    Code::Io,
                    format!("cannot list directory: {reason}"),
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
            let Some(name) = entry.name else {
                self.diagnostics.push(Diagnostic::file(
                    path,
                    Code::UnexpectedFile,
                    "file name is not UTF-8",
                ));
                continue;
            };
            if overlay.contains_key(&join(path, &name)) {
                // The overlay's entry takes its place, and is counted then.
                self.entries -= 1;
                continue;
            }
            let child = join(path, &name);
            match entry.kind {
                RawKind::Link => self.link(&child),
                RawKind::Directory => entries.push(Entry {
                    name,
                    kind: EntryKind::Directory,
                }),
                RawKind::File => entries.push(Entry {
                    name,
                    kind: EntryKind::File,
                }),
                RawKind::Other => self.diagnostics.push(Diagnostic::file(
                    &child,
                    Code::UnexpectedFile,
                    "not a regular file or directory",
                )),
            }
        }
        entries
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

pub(crate) fn join(directory: &str, name: &str) -> String {
    if directory.is_empty() {
        name.to_owned()
    } else {
        format!("{directory}/{name}")
    }
}

/// Both access implementations refuse links themselves, at the component
/// where they occur, rather than relying on an earlier check.
#[cfg(all(test, unix))]
mod tests {
    use super::{Found, RawKind, Refused};
    use std::{fs, os::unix::fs::symlink};

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
        // A new file the system labels otherwise is refused before anything
        // is written, and none is left.
        assert!(dir.write("new", Some(&labelled), b"secret").is_err());
        assert!(!directory.path().join("dir/new").exists());
        dir.write("new", Some(&unlabelled), b"x").unwrap();
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
}
