// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Access beneath the root by path, for platforms without directory
//! handles: every component is checked for a symbolic link before use. A
//! link swapped in between the check and the use is not detected. There is
//! no lock.

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

fn kind_of(file_type: fs::FileType) -> RawKind {
    if file_type.is_symlink() {
        RawKind::Link
    } else if file_type.is_dir() {
        RawKind::Directory
    } else if file_type.is_file() {
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
            current.push(component);
            metadata = fs::symlink_metadata(&current).map_err(refused)?;
            if metadata.file_type().is_symlink() {
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
            Ok((_, metadata)) => Ok(match kind_of(metadata.file_type()) {
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
            if metadata.file_type().is_symlink() {
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

    #[allow(clippy::unnecessary_wraps, clippy::unused_self)]
    pub(crate) fn lock(&self) -> Result<Option<Lock>, Refused> {
        Ok(Some(Lock))
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
        Some(entry.file_type().map(|file_type| RawEntry {
            name: entry.file_name().into_string().ok(),
            kind: kind_of(file_type),
        }))
    }
}

pub(crate) struct Lock;

pub(crate) struct Directory(PathBuf);

impl Directory {
    /// The entry `name`, refused if it is a link.
    fn entry(&self, name: &str) -> Result<Option<fs::Metadata>, Refused> {
        match fs::symlink_metadata(self.0.join(name)) {
            Ok(metadata) => Ok(Some(metadata)),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(Refused::Io(error)),
        }
    }

    pub(crate) fn access(&self, name: &str) -> Result<Option<Access>, Refused> {
        Ok(self
            .entry(name)?
            .filter(fs::Metadata::is_file)
            .map(|metadata| access_of(&metadata)))
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

    pub(crate) fn rename(&self, from: &str, to: &str) -> Result<(), Refused> {
        fs::rename(self.0.join(from), self.0.join(to)).map_err(Refused::Io)
    }

    pub(crate) fn remove_file(&self, name: &str) -> Result<bool, Refused> {
        match self.entry(name)? {
            None => Ok(false),
            Some(metadata) if metadata.is_dir() => Err(Refused::Unexpected),
            Some(_) => fs::remove_file(self.0.join(name))
                .map(|()| true)
                .map_err(Refused::Io),
        }
    }

    pub(crate) fn remove_directory(&self, name: &str) -> Result<bool, Refused> {
        match self.entry(name)? {
            Some(metadata) if metadata.is_dir() => match fs::remove_dir(self.0.join(name)) {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == ErrorKind::DirectoryNotEmpty => Ok(false),
                Err(error) => Err(Refused::Io(error)),
            },
            Some(_) => Err(Refused::Unexpected),
            None => Ok(false),
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
        let all = if metadata.is_dir() { 0o777 } else { 0o666 };
        Access {
            mode: if metadata.permissions().readonly() {
                all & 0o555
            } else {
                all
            },
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
