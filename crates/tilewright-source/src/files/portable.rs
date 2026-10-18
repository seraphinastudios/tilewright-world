// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Access beneath the root by path, for platforms without directory
//! handles: every component is checked for a symbolic link before use. A
//! link swapped in between the check and the use is not detected.

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
