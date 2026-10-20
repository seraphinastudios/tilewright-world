// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Unix access beneath the root through directory handles. Each component
//! is opened relative to the directory before it with `O_NOFOLLOW`, so a
//! symbolic link is never followed, not even one swapped in after a check.
//! Writes, renames and removals act on names inside a directory opened that
//! way, so they never follow a link either.

use super::{Found, RawEntry, RawKind, Refused};
use rustix::{
    fs::{
        AtFlags, CWD, Dir, FileType, FlockOperation, Mode, OFlags, fchmod, flock, fstat, fsync,
        mkdirat, openat, renameat, statat, unlinkat,
    },
    io::Errno,
};
use std::{
    io::Write as _,
    os::fd::{AsFd, BorrowedFd, OwnedFd},
    path::Path,
};

/// The root directory, opened once so that every access is relative to it.
pub(crate) struct Root(Result<OwnedFd, Errno>);

fn directory_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC
}

fn kind_of(file_type: FileType) -> RawKind {
    match file_type {
        FileType::RegularFile => RawKind::File,
        FileType::Directory => RawKind::Directory,
        FileType::Symlink => RawKind::Link,
        _ => RawKind::Other,
    }
}

/// Why opening `name` in `directory` failed; `prefix` is the path up to and
/// including `name`.
fn classify(directory: BorrowedFd<'_>, name: &str, errno: Errno, prefix: &[&str]) -> Refused {
    match statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) if kind_of(FileType::from_raw_mode(stat.st_mode)) == RawKind::Link => {
            Refused::Link(prefix.join("/"))
        }
        Ok(_) if errno == Errno::NOTDIR || errno == Errno::NXIO => Refused::Unexpected,
        _ if errno == Errno::NOENT => Refused::Missing,
        _ => Refused::Io(errno.into()),
    }
}

impl Root {
    pub(crate) fn new(path: &Path) -> Self {
        // The root itself may be a link; nothing beneath it is followed.
        Self(openat(
            CWD,
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        ))
    }

    /// A second handle on the same directory.
    pub(crate) fn try_clone(&self) -> Self {
        Self(match &self.0 {
            Ok(fd) => fd.try_clone().map_err(|error| {
                error
                    .raw_os_error()
                    .map_or(Errno::IO, Errno::from_raw_os_error)
            }),
            Err(errno) => Err(*errno),
        })
    }

