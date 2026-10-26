// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Unix access beneath the root through directory handles. Each component
//! is opened relative to the directory before it with `O_NOFOLLOW`, so a
//! symbolic link is never followed, not even one swapped in after a check.
//! Writes, renames and removals act on names inside a directory opened that
//! way, so they never follow a link either.

use super::{Found, RawEntry, RawKind, Refused};
use crate::access::Access;
use rustix::{
    fs::{
        AtFlags, CWD, Dir, FileType, FlockOperation, Gid, Mode, OFlags, Stat, fchmod, fchown,
        flock, fstat, fsync, mkdirat, openat, renameat, statat, unlinkat,
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
    /// Who may use the regular file `name`, if there is one. A file whose
    /// access publishing could not keep is refused: on Linux one with an
    /// access control list, since its mode bits do not say who may read it
    /// and a file replacing it could not keep the list. Other systems' lists
    /// are not read.
    pub(crate) fn access(&self, name: &str) -> Result<Option<Access>, Refused> {
        match statat(&self.0, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(stat) if kind_of(FileType::from_raw_mode(stat.st_mode)) == RawKind::File => {
                self.file_access(name, &stat)
            }
            Ok(_) | Err(Errno::NOENT) => Ok(None),
            Err(errno) => Err(Refused::Io(errno.into())),
        }
    }

    /// The access of the regular file `name`, opened to check what its
    /// status does not show.
    #[cfg(target_os = "linux")]
    fn file_access(&self, name: &str, _: &Stat) -> Result<Option<Access>, Refused> {
        let flags =
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC | OFlags::NOCTTY;
        let fd = openat(&self.0, name, flags, Mode::empty())
            .map_err(|errno| classify(self.0.as_fd(), name, errno, &[name]))?;
        // What is read is what was opened.
        let stat = fstat(&fd).map_err(|errno| Refused::Io(errno.into()))?;
        if kind_of(FileType::from_raw_mode(stat.st_mode)) != RawKind::File {
            return Ok(None);
        }
        refuse_access_control_lists(&fd, false)?;
        Ok(Some(Access {
            control: labels(&fd)?,
            ..access_of(&stat)
        }))
    }

    #[cfg(not(target_os = "linux"))]
    #[allow(clippy::unnecessary_wraps, clippy::unused_self)]
    fn file_access(&self, _: &str, stat: &Stat) -> Result<Option<Access>, Refused> {
        Ok(Some(access_of(stat)))
    }

    /// Who may use this directory. On Linux, a directory with an access
    /// control list, its own or one it gives new files, is refused.
    pub(crate) fn own_access(&self) -> Result<Access, Refused> {
        let stat = fstat(&self.0).map_err(|errno| Refused::Io(errno.into()))?;
        #[cfg(target_os = "linux")]
        refuse_access_control_lists(&self.0, true)?;
        Ok(Access {
            control: labels(&self.0)?,
            ..access_of(&stat)
        })
    }

    /// Gives this directory the mode and group of `access`, and checks
    /// that the system gave it the labels of `access`.
    pub(crate) fn set_own_access(&self, access: &Access) -> Result<(), Refused> {
        check_labels(&self.0, access)?;
        set_access(&self.0, access)
    }

    /// Writes a new file `name` holding `bytes` and syncs it. Whatever was
    /// at `name` is removed first, so a link or a second hard link there is
    /// never written through. With `access`, the file must have the labels
    /// of `access`, and on Linux loses an access control list its directory
    /// gives it, both before anything is written; it is its owner's alone
    /// until written, then gets that mode and group. Without, the
    /// platform's default.
    pub(crate) fn write(
        &self,
        name: &str,
        access: Option<&Access>,
        bytes: &[u8],
    ) -> Result<(), Refused> {
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
        let mode = if access.is_some() { 0o600 } else { 0o666 };
        let fd = openat(&self.0, name, flags, Mode::from_raw_mode(mode))
            .map_err(|errno| classify(self.0.as_fd(), name, errno, &[name]))?;
        let written = (|| {
            let mut file = std::fs::File::from(fd);
            if let Some(access) = access {
                // An access control list the directory gives new files
                // would let others read what the mode bits do not, and a
                // label other than the one asked for would be another
                // audience; both are dealt with before a byte is written.
                #[cfg(target_os = "linux")]
                strip_access_control_list(&file)?;
                check_labels(&file, access)?;
            }
            file.write_all(bytes).map_err(Refused::Io)?;
            if let Some(access) = access {
                set_access(&file, access)?;
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

    /// Checks, before anything is made, that the `SELinux` policy gives a
    /// file made here as `name` the label it gives one made as `staged`. A
    /// rule of the policy that keys a new file's label on its name applies
    /// when a file is made, not when it is renamed, so a file staged under
    /// another name and renamed to `name` would keep a label the policy
    /// does not give `name`. The staged file's own label is checked when it
    /// is written. Where `SELinux` labels nothing here, there is nothing to
    /// check; where it labels the directory but cannot be asked, the check
    /// fails.
    #[cfg(target_os = "linux")]
    pub(crate) fn check_final_name(&self, staged: &str, name: &str) -> Result<(), Refused> {
        let Some(directory) = label(&self.0, "security.selinux")? else {
            return Ok(());
        };
        // A thread that sets the label of what it makes (`setfscreatecon`)
        // gives both names that label; the staged file's is checked when
        // it is written.
        if selinux::creates_as_set().map_err(Refused::Io)? {
            return Ok(());
        }
        // A labelled tree whose policy cannot be asked, as in a container
        // that hides the SELinux file system, is refused, not guessed at.
        let given = selinux::compute_create(&directory, staged).map_err(Refused::Io)?;
        let expected = selinux::compute_create(&directory, name).map_err(Refused::Io)?;
        if given == expected {
            Ok(())
        } else {
            Err(unkept(
                "the system's policy labels a new file of its name otherwise than the file staged for it, and renaming does not relabel",
            ))
        }
    }

    #[cfg(not(target_os = "linux"))]
    #[allow(clippy::unnecessary_wraps, clippy::unused_self)]
    pub(crate) fn check_final_name(&self, _: &str, _: &str) -> Result<(), Refused> {
        Ok(())
    }

    /// The file system this directory is on, where Linux does not show its
    /// access control lists; see [`unchecked_file_system`].
    #[cfg(target_os = "linux")]
    pub(crate) fn unchecked_file_system(&self) -> Result<Option<&'static str>, Refused> {
        #[cfg(test)]
        if let Some(magic) = GIVEN_FILE_SYSTEM.with(std::cell::Cell::get) {
            return Ok(unchecked_file_system(magic));
        }
        let found = rustix::fs::fstatfs(&self.0).map_err(|errno| Refused::Io(errno.into()))?;
        // The magic numbers are 32 bits wide, whatever the width of the
        // field.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let magic = found.f_type as u32;
        Ok(unchecked_file_system(magic))
    }

    /// Elsewhere access control lists are not read at all.
    #[cfg(not(target_os = "linux"))]
    #[allow(clippy::unnecessary_wraps, clippy::unused_self)]
    pub(crate) fn unchecked_file_system(&self) -> Result<Option<&'static str>, Refused> {
        Ok(None)
    }

    /// Syncs the directory, so renames and removals in it are durable. On
    /// macOS `fsync` does not flush the drive's cache, so the directory is
    /// synced with `F_FULLFSYNC`, as std syncs files, or with `fsync` where
    /// the file system has no such sync.
    pub(crate) fn sync(&self) -> Result<bool, Refused> {
        #[cfg(target_os = "macos")]
        match rustix::fs::fcntl_fullfsync(&self.0) {
            Ok(()) => return Ok(true),
            Err(Errno::NOTSUP | Errno::OPNOTSUPP | Errno::INVAL | Errno::NOTTY) => {}
            Err(errno) => return Err(Refused::Io(errno.into())),
        }
        fsync(&self.0)
            .map(|()| true)
            .map_err(|errno| Refused::Io(errno.into()))
    }
}

/// Who may use a file or directory, from its status, without labels.
fn access_of(stat: &Stat) -> Access {
    Access {
        mode: mode_bits(stat.st_mode),
        user: stat.st_uid,
        group: stat.st_gid,
        control: [0; 32],
    }
}

/// The permission bits of a mode, which is a `u16` on Apple platforms.
#[allow(clippy::useless_conversion)]
fn mode_bits(mode: rustix::fs::RawMode) -> u32 {
    u32::from(mode) & 0o7777
}

/// Gives an open file or directory the mode and group of `access`: the
/// group first, since changing it may clear the set-group bit. A group the
/// user may not give is an error, since the group bits would otherwise
/// apply to another group.
fn set_access(fd: impl AsFd, access: &Access) -> Result<(), Refused> {
    let stat = fstat(&fd).map_err(|errno| Refused::Io(errno.into()))?;
    if stat.st_gid != access.group {
        fchown(&fd, None, Some(Gid::from_raw(access.group)))
            .map_err(|errno| Refused::Io(errno.into()))?;
    }
    let mode = rustix::fs::RawMode::try_from(access.mode & 0o7777)
        .map_err(|_| Refused::Io(std::io::ErrorKind::InvalidInput.into()))?;
    fchmod(&fd, Mode::from_raw_mode(mode)).map_err(|errno| Refused::Io(errno.into()))?;
    // The system may drop a bit without failing, such as the set-group bit
    // for a user outside the group, so the result is checked.
    let stat = fstat(&fd).map_err(|errno| Refused::Io(errno.into()))?;
    if mode_bits(stat.st_mode) != access.mode || stat.st_gid != access.group {
        return Err(Refused::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "could not be given mode {:o} and group {}: it has mode {:o} and group {}",
                access.mode,
                access.group,
                mode_bits(stat.st_mode),
                stat.st_gid
            ),
        )));
    }
    Ok(())
}

