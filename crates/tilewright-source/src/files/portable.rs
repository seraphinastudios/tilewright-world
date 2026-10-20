// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Access beneath the root by path, for platforms without directory
//! handles: every component is checked for a symbolic link before use. A
//! link swapped in between the check and the use is not detected. There is
//! no lock.

use super::{Found, RawEntry, RawKind, Refused};
use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

pub(crate) struct Root(PathBuf);

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
    pub(crate) fn new(path: &Path) -> Self {
        Self(path.to_path_buf())
    }

    /// The path of `components` and its metadata, after checking that no
    /// component is a link.
    fn checked(&self, components: &[&str]) -> Result<(PathBuf, fs::Metadata), Refused> {
        let mut current = self.0.clone();
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
        let mut current = self.0.clone();
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

    pub(crate) fn write(&self, name: &str, like: &str, bytes: &[u8]) -> Result<(), Refused> {
        use std::io::Write as _;
        let permissions = self
            .entry(like)?
            .filter(fs::Metadata::is_file)
            .map(|metadata| metadata.permissions());
        self.remove_file(name)?;
        let path = self.0.join(name);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(Refused::Io)?;
        let written = (|| {
            if let Some(permissions) = permissions {
                file.set_permissions(permissions).map_err(Refused::Io)?;
            }
            file.write_all(bytes).map_err(Refused::Io)?;
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

    pub(crate) fn sync(&self) -> Result<(), Refused> {
        #[cfg(unix)]
        fs::File::open(&self.0)
            .and_then(|directory| directory.sync_all())
            .map_err(Refused::Io)?;
        Ok(())
    }
}
