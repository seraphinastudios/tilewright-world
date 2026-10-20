// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Windows access beneath the root through directory handles. Each
//! component is opened relative to the directory before it, with that
//! directory's handle as the root of the open, so no path is resolved
//! again. A reparse point at a name (a symbolic link, junction, mount point
//! or any other) is opened itself, never followed, and refused. Writes,
//! renames and removals act on names inside a directory opened that way,
//! or on handles opened that way, so they never follow a link either.
//!
//! An entry has the access control list its directory gives it: publishing
//! compares the lists of the entries it touches with each other and with
//! the list their directory gives a new entry, before writing, and never
//! changes one.

use super::{Found, RawEntry, RawKind, Refused};
use crate::access::Access;
use sha2::{Digest as _, Sha256};
use std::{
    collections::VecDeque,
    fs::{File, OpenOptions},
    io::{self, Write as _},
    os::windows::{
        fs::OpenOptionsExt as _,
        io::{AsHandle, BorrowedHandle, OwnedHandle},
    },
    path::Path,
};
use tilewright_platform::windows::{self as os, Open, Status};
use windows_sys::{
    Wdk::Storage::FileSystem::{
        FILE_CREATE, FILE_DIRECTORY_FILE, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_IF,
    },
    Win32::{
        Foundation::{
            ERROR_DIR_NOT_EMPTY, STATUS_FILE_IS_A_DIRECTORY, STATUS_NOT_A_DIRECTORY,
            STATUS_OBJECT_NAME_COLLISION, STATUS_OBJECT_NAME_NOT_FOUND,
            STATUS_OBJECT_PATH_NOT_FOUND, STATUS_SHARING_VIOLATION,
        },
        Security::{
            DACL_SECURITY_INFORMATION, INHERITED_ACE, LABEL_SECURITY_INFORMATION,
            SCOPE_SECURITY_INFORMATION, SE_DACL_PRESENT, SE_DACL_PROTECTED, SE_SACL_PRESENT,
            SE_SELF_RELATIVE,
        },
        Storage::FileSystem::{
            DELETE, FILE_ADD_FILE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_ENCRYPTED,
            FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES,
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE, READ_CONTROL,
            SYNCHRONIZE,
        },
    },
};

/// The access every directory handle has: enough to list it, open its
/// entries and read its attributes and security.
const DIRECTORY_ACCESS: u32 =
    FILE_LIST_DIRECTORY | FILE_TRAVERSE | FILE_READ_ATTRIBUTES | READ_CONTROL;
/// Others may read, write, rename and delete what a handle is open on, as
/// on Unix.
const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

/// Why the root could not be opened, kept so every access can say so.
#[derive(Clone, Copy, Debug)]
struct RootError {
    code: Option<i32>,
    kind: io::ErrorKind,
}

impl RootError {
    fn of(error: &io::Error) -> Self {
        Self {
            code: error.raw_os_error(),
            kind: error.kind(),
        }
    }

    fn error(self) -> io::Error {
        self.code
            .map_or_else(|| self.kind.into(), io::Error::from_raw_os_error)
    }
}

/// The root directory, opened once so that every access is relative to it.
pub(crate) struct Root(Result<OwnedHandle, RootError>);

fn open(
    directory: BorrowedHandle<'_>,
    name: &str,
    access: u32,
    share: u32,
    disposition: u32,
    options: u32,
) -> Result<OwnedHandle, Status> {
    os::open_beneath(
        directory,
        name,
        &Open {
            access,
            share,
            disposition,
            options,
        },
    )
}

/// Whether a failed open means nothing is at the name.
fn missing(status: Status) -> bool {
    matches!(
        status.0,
        STATUS_OBJECT_NAME_NOT_FOUND | STATUS_OBJECT_PATH_NOT_FOUND
    )
}

fn attributes(handle: BorrowedHandle<'_>) -> Result<u32, Refused> {
    os::attributes(handle)
        .map(|(attributes, _)| attributes)
        .map_err(Refused::Io)
}

fn kind_of(attributes: u32) -> RawKind {
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        RawKind::Link
    } else if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        RawKind::Directory
    } else {
        RawKind::File
    }
}

