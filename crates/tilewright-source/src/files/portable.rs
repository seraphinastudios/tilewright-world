// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Access beneath the root by path, for platforms without directory
//! handles: every component is checked for a symbolic link before use, on
//! Windows for any reparse point (junctions and mount points among them). A
//! link swapped in between the check and the use is not detected, so a
//! local process changing the tree at the same moment can redirect an
//! access. On Windows the lock is a file in the root, held exclusively;
//! elsewhere there is no lock.

use super::{Found, RawEntry, RawKind, Refused};
use crate::access::Access;
use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

/// The root's absolute path, or why it could not be resolved: then every
/// access fails, rather than resolving a relative path later.
pub(crate) struct Root(Result<PathBuf, ErrorKind>);

/// Whether an entry is a reparse point: on Windows any, which is not
/// followed; elsewhere none but the links `is_symlink` reports.
fn reparse_point(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}

/// Whether an entry, as `symlink_metadata` gives it, is a link: a symbolic
/// link, or on Windows any reparse point.
fn is_link(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink() || reparse_point(metadata)
}

fn kind_of(metadata: &fs::Metadata) -> RawKind {
    if is_link(metadata) {
        RawKind::Link
    } else if metadata.is_dir() {
        RawKind::Directory
    } else if metadata.is_file() {
        RawKind::File
    } else {
        RawKind::Other
    }
}

impl Root {
    /// The root, resolved to an absolute path once, so that a later change
    /// of the working directory does not move it. A path that cannot be
    /// resolved yet, such as a missing one, is still made absolute.
    pub(crate) fn new(path: &Path) -> Self {
        Self(
            fs::canonicalize(path)
                .or_else(|_| std::path::absolute(path))
                .map_err(|error| error.kind()),
        )
    }

    pub(crate) fn try_clone(&self) -> Self {
        Self(self.0.clone())
    }

    /// The root's path, if it was resolved.
    fn base(&self) -> Result<PathBuf, Refused> {
        self.0.clone().map_err(|kind| Refused::Io(kind.into()))
    }

    /// The path of `components` and its metadata, after checking that no
    /// component is a link.
    fn checked(&self, components: &[&str]) -> Result<(PathBuf, fs::Metadata), Refused> {
        let mut current = self.base()?;
        let mut metadata = fs::metadata(&current).map_err(refused)?;
        for (index, component) in components.iter().enumerate() {
            // Beneath a file there is nothing, which Windows reports as a
            // missing path rather than as a file in the way.
            if !metadata.is_dir() {
                return Err(Refused::Unexpected);
            }
            current.push(component);
            metadata = fs::symlink_metadata(&current).map_err(refused)?;
            if is_link(&metadata) {
                return Err(Refused::Link(components[..=index].join("/")));
            }
        }
        Ok((current, metadata))
    }

    pub(crate) fn file(&self, components: &[&str]) -> Result<(fs::File, u64), Refused> {
        let (path, metadata) = self.checked(components)?;
        if components.is_empty() || !metadata.is_file() {
            return Err(Refused::Unexpected);
        }
        Ok((fs::File::open(path).map_err(Refused::Io)?, metadata.len()))
    }

    pub(crate) fn kind(&self, components: &[&str]) -> Result<Found, Refused> {
        match self.checked(components) {
            Ok((_, metadata)) => Ok(match kind_of(&metadata) {
                RawKind::File => Found::File,
                RawKind::Directory => Found::Directory,
                RawKind::Link | RawKind::Other => Found::Other,
            }),
            Err(Refused::Missing) => Ok(Found::Missing),
            Err(refused) => Err(refused),
        }
    }