/// The access control lists Linux file systems keep beside the mode bits:
/// POSIX, NFS version 4 (also as ZFS shows them) and rich ones.
#[cfg(target_os = "linux")]
const ACCESS_CONTROL_LISTS: [&str; 4] = [
    "system.posix_acl_access",
    "system.nfs4_acl",
    "system.nfs4_acl_xdr",
    "system.richacl",
];

/// File systems whose access control lists Linux does not show as the
/// attributes above, by the magic number `statfs` gives: who may read a
/// file there is not what its mode bits and those attributes say.
#[cfg(target_os = "linux")]
const UNCHECKED_FILE_SYSTEMS: [(u32, &str); 7] = [
    (0xFF53_4D42, "SMB"),
    (0xFE53_4D42, "SMB"),
    (0x0000_517B, "SMB"),
    (0x0102_1997, "9p"),
    (0x6573_5546, "FUSE"),
    (0x5346_544E, "NTFS"),
    (0x7366_746E, "NTFS"),
];

/// The name of a file system whose access control lists Linux does not
/// show, by its `statfs` magic number; `None` for any other.
#[cfg(target_os = "linux")]
pub(crate) fn unchecked_file_system(magic: u32) -> Option<&'static str> {
    UNCHECKED_FILE_SYSTEMS
        .iter()
        .find(|(known, _)| *known == magic)
        .map(|(_, name)| *name)
}