/// What `name` in `directory` is, without following it; `None` when
/// nothing is there.
fn probe(directory: BorrowedHandle<'_>, name: &str) -> Result<Option<RawKind>, Refused> {
    match open(
        directory,
        name,
        FILE_READ_ATTRIBUTES,
        SHARE_ALL,
        FILE_OPEN,
        0,
    ) {
        Ok(handle) => Ok(Some(kind_of(attributes(handle.as_handle())?))),
        Err(status) if missing(status) => Ok(None),
        Err(status) => Err(Refused::Io(status.io_error())),
    }
}

/// Why opening `name` in `directory` failed; `prefix` is the path up to and
/// including `name`.
fn classify(directory: BorrowedHandle<'_>, name: &str, status: Status, prefix: &[&str]) -> Refused {
    if missing(status) {
        return Refused::Missing;
    }
    match probe(directory, name) {
        Ok(Some(RawKind::Link)) => Refused::Link(prefix.join("/")),
        _ if matches!(
            status.0,
            STATUS_NOT_A_DIRECTORY | STATUS_FILE_IS_A_DIRECTORY
        ) =>
        {
            Refused::Unexpected
        }
        _ => Refused::Io(status.io_error()),
    }
}

/// Opens the directory `name` in `directory`, refusing a reparse point.
fn open_directory(
    directory: BorrowedHandle<'_>,
    name: &str,
    disposition: u32,
    prefix: &[&str],
) -> Result<OwnedHandle, Refused> {
    let handle = open(
        directory,
        name,
        DIRECTORY_ACCESS,
        SHARE_ALL,
        disposition,
        FILE_DIRECTORY_FILE,
    )
    .map_err(|status| classify(directory, name, status, prefix))?;
    if kind_of(attributes(handle.as_handle())?) == RawKind::Link {
        return Err(Refused::Link(prefix.join("/")));
    }
    Ok(handle)
}

impl Root {
    pub(crate) fn new(path: &Path) -> Self {
        // The root itself may be a link; nothing beneath it is followed.
        let opened = OpenOptions::new()
            .access_mode(DIRECTORY_ACCESS | SYNCHRONIZE)
            .share_mode(SHARE_ALL)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
            .and_then(|file| {
                if file.metadata()?.is_dir() {
                    Ok(OwnedHandle::from(file))
                } else {
                    Err(io::ErrorKind::NotADirectory.into())
                }
            });
        Self(opened.map_err(|error| RootError::of(&error)))
    }

    /// A second handle on the same directory.
    pub(crate) fn try_clone(&self) -> Self {
        Self(match &self.0 {
            Ok(handle) => handle.try_clone().map_err(|error| RootError::of(&error)),
            Err(error) => Err(*error),
        })
    }