    pub(crate) fn directory(
        &self,
        components: &[&str],
        create: bool,
    ) -> Result<Directory, Refused> {
        let mut current = self.base()?;
        fs::metadata(&current).map_err(refused)?;
        for (index, component) in components.iter().enumerate() {
            current.push(component);
            let metadata = match fs::symlink_metadata(&current) {
                Err(error) if error.kind() == ErrorKind::NotFound && create => {
                    match fs::create_dir(&current) {
                        Err(error) if error.kind() != ErrorKind::AlreadyExists => {
                            return Err(Refused::Io(error));
                        }
                        _ => {}
                    }
                    fs::symlink_metadata(&current)
                }
                metadata => metadata,
            }
            .map_err(refused)?;
            if is_link(&metadata) {
                return Err(Refused::Link(components[..=index].join("/")));
            }
            if !metadata.is_dir() {
                return Err(Refused::Unexpected);
            }
        }
        Ok(Directory(current))
    }

    pub(crate) fn make_directory(&self, components: &[&str]) -> Result<bool, Refused> {
        let Some((_, parent)) = components.split_last() else {
            return Ok(false);
        };
        self.directory(parent, false)?;
        let mut path = self.base()?;
        path.extend(components);
        match fs::create_dir(&path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(Refused::Io(error)),
        }
    }

    /// Takes the root's lock: on Windows the file [`LOCK_NAME`] in the
    /// root, created afresh, shared with no one and deleted when the lock
    /// is dropped, or by the system if the process stops; `None` when
    /// another holds it. One that no one holds is reported, not taken over,
    /// since it may be anyone's. Elsewhere there is no lock.
    ///
    /// [`LOCK_NAME`]: crate::access::LOCK_NAME
    #[cfg(windows)]
    pub(crate) fn lock(&self) -> Result<Option<Lock>, Refused> {
        use std::os::windows::fs::OpenOptionsExt as _;
        const GENERIC_WRITE: u32 = 0x4000_0000;
        const DELETE: u32 = 0x0001_0000;
        const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const ERROR_SHARING_VIOLATION: i32 = 32;
        let path = self.base()?.join(crate::access::LOCK_NAME);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .access_mode(GENERIC_WRITE | DELETE)
            .share_mode(0)
            .custom_flags(FILE_FLAG_DELETE_ON_CLOSE)
            .open(&path)
        {
            Ok(file) => return Ok(Some(Lock(Some(file)))),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(Refused::Io(error)),
        }
        // Something is there: a lock another holds cannot be opened even
        // to look at it.
        match fs::OpenOptions::new()
            .access_mode(DELETE)
            .share_mode(0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(&path)
        {
            Err(error) if error.raw_os_error() == Some(ERROR_SHARING_VIOLATION) => Ok(None),
            Err(error) if error.kind() == ErrorKind::NotFound => {
                Err(Refused::Io(std::io::Error::new(
                    ErrorKind::WouldBlock,
                    "the lock was released while it was taken; try again",
                )))
            }
            Err(error) => Err(Refused::Io(error)),
            Ok(_) => Err(Refused::Io(std::io::Error::new(
                ErrorKind::AlreadyExists,
                format!(
                    "{} is in the root but no edit holds it; remove it if it is not wanted",
                    crate::access::LOCK_NAME
                ),
            ))),
        }
    }

    #[cfg(not(windows))]
    #[allow(clippy::unnecessary_wraps, clippy::unused_self)]
    pub(crate) fn lock(&self) -> Result<Option<Lock>, Refused> {
        Ok(Some(Lock(None)))
    }

    pub(crate) fn list(&self, components: &[&str]) -> Result<Listing, Refused> {
        let (path, metadata) = self.checked(components)?;
        if !metadata.is_dir() {
            return Err(Refused::Unexpected);
        }
        Ok(Listing(fs::read_dir(path).map_err(Refused::Io)?))
    }
}

fn refused(error: std::io::Error) -> Refused {
    match error.kind() {
        ErrorKind::NotFound => Refused::Missing,
        ErrorKind::NotADirectory => Refused::Unexpected,
        _ => Refused::Io(error),
    }
}

pub(crate) struct Listing(fs::ReadDir);

impl Iterator for Listing {
    type Item = std::io::Result<RawEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        let entry = match self.0.next()? {
            Ok(entry) => entry,
            Err(error) => return Some(Err(error)),
        };
        // An entry's metadata, not following a link; on Windows it comes
        // with the listing.
        Some(entry.metadata().map(|metadata| RawEntry {
            name: entry.file_name().into_string().ok(),
            kind: kind_of(&metadata),
        }))
    }
}

