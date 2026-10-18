// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Unix access beneath the root through directory handles. Each component
//! is opened relative to the directory before it with `O_NOFOLLOW`, so a
//! symbolic link is never followed, not even one swapped in after a check.

use super::{Found, RawEntry, RawKind, Refused};
use rustix::{
    fs::{AtFlags, CWD, Dir, FileType, Mode, OFlags, fstat, openat, statat},
    io::Errno,
};
use std::{
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
