// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Confined access to the files of a source tree, for tools that edit it.
//!
//! This is the loader's own path confinement: paths are root-relative and
//! `/`-separated, with no empty, `.` or `..` component and no `\`, `:` or
//! NUL in any component, and nothing beneath the root is followed if it is
//! a symbolic link. On Unix and Windows every component is opened through
//! its parent's directory handle without following links (on Windows, any
//! reparse point: symbolic links, junctions and mount points among them),
//! and writes, renames and removals act on names inside a directory opened
//! that way, so a link swapped in at any moment is never followed.
//! Elsewhere components are checked by path before use, a best effort that
//! a local process changing the tree at the same time can race.

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
pub(crate) fn checked(path: &str) -> Result<Vec<&str>, AccessError> {
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

/// Who may use a file or directory, as publishing keeps it.
///
/// On Unix: the permission bits, the owner and the group. Beside them on
/// Linux, the mandatory access control labels (`SELinux`, Smack), which
/// publishing requires to be the same on every entry it compares. On
/// Windows, where entries take the access their directory gives them: the
/// inherited discretionary access control list. Where the mode bits do not
/// say who may read, as with an access control list on Unix, or access
/// control of an entry's own on Windows, reading the access is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Access {
    /// The permission bits, with the set-user, set-group and sticky bits;
    /// 0 on Windows.
    pub(crate) mode: u32,
    /// The owning user; 0 on Windows.
    pub(crate) user: u32,
    /// The owning group; 0 on Windows.
    pub(crate) group: u32,
    /// A digest of the access control beside the mode bits that publishing
    /// requires to be the same throughout: on Linux the labels, on Windows
    /// the access control list. All zero where there is none.
    pub(crate) control: [u8; 32],
}

/// Who may reach content through a file or directory: for a file who may
/// read it, for a directory who may search it. Publishing requires the
/// entries it compares to have one audience each where they are
/// [`comparable`](Audience::comparable).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Audience {
    /// Which audiences are compared: on Unix every entry's with every
    /// other's, since a new file takes its directory's mode; on Windows a
    /// file's with a file's and a directory's with a directory's, since
    /// their lists differ in kind.
    kind: u8,
    /// The classes that may reach, as read bits.
    reach: u32,
    /// The group, where only it may reach.
    group: Option<u32>,
    control: [u8; 32],
}

impl Audience {
    /// Whether the two can be compared.
    #[must_use]
    pub const fn comparable(&self, other: &Self) -> bool {
        self.kind == other.kind
    }
}

impl Access {
    /// Who may reach content through an entry with this access, a
    /// directory if `directory`.
    #[must_use]
    pub fn audience(&self, directory: bool) -> Audience {
        if cfg!(windows) {
            return Audience {
                kind: 1 + u8::from(directory),
                reach: 0,
                group: None,
                control: self.control,
            };
        }
        let reach = if directory {
            (self.mode & 0o111) << 2
        } else {
            self.mode & 0o444
        };
        Audience {
            kind: 0,
            reach,
            group: (reach & 0o040 != 0 && reach & 0o004 == 0).then_some(self.group),
            control: self.control,
        }
    }

    /// The access a new file in a directory with this access is given: on
    /// Unix the directory's mode without the execute bits, its group and
    /// its labels. `None` on Windows, where a new file takes the access
    /// control list its directory gives it.
    #[must_use]
    pub fn new_file(&self) -> Option<Self> {
        (!cfg!(windows)).then_some(Self {
            mode: self.mode & 0o666,
            ..*self
        })
    }
}

/// A source tree root. The root itself may be a link; nothing beneath it is
/// followed.
pub struct SourceRoot {
    root: platform::Root,
}

impl SourceRoot {
    /// Opens the root. On Unix and Windows the directory is opened here,
    /// and every later access goes through that handle, so moving or
    /// replacing what the path names afterwards does not move the root;
    /// elsewhere the path is resolved to an absolute one here.
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            root: platform::Root::new(root),
        }
    }

    /// A second handle on the same root directory.
    #[must_use]
    pub fn try_clone(&self) -> Self {
        Self {
            root: self.root.try_clone(),
        }
    }

    pub(crate) fn platform(&self) -> platform::Root {
        self.root.try_clone()
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

    /// Makes the directory `path` in its existing parent. Returns whether
    /// this call made it: `false` when something by that name was already
    /// there, which the caller then did not create.
    pub fn make_directory(&self, path: &str) -> Result<bool, AccessError> {
        self.root
            .make_directory(&checked(path)?)
            .map_err(|error| refused(path, error))
    }

    /// Takes the root's exclusive lock, held until the returned value is
    /// dropped. It fails at once with [`AccessError::Locked`] if another
    /// holder, in this process or another, has it. On Unix it is an
    /// advisory `flock` of the root directory; on Windows a handle on the
    /// root directory that asks for write access and shares it with no
    /// one, so it also fails while another program has the root directory
    /// open for writing. Elsewhere there is no lock and this always
    /// succeeds.
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
    /// `name` is removed first, so a link or hard link there is never
    /// written through. With `access`, the new file must have the access
    /// control beside the mode bits that `access` has, its labels on Linux
    /// or its access control list on Windows, which is checked before
    /// anything is written; on Unix it is its owner's alone until written,
    /// then takes that mode and group, without an access control list its
    /// directory would give it on Linux. Without `access`, it has the
    /// platform's default. A new file with access control that reading its
    /// access would refuse is refused before anything is written, except
    /// without `access` on Unix.
    pub fn write(
        &self,
        name: &str,
        access: Option<&Access>,
        bytes: &[u8],
    ) -> Result<(), AccessError> {
        self.directory
            .write(checked_name(name)?, access, bytes)
            .map_err(|error| refused(&self.entry(name), error))
    }

    /// Who may use the regular file `name`; `None` when there is no regular
    /// file there. A file whose access publishing could not keep is
    /// refused: on Linux and macOS one with an access control list; on
    /// Windows one with access control of its own (an access control entry
    /// that is not inherited, or a list protected from inheritance), a
    /// mandatory integrity label or central access policy, encryption, or
    /// the read-only attribute.
    pub fn access(&self, name: &str) -> Result<Option<Access>, AccessError> {
        self.directory
            .access(checked_name(name)?)
            .map_err(|error| refused(&self.entry(name), error))
    }

    /// Who may use this directory. A directory is refused as a file is by
    /// [`access`](Self::access), except that it may be read-only; on Linux a
    /// default access control list it gives new files is refused too.
    pub fn own_access(&self) -> Result<Access, AccessError> {
        self.directory
            .own_access()
            .map_err(|error| refused(&self.path, error))
    }

    /// Gives this directory the mode and group of `access`, and checks that
    /// it has the labels of `access`. On Windows, where a directory has the
    /// access control list it inherits, checks that the list is that of
    /// `access`.
    pub fn set_own_access(&self, access: &Access) -> Result<(), AccessError> {
        self.directory
            .set_own_access(access)
            .map_err(|error| refused(&self.path, error))
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
    /// Returns `false` on platforms that cannot sync a directory, where
    /// they may not be durable yet.
    pub fn sync(&self) -> Result<bool, AccessError> {
        self.directory
            .sync()
            .map_err(|error| refused(&self.path, error))
    }
}

impl fmt::Debug for SourceRoot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SourceRoot")
    }
}