    fn root(&self) -> Result<BorrowedHandle<'_>, Refused> {
        match &self.0 {
            Ok(handle) => Ok(handle.as_handle()),
            Err(error) if error.kind == io::ErrorKind::NotFound => Err(Refused::Missing),
            Err(error) => Err(Refused::Io(error.error())),
        }
    }

    /// Runs `access` on the directory holding the last of `components` and
    /// that component's name, opening the directories one at a time.
    fn parent<T>(
        &self,
        components: &[&str],
        access: impl FnOnce(BorrowedHandle<'_>, &str) -> Result<T, Refused>,
    ) -> Result<T, Refused> {
        let root = self.root()?;
        let Some((name, parents)) = components.split_last() else {
            return Err(Refused::Unexpected);
        };
        let mut current: Option<OwnedHandle> = None;
        for (index, component) in parents.iter().enumerate() {
            let directory = current.as_ref().map_or(root, AsHandle::as_handle);
            current = Some(open_directory(
                directory,
                component,
                FILE_OPEN,
                &components[..=index],
            )?);
        }
        access(current.as_ref().map_or(root, AsHandle::as_handle), name)
    }

    /// Opens a regular file and returns it with its length.
    pub(crate) fn file(&self, components: &[&str]) -> Result<(File, u64), Refused> {
        self.parent(components, |directory, name| {
            let handle = open(
                directory,
                name,
                FILE_GENERIC_READ,
                SHARE_ALL,
                FILE_OPEN,
                FILE_NON_DIRECTORY_FILE,
            )
            .map_err(|status| classify(directory, name, status, components))?;
            if kind_of(attributes(handle.as_handle())?) != RawKind::File {
                return Err(Refused::Link(components.join("/")));
            }
            let file = File::from(handle);
            let length = file.metadata().map_err(Refused::Io)?.len();
            Ok((file, length))
        })
    }

    /// What the last of `components` is, without following it.
    pub(crate) fn kind(&self, components: &[&str]) -> Result<Found, Refused> {
        self.parent(components, |directory, name| {
            match probe(directory, name)? {
                None => Ok(Found::Missing),
                Some(RawKind::Link) => Err(Refused::Link(components.join("/"))),
                Some(RawKind::Directory) => Ok(Found::Directory),
                Some(RawKind::File | RawKind::Other) => Ok(Found::File),
            }
        })
    }

    /// Opens the directory `components`, creating missing ones when
    /// `create`.
    pub(crate) fn directory(
        &self,
        components: &[&str],
        create: bool,
    ) -> Result<Directory, Refused> {
        let root = self.root()?;
        let disposition = if create { FILE_OPEN_IF } else { FILE_OPEN };
        let mut current: Option<OwnedHandle> = None;
        for (index, component) in components.iter().enumerate() {
            let directory = current.as_ref().map_or(root, AsHandle::as_handle);
            current = Some(open_directory(
                directory,
                component,
                disposition,
                &components[..=index],
            )?);
        }
        match current {
            Some(directory) => Ok(Directory(directory)),
            None => root
                .try_clone_to_owned()
                .map(Directory)
                .map_err(Refused::Io),
        }
    }

    /// Makes the directory `components` in its existing parent; `false`
    /// when an entry by that name was already there.
    pub(crate) fn make_directory(&self, components: &[&str]) -> Result<bool, Refused> {
        let Some((name, parent)) = components.split_last() else {
            return Ok(false);
        };
        let parent = self.directory(parent, false)?;
        match open(
            parent.0.as_handle(),
            name,
            FILE_READ_ATTRIBUTES,
            SHARE_ALL,
            FILE_CREATE,
            FILE_DIRECTORY_FILE,
        ) {
            Ok(_) => Ok(true),
            Err(Status(STATUS_OBJECT_NAME_COLLISION)) => Ok(false),
            Err(status) => Err(classify(parent.0.as_handle(), name, status, components)),
        }
    }

    /// Takes an exclusive lock on the root directory, held until the
    /// returned lock is dropped; `None` when another holds it. The lock is
    /// a handle on the root that asks for write access and shares it with
    /// no other handle, so a second one is refused.
    pub(crate) fn lock(&self) -> Result<Option<Lock>, Refused> {
        match open(
            self.root()?,
            "",
            FILE_ADD_FILE,
            FILE_SHARE_READ | FILE_SHARE_DELETE,
            FILE_OPEN,
            FILE_DIRECTORY_FILE,
        ) {
            Ok(handle) => Ok(Some(Lock(handle))),
            Err(Status(STATUS_SHARING_VIOLATION)) => Ok(None),
            Err(status) => Err(Refused::Io(status.io_error())),
        }
    }

    /// Lists a directory. Entries are read as they are iterated.
    pub(crate) fn list(&self, components: &[&str]) -> Result<Listing, Refused> {
        let directory = if components.is_empty() {
            // A handle of its own, so the listing does not share its place
            // with another handle on the root.
            open(
                self.root()?,
                "",
                DIRECTORY_ACCESS,
                SHARE_ALL,
                FILE_OPEN,
                FILE_DIRECTORY_FILE,
            )
            .map_err(|status| Refused::Io(status.io_error()))?
        } else {
            self.parent(components, |directory, name| {
                open_directory(directory, name, FILE_OPEN, components)
            })?
        };
        Ok(Listing {
            directory,
            pending: VecDeque::new(),
            started: false,
            done: false,
        })
    }
}

pub(crate) struct Listing {
    directory: OwnedHandle,
    pending: VecDeque<os::Entry>,
    started: bool,
    done: bool,
}

impl Iterator for Listing {
    type Item = io::Result<RawEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(entry) = self.pending.pop_front() {
                let dot = u16::from(b'.');
                if entry.name == [dot] || entry.name == [dot, dot] {
                    continue;
                }
                return Some(Ok(RawEntry {
                    name: String::from_utf16(&entry.name).ok(),
                    kind: kind_of(entry.attributes),
                }));
            }
            if self.done {
                return None;
            }
            match os::read_directory(self.directory.as_handle(), !self.started) {
                Ok(Some(entries)) => {
                    self.started = true;
                    self.pending = entries.into();
                }
                Ok(None) => {
                    self.done = true;
                    return None;
                }
                Err(error) => {
                    self.done = true;
                    return Some(Err(error));
                }
            }
        }
    }
}