/// The root's lock, where there is one, released when dropped.
pub(crate) struct Lock(#[allow(dead_code)] Option<fs::File>);

pub(crate) struct Directory(PathBuf);

impl Directory {
    /// The entry `name`, not followed if it is a link.
    fn entry(&self, name: &str) -> Result<Option<fs::Metadata>, Refused> {
        match fs::symlink_metadata(self.0.join(name)) {
            Ok(metadata) => Ok(Some(metadata)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(Refused::Io(error)),
        }
    }

    /// Who may use the regular file `name`; `None` when there is none, or a
    /// link. Where there are no modes, a read-only file is refused, since
    /// replacing it would drop that.
    pub(crate) fn access(&self, name: &str) -> Result<Option<Access>, Refused> {
        let Some(metadata) = self
            .entry(name)?
            .filter(|metadata| kind_of(metadata) == RawKind::File)
        else {
            return Ok(None);
        };
        if cfg!(not(unix)) && metadata.permissions().readonly() {
            return Err(Refused::Io(std::io::Error::new(
                ErrorKind::Unsupported,
                "it is read-only, which publishing could not keep",
            )));
        }
        Ok(Some(access_of(&metadata)))
    }

    pub(crate) fn own_access(&self) -> Result<Access, Refused> {
        fs::metadata(&self.0)
            .map(|metadata| access_of(&metadata))
            .map_err(Refused::Io)
    }

    pub(crate) fn set_own_access(&self, access: &Access) -> Result<(), Refused> {
        let current = fs::metadata(&self.0).map_err(Refused::Io)?.permissions();
        fs::set_permissions(&self.0, permissions_of(access, &current)).map_err(Refused::Io)
    }

    /// Writes a new file; with `access`, it gets that mode after writing.
    /// Groups are not kept here.
    pub(crate) fn write(
        &self,
        name: &str,
        access: Option<&Access>,
        bytes: &[u8],
    ) -> Result<(), Refused> {
        use std::io::Write as _;
        self.remove_file(name)?;
        let path = self.0.join(name);
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        // A file with permissions of its own is only its owner's until it
        // has them, where the platform allows; they are set after writing.
        #[cfg(unix)]
        if access.is_some() {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&path).map_err(Refused::Io)?;
        let written = (|| {
            file.write_all(bytes).map_err(Refused::Io)?;
            if let Some(access) = access {
                let current = file.metadata().map_err(Refused::Io)?.permissions();
                file.set_permissions(permissions_of(access, &current))
                    .map_err(Refused::Io)?;
            }
            file.sync_all().map_err(Refused::Io)
        })();
        if written.is_err() {
            // A partial file is not left behind.
            drop(file);
            let _ = fs::remove_file(&path);
        }
        written
    }

    /// There are no labels a file's name decides here.
    #[allow(clippy::unnecessary_wraps, clippy::unused_self, dead_code)]
    pub(crate) fn check_final_name(&self, _: &str, _: &str) -> Result<(), Refused> {
        Ok(())
    }

    /// Renames the regular file `from` to `to`, replacing `to`.
    pub(crate) fn rename(&self, from: &str, to: &str) -> Result<(), Refused> {
        match self.entry(from)? {
            Some(metadata) if kind_of(&metadata) == RawKind::File => {}
            Some(metadata) if kind_of(&metadata) == RawKind::Link => {
                return Err(Refused::Link(from.to_owned()));
            }
            Some(_) => return Err(Refused::Unexpected),
            None => return Err(Refused::Missing),
        }
        fs::rename(self.0.join(from), self.0.join(to)).map_err(Refused::Io)
    }

    /// Removes the file or link `name`, a link to a directory included;
    /// `false` when there was none.
    pub(crate) fn remove_file(&self, name: &str) -> Result<bool, Refused> {
        let path = self.0.join(name);
        match self.entry(name)? {
            None => Ok(false),
            Some(metadata) => match kind_of(&metadata) {
                RawKind::Directory => Err(Refused::Unexpected),
                // On Windows a link to a directory, a junction among them,
                // is removed as a directory, which removes the link alone.
                RawKind::Link if metadata.is_dir() || directory_link(&metadata) => {
                    fs::remove_dir(&path).map_err(Refused::Io)?;
                    self.gone(name).map(|()| true)
                }
                _ => {
                    fs::remove_file(&path).map_err(Refused::Io)?;
                    self.gone(name).map(|()| true)
                }
            },
        }
    }

    pub(crate) fn remove_directory(&self, name: &str) -> Result<bool, Refused> {
        match self.entry(name)? {
            Some(metadata) if kind_of(&metadata) == RawKind::Directory => {
                match fs::remove_dir(self.0.join(name)) {
                    Ok(()) => self.gone(name).map(|()| true),
                    Err(error) if error.kind() == ErrorKind::DirectoryNotEmpty => Ok(false),
                    Err(error) => Err(Refused::Io(error)),
                }
            }
            Some(_) => Err(Refused::Unexpected),
            None => Ok(false),
        }
    }

    /// Checks that `name`, just removed, is gone: on Windows a removed
    /// entry may stay while another program has it open.
    fn gone(&self, name: &str) -> Result<(), Refused> {
        match self.entry(name) {
            Ok(None) => Ok(()),
            Ok(Some(_)) | Err(_) => Err(Refused::Io(std::io::Error::new(
                ErrorKind::ResourceBusy,
                "it is not removed until another program closes it",
            ))),
        }
    }

    /// Syncs the directory where the platform can; `false` where it
    /// cannot, so a caller knows the directory's entries may not be
    /// durable yet.
    #[allow(clippy::unnecessary_wraps, clippy::unused_self)]
    pub(crate) fn sync(&self) -> Result<bool, Refused> {
        #[cfg(unix)]
        {
            fs::File::open(&self.0)
                .and_then(|directory| directory.sync_all())
                .map_err(Refused::Io)?;
            Ok(true)
        }
        #[cfg(not(unix))]
        Ok(false)
    }
}

/// Whether a link, as `symlink_metadata` gives it, is one to a directory,
/// which Windows removes as a directory.
fn directory_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
        metadata.file_attributes() & FILE_ATTRIBUTE_DIRECTORY != 0
    }
    #[cfg(not(windows))]
    {
        let _ = metadata;
        false
    }
}

/// Who may use a file or directory: its mode where there is one, and
/// elsewhere read-only or not.
fn access_of(metadata: &fs::Metadata) -> Access {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        Access {
            mode: metadata.mode() & 0o7777,
            user: metadata.uid(),
            group: metadata.gid(),
            control: [0; 32],
        }
    }
    #[cfg(not(unix))]
    {
        // Windows does not honour the read-only attribute on a directory,
        // so it says nothing of the files made in one.
        let mode = if metadata.is_dir() {
            0o777
        } else if metadata.permissions().readonly() {
            0o444
        } else {
            0o666
        };
        Access {
            mode,
            user: 0,
            group: 0,
            control: [0; 32],
        }
    }
}

/// The permissions of `access`: its mode where there is one, and
/// elsewhere `current` made read-only when `access` allows no writing.
fn permissions_of(access: &Access, current: &fs::Permissions) -> fs::Permissions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = current;
        fs::Permissions::from_mode(access.mode)
    }
    #[cfg(not(unix))]
    {
        let mut permissions = current.clone();
        permissions.set_readonly(access.mode & 0o222 == 0);
        permissions
    }
}