/// The mandatory access control labels Linux security modules keep, which
/// say who may read a file beside its mode bits.
#[cfg(target_os = "linux")]
const LABELS: [&str; 2] = ["security.selinux", "security.SMACK64"];

/// Labels that publishing refuses, since it could not keep them.
#[cfg(target_os = "linux")]
const UNKEPT_LABELS: [&str; 3] = [
    "security.SMACK64EXEC",
    "security.SMACK64MMAP",
    "security.SMACK64TRANSMUTE",
];

/// An error for access control that publishing could not keep.
fn unkept(what: &str) -> Refused {
    Refused::Io(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!("{what}, which publishing could not keep"),
    ))
}

/// Refuses an open file or directory that has a POSIX, NFS version 4 or
/// rich access control list, and for a `directory` also a default list it
/// gives new files.
#[cfg(target_os = "linux")]
fn refuse_access_control_lists(fd: impl AsFd, directory: bool) -> Result<(), Refused> {
    let found = has_any(&fd, &ACCESS_CONTROL_LISTS)?
        || (directory && has_any(&fd, &["system.posix_acl_default"])?);
    if found {
        return Err(unkept("it has an access control list"));
    }
    Ok(())
}

/// Whether the open file has any of the extended attributes `names`.
#[cfg(target_os = "linux")]
fn has_any(fd: impl AsFd, names: &[&str]) -> Result<bool, Refused> {
    for name in names {
        // Asked with no room, the call gives the size of a value that is
        // there, or says there is none.
        match rustix::fs::fgetxattr(&fd, *name, &mut [0_u8; 0][..]) {
            Ok(_) | Err(Errno::RANGE) => return Ok(true),
            Err(Errno::NODATA | Errno::NOTSUP) => {}
            Err(errno) => return Err(Refused::Io(errno.into())),
        }
    }
    Ok(false)
}

