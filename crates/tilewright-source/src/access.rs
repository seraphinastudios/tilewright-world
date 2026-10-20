// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Confined access to the files of a source tree, for tools that edit it.
//!
//! This is the loader's own path confinement: paths are root-relative and
//! `/`-separated, with no empty, `.` or `..` component and no `\`, `:` or
//! NUL in any component, and nothing beneath the root is followed if it is
//! a symbolic link. On Unix every
//! component is opened through its parent's directory handle without
//! following links, and writes, renames and removals act on names inside a
//! directory opened that way, so a link swapped in at any moment is never
//! followed. Elsewhere components are checked by path before use, a best
//! effort that a local process changing the tree at the same time can race.

use crate::files::{Refused, components, platform};
use std::{fmt, io::Read as _, path::Path};

/// Why a path beneath the root could not be used.
#[derive(Debug)]
pub enum AccessError {
    /// The path has an empty, `.` or `..` component, or a component with
    /// `\`, `:` or NUL; or a name has a `/`.
    Path(String),
    /// A symbolic link at this prefix of the path.
    Link(String),
    /// The entry is not a regular file or directory, as the access needs.
    Unexpected(String),
    /// The file is larger than the limit of the read.
    TooLarge(String),
    /// Another holder has the root's lock.
    Locked,
    Io(String, std::io::Error),
}

impl fmt::Display for AccessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Path(path) => write!(formatter, "{path}: not a relative path beneath the root"),
            Self::Link(path) => write!(formatter, "{path}: symbolic links are not followed"),
            Self::Unexpected(path) => {
                write!(formatter, "{path}: not a regular file or directory")
            }
            Self::TooLarge(path) => write!(formatter, "{path}: file is too large"),
            Self::Locked => write!(formatter, "another edit holds the source tree's lock"),
            Self::Io(path, error) => write!(formatter, "{path}: {error}"),
        }
    }
}

impl std::error::Error for AccessError {}

fn refused(path: &str, refused: Refused) -> AccessError {
    match refused {
        Refused::Link(prefix) => AccessError::Link(prefix),
        Refused::Missing | Refused::Unexpected => AccessError::Unexpected(path.to_owned()),
        Refused::Io(error) => AccessError::Io(path.to_owned(), error),
    }
}

/// The checked components of a root-relative path; the empty path is the
/// root.
fn checked(path: &str) -> Result<Vec<&str>, AccessError> {
    if path.is_empty() {
        return Ok(Vec::new());
    }
    if path.len() > 4096 || path.split('/').any(|name| checked_name(name).is_err()) {
        return Err(AccessError::Path(path.to_owned()));
    }
    Ok(components(path))
}

/// Whether `name` can name an entry of a directory: not empty, `.` or `..`,
/// and without `/`, `\`, `:` or NUL, so that no platform reads it as more
/// than one component, a drive or a stream.
fn checked_name(name: &str) -> Result<&str, AccessError> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.len() > 255
        || name.contains(['/', '\\', ':', '\0'])
    {
        Err(AccessError::Path(name.to_owned()))
    } else {
        Ok(name)
    }
}

/// A source tree root. The root itself may be a link; nothing beneath it is
/// followed.
pub struct SourceRoot {
    root: platform::Root,
}

impl SourceRoot {
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            root: platform::Root::new(root),
        }
    }

    /// Reads a regular file of at most `limit` bytes. `Ok(None)` means the
    /// file, or a directory on its way, is missing.
    pub fn read(&self, path: &str, limit: u64) -> Result<Option<Vec<u8>>, AccessError> {
        let parts = checked(path)?;
        if parts.is_empty() {
            return Err(AccessError::Unexpected(path.to_owned()));
        }
        let (file, length) = match self.root.file(&parts) {
            Ok(opened) => opened,
            Err(Refused::Missing) => return Ok(None),
            Err(other) => return Err(refused(path, other)),
        };
        if length > limit {
            return Err(AccessError::TooLarge(path.to_owned()));
        }
        // One byte past the limit shows a file that grew after the check.
        let mut bytes = Vec::new();
        file.take(limit.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|error| AccessError::Io(path.to_owned(), error))?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
            return Err(AccessError::TooLarge(path.to_owned()));
        }
        Ok(Some(bytes))
    }

    /// Opens the directory `path` (the empty path is the root), creating it
    /// and its missing parents when `create`. `Ok(None)` means it is missing
    /// and was not created.
    pub fn directory(
        &self,
        path: &str,
        create: bool,
    ) -> Result<Option<SourceDirectory>, AccessError> {
        match self.root.directory(&checked(path)?, create) {
            Ok(directory) => Ok(Some(SourceDirectory {
                directory,
                path: path.to_owned(),
            })),
            Err(Refused::Missing) => Ok(None),
            Err(other) => Err(refused(path, other)),
        }
    }

    /// Takes the root's exclusive advisory lock, held until the returned
    /// value is dropped. It fails at once with [`AccessError::Locked`] if
    /// another holder, in this process or another, has it. Only Unix has
    /// the lock; elsewhere this always succeeds.
    pub fn lock(&self) -> Result<SourceLock, AccessError> {
        match self.root.lock() {
            Ok(Some(lock)) => Ok(SourceLock(lock)),
            Ok(None) => Err(AccessError::Locked),
            Err(other) => Err(refused(".", other)),
        }
    }
}

/// The root's lock; see [`SourceRoot::lock`].
pub struct SourceLock(#[allow(dead_code)] platform::Lock);

/// A directory beneath the root. Every name is a single entry of it.
pub struct SourceDirectory {
    directory: platform::Directory,
    path: String,
}

impl SourceDirectory {
    fn entry(&self, name: &str) -> String {
        crate::files::join(&self.path, name)
    }

    /// Writes a new file `name` with `bytes` and syncs it. Whatever was at
    /// `name` is removed first, so a link or hard link there is never written
    /// through. The file takes the permissions of the regular file `like`,
    /// if there is one.
    pub fn write(&self, name: &str, like: &str, bytes: &[u8]) -> Result<(), AccessError> {
        self.directory
            .write(checked_name(name)?, checked_name(like)?, bytes)
            .map_err(|error| refused(&self.entry(name), error))
    }

    /// Renames `from` to `to`, replacing `to`, atomically.
    pub fn rename(&self, from: &str, to: &str) -> Result<(), AccessError> {
        self.directory
            .rename(checked_name(from)?, checked_name(to)?)
            .map_err(|error| refused(&self.entry(to), error))
    }

    /// Removes the file `name`; `false` when there was none.
    pub fn remove_file(&self, name: &str) -> Result<bool, AccessError> {
        self.directory
            .remove_file(checked_name(name)?)
            .map_err(|error| refused(&self.entry(name), error))
    }

    /// Removes the directory `name` if it is empty; `false` when it is
    /// missing or not empty.
    pub fn remove_directory(&self, name: &str) -> Result<bool, AccessError> {
        self.directory
            .remove_directory(checked_name(name)?)
            .map_err(|error| refused(&self.entry(name), error))
    }

    /// Syncs the directory, making renames and removals in it durable.
    pub fn sync(&self) -> Result<(), AccessError> {
        self.directory
            .sync()
            .map_err(|error| refused(&self.path, error))
    }
}