/// An exclusive lock on the root, released when dropped.
pub(crate) struct Lock(#[allow(dead_code)] OwnedHandle);

/// A directory beneath the root, opened without following links.
pub(crate) struct Directory(OwnedHandle);

impl Directory {
    fn handle(&self) -> BorrowedHandle<'_> {
        self.0.as_handle()
    }

    /// Who may use the regular file `name`, if there is one. A file whose
    /// access publishing could not keep is refused (see `access_of`).
    pub(crate) fn access(&self, name: &str) -> Result<Option<Access>, Refused> {
        let handle = match open(
            self.handle(),
            name,
            READ_CONTROL | FILE_READ_ATTRIBUTES,
            SHARE_ALL,
            FILE_OPEN,
            0,
        ) {
            Ok(handle) => handle,
            Err(status) if missing(status) => return Ok(None),
            Err(status) => return Err(Refused::Io(status.io_error())),
        };
        let attributes = attributes(handle.as_handle())?;
        if kind_of(attributes) != RawKind::File {
            return Ok(None);
        }
        access_of(handle.as_handle(), attributes, false).map(Some)
    }

    /// Who may use this directory, refused as a file is, except that a
    /// directory may be read-only.
    pub(crate) fn own_access(&self) -> Result<Access, Refused> {
        access_of(self.handle(), attributes(self.handle())?, true)
    }

    /// Checks that this directory, which has the access control list it
    /// inherits, has that of `access`.
    pub(crate) fn set_own_access(&self, access: &Access) -> Result<(), Refused> {
        if self.own_access()?.control == access.control {
            Ok(())
        } else {
            Err(other_list())
        }
    }

    /// Writes a new file `name` holding `bytes` and syncs it. Whatever was
    /// at `name` is removed first, so a link or a second hard link there is
    /// never written through. The new file has the access control list its
    /// directory gives it; before a byte is written, access it has that
    /// publishing could not keep is refused, and with `access` the list
    /// must be that of `access`.
    pub(crate) fn write(
        &self,
        name: &str,
        access: Option<&Access>,
        bytes: &[u8],
    ) -> Result<(), Refused> {
        self.remove_file(name)?;
        let handle = open(
            self.handle(),
            name,
            FILE_GENERIC_WRITE | READ_CONTROL | DELETE | FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_DELETE,
            FILE_CREATE,
            FILE_NON_DIRECTORY_FILE,
        )
        .map_err(|status| classify(self.handle(), name, status, &[name]))?;
        let mut file = File::from(handle);
        let written = (|| {
            let given = access_of(file.as_handle(), attributes(file.as_handle())?, false)?;
            if access.is_some_and(|access| access.control != given.control) {
                return Err(other_list());
            }
            file.write_all(bytes).map_err(Refused::Io)?;
            file.sync_all().map_err(Refused::Io)
        })();
        if written.is_err() {
            // A partial file is not left behind: this one, by its handle.
            let _ = os::delete(file.as_handle());
        }
        written
    }

    /// Renames the regular file `from` to `to` in this directory, replacing
    /// `to`.
    pub(crate) fn rename(&self, from: &str, to: &str) -> Result<(), Refused> {
        let handle = open(
            self.handle(),
            from,
            DELETE | FILE_READ_ATTRIBUTES,
            SHARE_ALL,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE,
        )
        .map_err(|status| classify(self.handle(), from, status, &[from]))?;
        if kind_of(attributes(handle.as_handle())?) != RawKind::File {
            return Err(Refused::Link(from.to_owned()));
        }
        os::rename_beneath(handle.as_handle(), self.handle(), to).map_err(Refused::Io)
    }

    /// Removes the file or link `name`; `false` when there was none.
    pub(crate) fn remove_file(&self, name: &str) -> Result<bool, Refused> {
        let handle = match open(
            self.handle(),
            name,
            DELETE | FILE_READ_ATTRIBUTES,
            SHARE_ALL,
            FILE_OPEN,
            0,
        ) {
            Ok(handle) => handle,
            Err(status) if missing(status) => return Ok(false),
            Err(status) => return Err(Refused::Io(status.io_error())),
        };
        if kind_of(attributes(handle.as_handle())?) == RawKind::Directory {
            return Err(Refused::Unexpected);
        }
        os::delete(handle.as_handle()).map_err(Refused::Io)?;
        Ok(true)
    }

    /// Removes the empty directory `name`; `false` when it is missing or
    /// not empty.
    pub(crate) fn remove_directory(&self, name: &str) -> Result<bool, Refused> {
        let handle = match open(
            self.handle(),
            name,
            DELETE | FILE_READ_ATTRIBUTES,
            SHARE_ALL,
            FILE_OPEN,
            FILE_DIRECTORY_FILE,
        ) {
            Ok(handle) => handle,
            Err(status) if missing(status) => return Ok(false),
            Err(status) => return Err(Refused::Io(status.io_error())),
        };
        if kind_of(attributes(handle.as_handle())?) != RawKind::Directory {
            return Err(Refused::Unexpected);
        }
        match os::delete(handle.as_handle()) {
            Ok(()) => Ok(true),
            Err(error) if error.raw_os_error() == i32::try_from(ERROR_DIR_NOT_EMPTY).ok() => {
                Ok(false)
            }
            Err(error) => Err(Refused::Io(error)),
        }
    }

    /// Windows cannot sync a directory: its renames and removals may not be
    /// durable yet.
    #[allow(clippy::unnecessary_wraps, clippy::unused_self)]
    pub(crate) fn sync(&self) -> Result<bool, Refused> {
        Ok(false)
    }
}

