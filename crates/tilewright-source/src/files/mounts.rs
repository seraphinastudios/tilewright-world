// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! The mounts of this process on Linux, from `/proc/self/mountinfo`: the
//! file system type of each, by mount ID. The type is the kernel's name for
//! the driver (`ext4`, `9p`, `overlay`, `fuse.sshfs`), which, unlike the
//! magic number `statfs` gives, a file system cannot borrow from what it
//! shares.
//!
//! The file is read a line at a time, each line on its own: a line that is
//! too long or malformed leaves only its own mount unknown, so that a mount
//! another user makes, such as a FUSE one with a type of their choosing,
//! cannot leave every other mount unknown.

use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read as _},
};

/// Longest line read: a mount point and a root are each at most 4,096
/// bytes, escaped at most four times as long.
const LINE: usize = 64 << 10;

/// Longest file system type kept, unescaped; the kernel's own are short.
const TYPE: usize = 255;

/// The most bytes of a descriptor's `/proc/self/fdinfo` entry read.
const FDINFO: u64 = 64 << 10;

/// What a mount's line says of its type: the type, as bytes, or why it
/// could not be read.
type Found = Result<Vec<u8>, String>;

/// The file system type of each mount, by mount ID, or why the mounts
/// could not be read at all.
pub(crate) struct Mounts(Result<BTreeMap<u64, Found>, String>);

impl Mounts {
    pub(crate) fn read() -> Self {
        let cannot =
            |error: std::io::Error| format!("/proc/self/mountinfo could not be read ({error})");
        Self(
            std::fs::File::open("/proc/self/mountinfo")
                .map_err(cannot)
                .and_then(|file| scan(BufReader::new(file)).map_err(cannot)),
        )
    }

    /// The mounts `bytes` lists, as `/proc/self/mountinfo` would, for tests.
    #[cfg(test)]
    pub(crate) fn parsed(bytes: &[u8]) -> Self {
        Self(Ok(scan(bytes).unwrap()))
    }

    /// Mounts that could not be read, for tests.
    #[cfg(test)]
    pub(crate) fn failed(reason: &str) -> Self {
        Self(Err(reason.to_owned()))
    }

    /// The file system type of the mount `id`, or why it is not known.
    pub(crate) fn type_of(&self, id: u64) -> Result<&[u8], String> {
        let mounts = self.0.as_ref().map_err(Clone::clone)?;
        match mounts.get(&id) {
            Some(Ok(found)) => Ok(found),
            Some(Err(reason)) => Err(reason.clone()),
            None => Err(format!("its mount, {id}, is not in /proc/self/mountinfo")),
        }
    }
}

/// Reads `/proc/self/mountinfo` a line at a time. Each line holds the mount
/// ID, the parent's ID, the device, the root, the mount point and the
/// options, then optional fields up to a lone `-`, then the file system
/// type. Paths need not be UTF-8, so lines are read as bytes. A line whose
/// ID cannot be read is left out, so its mount is not found; one too long
/// or otherwise malformed, or a second line for one ID, makes that ID's
/// type unknown. Only an error reading fails the whole.
pub(crate) fn scan(mut reader: impl BufRead) -> std::io::Result<BTreeMap<u64, Found>> {
    let mut mounts = BTreeMap::new();
    let mut line = Vec::new();
    while let Some(long) = read_line(&mut reader, &mut line)? {
        if let Some((id, found)) = entry(&line, long)
            && mounts.insert(id, found).is_some()
        {
            mounts.insert(
                id,
                Err(format!(
                    "its mount, {id}, is listed twice in /proc/self/mountinfo"
                )),
            );
        }
    }
    Ok(mounts)
}

/// Reads one line into `line`, without its newline, keeping at most
/// [`LINE`] bytes of it. Returns whether it was longer, or `None` at the
/// end.
fn read_line(reader: &mut impl BufRead, line: &mut Vec<u8>) -> std::io::Result<Option<bool>> {
    line.clear();
    let mut long = false;
    let mut any = false;
    loop {
        let buffer = reader.fill_buf()?;
        if buffer.is_empty() {
            return Ok(any.then_some(long));
        }
        any = true;
        let (taken, ended) = match buffer.iter().position(|byte| *byte == b'\n') {
            Some(end) => (end, true),
            None => (buffer.len(), false),
        };
        let room = LINE.saturating_sub(line.len());
        long |= taken > room;
        line.extend_from_slice(&buffer[..taken.min(room)]);
        reader.consume(taken + usize::from(ended));
        if ended {
            return Ok(Some(long));
        }
    }
}

/// The mount ID of a line and what it says of the mount's type; `None`
/// where the line has no ID that can be read.
fn entry(line: &[u8], long: bool) -> Option<(u64, Found)> {
    let digits = line.split(|byte| *byte == b' ').next()?;
    if digits.is_empty() || digits.len() > 20 || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let id = std::str::from_utf8(digits).ok()?.parse::<u64>().ok()?;
    if long {
        return Some((
            id,
            Err(format!(
                "its line in /proc/self/mountinfo is longer than {LINE} bytes"
            )),
        ));
    }
    let fields: Vec<&[u8]> = line.split(|byte| *byte == b' ').collect();
    let found = fields
        .iter()
        .skip(6)
        .position(|field| *field == b"-")
        .and_then(|position| fields.get(position + 7))
        .and_then(|field| unescape(field));
    Some((
        id,
        found.ok_or_else(|| "its line in /proc/self/mountinfo is malformed".to_owned()),
    ))
}

/// A field with the kernel's octal escapes (`\040` for a space) undone; a
/// field that is empty, wrongly escaped or longer than [`TYPE`] bytes is
/// `None`.
fn unescape(field: &[u8]) -> Option<Vec<u8>> {
    if field.is_empty() || field.len() > TYPE * 4 {
        return None;
    }
    let mut bytes = Vec::with_capacity(field.len());
    let mut rest = field;
    while let Some((&byte, after)) = rest.split_first() {
        if byte == b'\\' {
            let digits = after.get(..3)?;
            if !digits.iter().all(|digit| (b'0'..=b'7').contains(digit)) {
                return None;
            }
            let value = digits
                .iter()
                .fold(0_u32, |value, digit| value * 8 + u32::from(digit - b'0'));
            bytes.push(u8::try_from(value).ok()?);
            rest = &after[3..];
        } else {
            bytes.push(byte);
            rest = after;
        }
    }
    (bytes.len() <= TYPE).then_some(bytes)
}

/// The mount ID of the open descriptor `fd`, from its `mnt_id:` line in
/// `/proc/self/fdinfo`, which Linux has given since 3.15, before `statx`
/// did (5.8); `None` where it cannot be read.
pub(crate) fn fdinfo_mount_id(fd: std::os::fd::RawFd) -> Option<u64> {
    let mut bytes = Vec::new();
    std::fs::File::open(format!("/proc/self/fdinfo/{fd}"))
        .and_then(|file| file.take(FDINFO).read_to_end(&mut bytes))
        .ok()?;
    bytes.split(|byte| *byte == b'\n').find_map(|line| {
        let value = line.strip_prefix(b"mnt_id:")?;
        std::str::from_utf8(value).ok()?.trim().parse::<u64>().ok()
    })
}