    fn root(&self) -> Result<BorrowedFd<'_>, Refused> {
        match &self.0 {
            Ok(fd) => Ok(fd.as_fd()),
            Err(Errno::NOENT) => Err(Refused::Missing),
            Err(errno) => Err(Refused::Io((*errno).into())),
        }
    }

    /// Runs `access` on the directory holding the last of `components` and
    /// that component's name, opening the directories one at a time.
    fn parent<T>(
        &self,
        components: &[&str],
        access: impl FnOnce(BorrowedFd<'_>, &str) -> Result<T, Refused>,
    ) -> Result<T, Refused> {
        let root = self.root()?;
        let Some((name, parents)) = components.split_last() else {
            return Err(Refused::Unexpected);
        };
        let mut current: Option<OwnedFd> = None;
        for (index, component) in parents.iter().enumerate() {
            let directory = current.as_ref().map_or(root, AsFd::as_fd);
            let opened = openat(directory, *component, directory_flags(), Mode::empty())
                .map_err(|errno| classify(directory, component, errno, &components[..=index]))?;
            current = Some(opened);
        }
        access(current.as_ref().map_or(root, AsFd::as_fd), name)
    }

    /// Opens a regular file and returns it with its length.
    pub(crate) fn file(&self, components: &[&str]) -> Result<(std::fs::File, u64), Refused> {
        self.parent(components, |directory, name| {
            // Non-blocking, so opening a FIFO cannot hang before it is rejected.
            let flags = OFlags::RDONLY
                | OFlags::NOFOLLOW
                | OFlags::CLOEXEC
                | OFlags::NOCTTY
                | OFlags::NONBLOCK;
            let fd = openat(directory, name, flags, Mode::empty())
                .map_err(|errno| classify(directory, name, errno, components))?;
            let stat = fstat(&fd).map_err(|errno| Refused::Io(errno.into()))?;
            if kind_of(FileType::from_raw_mode(stat.st_mode)) != RawKind::File {
                return Err(Refused::Unexpected);
            }
            Ok((
                std::fs::File::from(fd),
                u64::try_from(stat.st_size).unwrap_or(0),
            ))
        })
    }

    /// What the last of `components` is, without following it.
    pub(crate) fn kind(&self, components: &[&str]) -> Result<Found, Refused> {
        self.parent(components, |directory, name| {
            match statat(directory, name, AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => match kind_of(FileType::from_raw_mode(stat.st_mode)) {
                    RawKind::File => Ok(Found::File),
                    RawKind::Directory => Ok(Found::Directory),
                    RawKind::Link => Err(Refused::Link(components.join("/"))),
                    RawKind::Other => Ok(Found::Other),
                },
                Err(Errno::NOENT) => Ok(Found::Missing),
                Err(errno) => Err(Refused::Io(errno.into())),
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
        let mut current: Option<OwnedFd> = None;
        for (index, component) in components.iter().enumerate() {
            let directory = current.as_ref().map_or(root, AsFd::as_fd);
            let opened = match openat(directory, *component, directory_flags(), Mode::empty()) {
                Err(Errno::NOENT) if create => {
                    match mkdirat(directory, *component, Mode::from_raw_mode(0o777)) {
                        Ok(()) | Err(Errno::EXIST) => {}
                        Err(errno) => return Err(Refused::Io(errno.into())),
                    }
                    openat(directory, *component, directory_flags(), Mode::empty())
                }
                opened => opened,
            }
            .map_err(|errno| classify(directory, component, errno, &components[..=index]))?;
            current = Some(opened);
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
        match mkdirat(&parent.0, *name, Mode::from_raw_mode(0o777)) {
            Ok(()) => Ok(true),
            Err(Errno::EXIST) => Ok(false),
            Err(errno) => Err(classify(parent.0.as_fd(), name, errno, components)),
        }
    }

    /// Takes an exclusive advisory lock on the root directory, held until
    /// the returned lock is dropped; `None` when another holds it.
    pub(crate) fn lock(&self) -> Result<Option<Lock>, Refused> {
        let fd = openat(self.root()?, ".", directory_flags(), Mode::empty())
            .map_err(|errno| Refused::Io(errno.into()))?;
        match flock(&fd, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(Some(Lock(fd))),
            Err(Errno::WOULDBLOCK) => Ok(None),
            Err(errno) => Err(Refused::Io(errno.into())),
        }
    }

    /// Lists a directory. Entries are read as they are iterated.
    pub(crate) fn list(&self, components: &[&str]) -> Result<Listing, Refused> {
        let directory = if components.is_empty() {
            self.root()?.try_clone_to_owned().map_err(Refused::Io)?
        } else {
            self.parent(components, |directory, name| {
                openat(directory, name, directory_flags(), Mode::empty())
                    .map_err(|errno| classify(directory, name, errno, components))
            })?
        };
        let entries = Dir::read_from(&directory).map_err(|errno| Refused::Io(errno.into()))?;
        Ok(Listing {
            directory,
            entries,
            done: false,
        })
    }
}

pub(crate) struct Listing {
    directory: OwnedFd,
    entries: Dir,
    done: bool,
}

impl Iterator for Listing {
    type Item = std::io::Result<RawEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        while !self.done {
            let entry = match self.entries.next()? {
                Ok(entry) => entry,
                Err(errno) => {
                    self.done = true;
                    return Some(Err(errno.into()));
                }
            };
            let name = entry.file_name();
            if matches!(name.to_bytes(), b"." | b"..") {
                continue;
            }
            let kind = match entry.file_type() {
                FileType::Unknown => {
                    match statat(&self.directory, name, AtFlags::SYMLINK_NOFOLLOW) {
                        Ok(stat) => kind_of(FileType::from_raw_mode(stat.st_mode)),
                        Err(errno) => return Some(Err(errno.into())),
                    }
                }
                known => kind_of(known),
            };
            return Some(Ok(RawEntry {
                name: std::str::from_utf8(name.to_bytes()).ok().map(str::to_owned),
                kind,
            }));
        }
        None
    }
}

/// An exclusive lock on the root, released when dropped.
pub(crate) struct Lock(#[allow(dead_code)] OwnedFd);

/// A directory beneath the root, opened without following links.
pub(crate) struct Directory(OwnedFd);

impl Directory {
    /// Writes a new file `name` holding `bytes` and syncs it. Whatever was at
    /// `name` is removed first, so a link or a second hard link there is
    /// never written through. The file takes the permissions of the regular
    /// file `like`, if there is one.
    /// The permissions of the regular file `name`, if there is one.
    pub(crate) fn permissions(&self, name: &str) -> Result<Option<std::fs::Permissions>, Refused> {
        use std::os::unix::fs::PermissionsExt as _;
        match statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) if kind_of(FileType::from_raw_mode(stat.st_mode)) == RawKind::File => {
                #[cfg(target_os = "linux")]
                if self.has_access_control_list(name)? {
                    return Err(Refused::Io(std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        "it has an access control list, which a file replacing it could not keep",
                    )));
                }
                Ok(Some(std::fs::Permissions::from_mode(
                    Mode::from_raw_mode(stat.st_mode).as_raw_mode(),
                )))
            }
            Ok(_) | Err(Errno::NOENT) => Ok(None),
            Err(errno) => Err(Refused::Io(errno.into())),
        }
    }

    /// Whether the file `name` has an access control list beyond its mode
    /// bits.
    #[cfg(target_os = "linux")]
    fn has_access_control_list(&self, name: &str) -> Result<bool, Refused> {
        let flags =
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC | OFlags::NOCTTY;
        let fd = openat(&self.0, name, flags, Mode::empty())
            .map_err(|errno| classify(self.0.as_fd(), name, errno, &[name]))?;
        access_control_list(&fd)
    }

    pub(crate) fn write(
        &self,
        name: &str,
        permissions: Option<&std::fs::Permissions>,
        bytes: &[u8],
    ) -> Result<(), Refused> {
        use std::os::unix::fs::PermissionsExt as _;
        self.remove_file(name)?;
        let flags = OFlags::WRONLY
            | OFlags::CREATE
            | OFlags::EXCL
            | OFlags::NOFOLLOW
            | OFlags::CLOEXEC
            | OFlags::NOCTTY;
        // A file with permissions of its own is only its owner's until it
        // has them: they are set after writing, which would clear set-user
        // and set-group bits set before.
        let mode = if permissions.is_some() { 0o600 } else { 0o666 };
        let fd = openat(&self.0, name, flags, Mode::from_raw_mode(mode))
            .map_err(|errno| classify(self.0.as_fd(), name, errno, &[name]))?;
        let written = (|| {
            let mut file = std::fs::File::from(fd);
            file.write_all(bytes).map_err(Refused::Io)?;
            if let Some(permissions) = permissions {
                // An access control list the directory gives new files
                // would let others read what the mode bits do not.
                #[cfg(target_os = "linux")]
                strip_access_control_list(&file)?;
                fchmod(&file, Mode::from_raw_mode(permissions.mode()))
                    .map_err(|errno| Refused::Io(errno.into()))?;
            }
            file.sync_all().map_err(Refused::Io)
        })();
        if written.is_err() {
            // A partial file is not left behind.
            let _ = self.remove_file(name);
        }
        written
    }

    /// Renames `from` to `to` in this directory, replacing `to`.
    pub(crate) fn rename(&self, from: &str, to: &str) -> Result<(), Refused> {
        renameat(&self.0, from, &self.0, to).map_err(|errno| Refused::Io(errno.into()))
    }

    /// Removes the file or link `name`; `false` when there was none.
    pub(crate) fn remove_file(&self, name: &str) -> Result<bool, Refused> {
        match unlinkat(&self.0, name, AtFlags::empty()) {
            Ok(()) => Ok(true),
            Err(Errno::NOENT) => Ok(false),
            Err(errno) => Err(Refused::Io(errno.into())),
        }
    }

    /// Removes the empty directory `name`; `false` when it is missing or
    /// not empty.
    pub(crate) fn remove_directory(&self, name: &str) -> Result<bool, Refused> {
        match unlinkat(&self.0, name, AtFlags::REMOVEDIR) {
            Ok(()) => Ok(true),
            Err(Errno::NOENT | Errno::NOTEMPTY | Errno::EXIST) => Ok(false),
            Err(errno) => Err(Refused::Io(errno.into())),
        }
    }

    /// Syncs the directory, so renames and removals in it are durable.
    pub(crate) fn sync(&self) -> Result<bool, Refused> {
        fsync(&self.0)
            .map(|()| true)
            .map_err(|errno| Refused::Io(errno.into()))
    }
}

/// The access control lists Linux file systems keep beside the mode bits.
#[cfg(target_os = "linux")]
const ACCESS_CONTROL_LISTS: [&str; 3] = [
    "system.posix_acl_access",
    "system.nfs4_acl",
    "system.richacl",
];

/// Whether the open file has an access control list beyond its mode bits:
/// a POSIX, NFS version 4 or rich one.
#[cfg(target_os = "linux")]
fn access_control_list(fd: impl AsFd) -> Result<bool, Refused> {
    for attribute in ACCESS_CONTROL_LISTS {
        // Asked with no room, the call gives the size of a value that is
        // there, or says there is none.
        match rustix::fs::fgetxattr(&fd, attribute, &mut [0_u8; 0][..]) {
            Ok(_) | Err(Errno::RANGE) => return Ok(true),
            Err(Errno::NODATA | Errno::NOTSUP) => {}
            Err(errno) => return Err(Refused::Io(errno.into())),
        }
    }
    Ok(false)
}

/// Removes the POSIX access control list a new file got from its
/// directory, so that its mode bits alone say who may read it; any other
/// list that stays is an error.
#[cfg(target_os = "linux")]
fn strip_access_control_list(fd: impl AsFd) -> Result<(), Refused> {
    match rustix::fs::fremovexattr(&fd, "system.posix_acl_access") {
        Ok(()) | Err(Errno::NODATA | Errno::NOTSUP) => {}
        Err(errno) => return Err(Refused::Io(errno.into())),
    }
    if access_control_list(&fd)? {
        return Err(Refused::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "the directory gives new files an access control list that cannot be removed",
        )));
    }
    Ok(())
}