fn other_list() -> Refused {
    Refused::Io(io::Error::new(
        io::ErrorKind::Unsupported,
        "its directory gives a new entry another access control list than the one publishing must keep, and publishing does not change access control lists",
    ))
}

/// An error for access control that publishing could not keep.
fn unkept(what: &str) -> Refused {
    Refused::Io(io::Error::new(
        io::ErrorKind::Unsupported,
        format!("{what}, which publishing could not keep"),
    ))
}

/// Who may use the file or directory `handle` is open on, with
/// `READ_CONTROL` access: a digest of its access control list, compared
/// with others' and with the list its directory gives a new entry. Whether
/// an entry is marked inherited is not compared: a system gives a new entry
/// copies of its directory's entries, marked inherited only where the
/// directory's list is. Refused: an entry that is encrypted, a file that is
/// read-only, and a list protected from inheritance, a mandatory integrity
/// label or a central access policy.
fn access_of(
    handle: BorrowedHandle<'_>,
    attributes: u32,
    directory: bool,
) -> Result<Access, Refused> {
    if attributes & FILE_ATTRIBUTE_ENCRYPTED != 0 {
        return Err(unkept("it is encrypted"));
    }
    if !directory && attributes & FILE_ATTRIBUTE_READONLY != 0 {
        return Err(unkept("it is read-only"));
    }
    let descriptor = os::security(
        handle,
        DACL_SECURITY_INFORMATION | LABEL_SECURITY_INFORMATION | SCOPE_SECURITY_INFORMATION,
    )
    .map_err(Refused::Io)?;
    Ok(Access {
        mode: 0,
        user: 0,
        group: 0,
        control: control(&descriptor)?,
    })
}

fn malformed() -> Refused {
    Refused::Io(io::Error::new(
        io::ErrorKind::InvalidData,
        "malformed security descriptor",
    ))
}

fn u16_at(bytes: &[u8], at: usize) -> Result<u16, Refused> {
    bytes
        .get(at..at.checked_add(2).ok_or_else(malformed)?)
        .map(|field| u16::from_le_bytes([field[0], field[1]]))
        .ok_or_else(malformed)
}

fn u32_at(bytes: &[u8], at: usize) -> Result<usize, Refused> {
    bytes
        .get(at..at.checked_add(4).ok_or_else(malformed)?)
        .and_then(|field| field.try_into().ok())
        .map(u32::from_le_bytes)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(malformed)
}

/// An access control list's revision and its entries, each as its bytes.
type List<'a> = (u8, Vec<&'a [u8]>);

/// The access control list at `offset` of a self-relative security
/// descriptor, if `present`.
fn acl(descriptor: &[u8], present: bool, offset: usize) -> Result<Option<List<'_>>, Refused> {
    if !present || offset == 0 {
        return Ok(None);
    }
    // ACL: revision, padding, size (u16), entry count (u16), padding.
    let size = usize::from(u16_at(
        descriptor,
        offset.checked_add(2).ok_or_else(malformed)?,
    )?);
    let list = descriptor
        .get(offset..offset.checked_add(size).ok_or_else(malformed)?)
        .ok_or_else(malformed)?;
    let revision = *list.first().ok_or_else(malformed)?;
    let count = u16_at(list, 4)?;
    let mut entries = Vec::with_capacity(usize::from(count));
    let mut at = 8_usize;
    for _ in 0..count {
        // ACE header: type, flags, size (u16).
        let entry_size = usize::from(u16_at(list, at + 2)?);
        let entry = list
            .get(at..at + entry_size)
            .filter(|_| entry_size >= 4)
            .ok_or_else(malformed)?;
        entries.push(entry);
        at += entry_size;
    }
    Ok(Some((revision, entries)))
}