/// The largest label read; real ones are far shorter.
#[cfg(target_os = "linux")]
const LABEL_LIMIT: usize = 4096;

/// A digest of the open file's or directory's mandatory access control
/// labels, all zero when it has none. Elsewhere than on Linux there are
/// none.
#[cfg(target_os = "linux")]
fn labels(fd: impl AsFd) -> Result<[u8; 32], Refused> {
    use sha2::{Digest as _, Sha256};
    // Smack's other labels say how a file is executed or mapped, or how a
    // directory labels what is made in it; a replacement, or an entry made
    // again, would not get them.
    if has_any(&fd, &UNKEPT_LABELS)? {
        return Err(unkept(
            "it has a Smack execution, mapping or transmuting label",
        ));
    }
    let mut values = Vec::with_capacity(LABELS.len());
    for name in LABELS {
        values.push((name, label(&fd, name)?));
    }
    #[cfg(test)]
    if let Some(given) = given_label(&fd)? {
        values[0].1 = Some(given);
    }
    if values.iter().all(|(_, value)| value.is_none()) {
        return Ok([0; 32]);
    }
    let mut hasher = Sha256::new();
    for (name, value) in values {
        hasher.update(name.as_bytes());
        match value {
            None => hasher.update([0]),
            Some(value) => {
                hasher.update([1]);
                hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_le_bytes());
                hasher.update(&value);
            }
        }
    }
    Ok(hasher.finalize().into())
}

/// The label `name` of an open file or directory, if it has one.
#[cfg(target_os = "linux")]
fn label(fd: impl AsFd, name: &str) -> Result<Option<Vec<u8>>, Refused> {
    let size = match rustix::fs::fgetxattr(&fd, name, &mut [0_u8; 0][..]) {
        Ok(size) => size,
        Err(Errno::NODATA | Errno::NOTSUP) => return Ok(None),
        Err(errno) => return Err(Refused::Io(errno.into())),
    };
    if size > LABEL_LIMIT {
        return Err(unkept("it has a label longer than publishing reads"));
    }
    let mut value = vec![0_u8; size];
    // A label that grew since its size was read is an error too.
    let read = rustix::fs::fgetxattr(&fd, name, &mut value[..])
        .map_err(|errno| Refused::Io(errno.into()))?;
    value.truncate(read);
    Ok(Some(value))
}

#[cfg(all(test, target_os = "linux"))]
thread_local! {
    /// `SELinux` labels by device and inode, for tests: setting a
    /// `security.*` attribute takes privileges a test does not have.
    pub(crate) static GIVEN_LABELS: std::cell::RefCell<std::collections::BTreeMap<(u64, u64), Vec<u8>>> =
        const { std::cell::RefCell::new(std::collections::BTreeMap::new()) };
}

#[cfg(all(test, target_os = "linux"))]
thread_local! {
    /// The `statfs` magic number every directory reports, for tests: the
    /// file systems it matters for cannot be mounted in one.
    pub(crate) static GIVEN_FILE_SYSTEM: std::cell::Cell<Option<u32>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(all(test, target_os = "linux"))]
