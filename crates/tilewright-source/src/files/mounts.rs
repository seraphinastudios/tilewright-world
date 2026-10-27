// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! The mounts of this process on Linux, from `/proc/self/mountinfo`: the
//! file system type of each, by mount ID. The type is the kernel's name for
//! the driver (`ext4`, `9p`, `overlay`, `fuse.sshfs`), which, unlike the
//! magic number `statfs` gives, a file system cannot borrow from what it
//! shares.

use std::{collections::BTreeMap, io::Read as _};

/// Most bytes read from `/proc/self/mountinfo`: far more than any system's
/// mounts take.
const LIMIT: u64 = 16 << 20;

/// Longest line read: a mount point and a root are each at most 4,096
/// bytes, escaped at most four times as long.
const LINE: usize = 64 << 10;

/// Longest file system type read, escaped.
const TYPE: usize = 1024;

/// The file system type of each mount, by mount ID, or why they could not
/// be read.
pub(crate) struct Mounts(Result<BTreeMap<u64, String>, String>);

impl Mounts {
    pub(crate) fn read() -> Self {
        Self(read_file().and_then(|bytes| parse(&bytes)))
    }

    /// The file system type of the mount `id`, or why it is not known.
    pub(crate) fn type_of(&self, id: u64) -> Result<&str, String> {
        let mounts = self.0.as_ref().map_err(Clone::clone)?;
        mounts
            .get(&id)
            .map(String::as_str)
            .ok_or_else(|| format!("its mount, {id}, is not in /proc/self/mountinfo"))
    }
}

fn read_file() -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    std::fs::File::open("/proc/self/mountinfo")
        .and_then(|file| file.take(LIMIT + 1).read_to_end(&mut bytes))
        .map_err(|error| format!("/proc/self/mountinfo could not be read ({error})"))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > LIMIT {
        return Err(format!("/proc/self/mountinfo is longer than {LIMIT} bytes"));
    }
    Ok(bytes)
}

/// Parses `/proc/self/mountinfo`. Each line holds the mount ID, the parent's
/// ID, the device, the root, the mount point and the options, then optional
/// fields up to a lone `-`, then the file system type. Paths need not be
/// UTF-8, so lines are read as bytes; a malformed line fails the whole.
pub(crate) fn parse(bytes: &[u8]) -> Result<BTreeMap<u64, String>, String> {
    let malformed = |line: usize| format!("line {line} of /proc/self/mountinfo is malformed");
    let mut mounts = BTreeMap::new();
    for (index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        if line.len() > LINE {
            return Err(format!(
                "line {} of /proc/self/mountinfo is longer than {LINE} bytes",
                index + 1
            ));
        }
        let fields: Vec<&[u8]> = line.split(|byte| *byte == b' ').collect();
        let separator = fields
            .iter()
            .skip(6)
            .position(|field| *field == b"-")
            .map(|position| position + 6);
        let id = std::str::from_utf8(fields[0])
            .ok()
            .filter(|id| id.bytes().all(|byte| byte.is_ascii_digit()))
            .and_then(|id| id.parse::<u64>().ok());
        let found = separator
            .and_then(|separator| fields.get(separator + 1))
            .and_then(|field| unescape(field));
        match (id, found) {
            (Some(id), Some(found)) => {
                if mounts.insert(id, found).is_some() {
                    return Err(malformed(index + 1));
                }
            }
            _ => return Err(malformed(index + 1)),
        }
    }
    Ok(mounts)
}

/// A field with the kernel's octal escapes (`\040` for a space) undone; a
/// field that is empty, too long, wrongly escaped or not UTF-8 is `None`.
fn unescape(field: &[u8]) -> Option<String> {
    if field.is_empty() || field.len() > TYPE {
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
    String::from_utf8(bytes).ok()
}