/// A digest of a self-relative security descriptor's access control list,
/// its entries' inherited marks aside. A list protected from inheritance,
/// and a label or central access policy, are refused.
fn control(descriptor: &[u8]) -> Result<[u8; 32], Refused> {
    let flags = u16_at(descriptor, 2)?;
    if flags & SE_SELF_RELATIVE == 0 {
        return Err(malformed());
    }
    if flags & SE_DACL_PROTECTED != 0 {
        return Err(unkept(
            "its access control list is protected from inheritance",
        ));
    }
    // Asked for labels and policies only, the system list holds no other
    // entries.
    let system = acl(
        descriptor,
        flags & SE_SACL_PRESENT != 0,
        u32_at(descriptor, 12)?,
    )?;
    if system.is_some_and(|(_, entries)| !entries.is_empty()) {
        return Err(unkept(
            "it has a mandatory integrity label or a central access policy",
        ));
    }
    let mut hasher = Sha256::new();
    match acl(
        descriptor,
        flags & SE_DACL_PRESENT != 0,
        u32_at(descriptor, 16)?,
    )? {
        // No list: everyone may use it, as on a file system without them.
        None => hasher.update(b"no access control list"),
        Some((revision, entries)) => {
            let inherited = u8::try_from(INHERITED_ACE).unwrap_or(u8::MAX);
            hasher.update(b"access control list");
            hasher.update([revision]);
            hasher.update(
                u64::try_from(entries.len())
                    .unwrap_or(u64::MAX)
                    .to_le_bytes(),
            );
            for entry in entries {
                hasher.update(&entry[..1]);
                hasher.update([entry[1] & !inherited]);
                hasher.update(&entry[2..]);
            }
        }
    }
    Ok(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::control;

    /// A self-relative descriptor with the control flags and an access
    /// control list whose entries have the flags `entries`, and `slack`
    /// unused bytes at its end.
    fn descriptor(flags: u16, entries: &[u8], slack: usize) -> Vec<u8> {
        let mut bytes = vec![1, 0];
        bytes.extend(flags.to_le_bytes());
        bytes.extend([0_u8; 8]); // owner, group
        bytes.extend(0_u32.to_le_bytes()); // no system list
        bytes.extend(20_u32.to_le_bytes()); // the list follows
        let size = 8 + entries.len() * 8 + slack;
        bytes.extend([2, 0]);
        bytes.extend(u16::try_from(size).unwrap().to_le_bytes());
        bytes.extend(u16::try_from(entries.len()).unwrap().to_le_bytes());
        bytes.extend([0, 0]);
        for flags in entries {
            bytes.extend([0, *flags, 8, 0, 0, 0, 0, 0]);
        }
        bytes.extend(vec![0; slack]);
        bytes
    }

    #[test]
    fn lists_are_compared_without_inherited_marks() {
        let present = 0x8000 | 0x0004;
        let list = control(&descriptor(present, &[0x10, 0x13], 0)).unwrap();
        assert_ne!(list, [0; 32]);
        // Marked inherited or not, and with room to spare, it is one list.
        assert_eq!(
            control(&descriptor(present, &[0x00, 0x03], 0)).unwrap(),
            list
        );
        assert_eq!(
            control(&descriptor(present, &[0x10, 0x13], 16)).unwrap(),
            list
        );
        // Other entries are another.
        assert_ne!(control(&descriptor(present, &[0x10], 0)).unwrap(), list);
        assert_ne!(
            control(&descriptor(present, &[0x10, 0x11], 0)).unwrap(),
            list
        );
        // A protected list, and a malformed one, are refused.
        assert!(control(&descriptor(present | 0x1000, &[0x10], 0)).is_err());
        let mut short = descriptor(present, &[0x10], 0);
        short.truncate(short.len() - 1);
        assert!(control(&short).is_err());
        assert!(control(&descriptor(present & !0x8000, &[0x10], 0)).is_err());
        // No list at all.
        assert!(control(&descriptor(0x8000, &[], 0)).is_ok());
    }
}