fn given_label(fd: impl AsFd) -> Result<Option<Vec<u8>>, Refused> {
    let stat = fstat(&fd).map_err(|errno| Refused::Io(errno.into()))?;
    Ok(GIVEN_LABELS.with(|labels| labels.borrow().get(&(stat.st_dev, stat.st_ino)).cloned()))
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::unnecessary_wraps)]
fn labels(_fd: impl AsFd) -> Result<[u8; 32], Refused> {
    Ok([0; 32])
}

/// Checks that the open file or directory has the labels of `access`:
/// publishing never relabels, so an entry its directory, or the system's
/// policy, labels otherwise is refused.
fn check_labels(fd: impl AsFd, access: &Access) -> Result<(), Refused> {
    if labels(fd)? == access.control {
        Ok(())
    } else {
        Err(unkept(
            "the system gave it other mandatory access control labels (SELinux, Smack) than the entries publishing compared it with",
        ))
    }
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
    if has_any(&fd, &ACCESS_CONTROL_LISTS)? {
        return Err(Refused::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "the directory gives new files an access control list that cannot be removed",
        )));
    }
    Ok(())
}

/// `SELinux`'s own answer to which label a new file gets.
#[cfg(target_os = "linux")]
pub(crate) mod selinux {
    use std::{
        fs,
        io::{self, Read as _, Write as _},
    };

    /// Where the kernel's `SELinux` file system is mounted.
    const FILE_SYSTEM: &str = "/sys/fs/selinux";

    /// The largest label read back.
    const LIMIT: u64 = 4096;

    /// A label without the NUL or newline that ends it.
    pub(crate) fn trimmed(label: &[u8]) -> &[u8] {
        let end = label
            .iter()
            .rposition(|byte| !matches!(byte, b'\0' | b'\n'))
            .map_or(0, |last| last + 1);
        &label[..end]
    }

    /// Whether this thread sets the label of the files it makes itself
    /// (`setfscreatecon`), so that no rule of the policy applies to them.
    pub(super) fn creates_as_set() -> io::Result<bool> {
        match fs::read("/proc/thread-self/attr/fscreate") {
            Ok(label) => Ok(!trimmed(&label).is_empty()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// The label the policy gives a regular file this thread makes as
    /// `name` in a directory labelled `directory`. An error where the
    /// policy cannot be asked, such as where the `SELinux` file system is
    /// not mounted.
    pub(super) fn compute_create(directory: &[u8], name: &str) -> io::Result<Vec<u8>> {
        let class = fs::read_to_string(format!("{FILE_SYSTEM}/class/file/index"))
            .map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("the tree has SELinux labels, but {FILE_SYSTEM} cannot be asked which label a new file gets ({error})"),
                )
            })?
            .trim()
            .parse::<u16>()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let thread = fs::read("/proc/thread-self/attr/current")?;
        let query = request(&thread, directory, class, name)?;
        let mut answer = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(format!("{FILE_SYSTEM}/create"))?;
        // The kernel takes the question in one write.
        if answer.write(&query)? != query.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "the label question was not taken whole",
            ));
        }
        let mut label = Vec::new();
        answer.take(LIMIT).read_to_end(&mut label)?;
        Ok(trimmed(&label).to_vec())
    }

    /// The question `SELinux`'s `create` file answers: the labels of who
    /// makes the file and of where, the class of file, and its name,
    /// percent-encoded so that no byte of it ends the question.
    pub(crate) fn request(
        source: &[u8],
        target: &[u8],
        class: u16,
        name: &str,
    ) -> io::Result<Vec<u8>> {
        let (source, target) = (trimmed(source), trimmed(target));
        let plain = |label: &[u8]| !label.is_empty() && label.iter().all(u8::is_ascii_graphic);
        if !plain(source) || !plain(target) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a label is not plain text",
            ));
        }
        let mut query = Vec::with_capacity(source.len() + target.len() + name.len() * 3 + 16);
        query.extend_from_slice(source);
        query.push(b' ');
        query.extend_from_slice(target);
        query.extend_from_slice(format!(" {class} ").as_bytes());
        for byte in name.bytes() {
            if byte.is_ascii_alphanumeric() || b"-_.".contains(&byte) {
                query.push(byte);
            } else {
                query.extend_from_slice(format!("%{byte:02x}").as_bytes());
            }
        }
        Ok(query)
    }
}
