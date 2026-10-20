// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Windows: entries reached relative to a directory handle. `open_beneath`
//! opens one entry of a directory with `NtCreateFile` and the directory's
//! handle as its root, so no path is resolved again and a reparse point at
//! the name (a symbolic link, junction or mount point) is opened itself,
//! never followed. Renames name their target relative to a directory
//! handle in the same way, through `NtSetInformationFile`, which resolves
//! the name relative to that handle. Everything else acts on an open handle.

use std::{
    io,
    mem::{offset_of, size_of},
    os::windows::io::{AsRawHandle as _, BorrowedHandle, FromRawHandle as _, OwnedHandle},
    ptr,
};
use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            FILE_OPEN_REPARSE_POINT, FILE_RENAME_INFORMATION, FILE_RENAME_INFORMATION_0,
            FILE_RENAME_POSIX_SEMANTICS, FILE_RENAME_REPLACE_IF_EXISTS,
            FILE_SYNCHRONOUS_IO_NONALERT, FileRenameInformation, FileRenameInformationEx,
            NtCreateFile, NtSetInformationFile,
        },
    },
    Win32::{
        Foundation::{
            ERROR_INSUFFICIENT_BUFFER, ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER,
            ERROR_NO_MORE_FILES, ERROR_NOT_SUPPORTED, HANDLE, OBJ_CASE_INSENSITIVE,
            RtlNtStatusToDosError, STATUS_INVALID_DEVICE_REQUEST, STATUS_INVALID_INFO_CLASS,
            STATUS_INVALID_PARAMETER, STATUS_NOT_IMPLEMENTED, STATUS_NOT_SUPPORTED,
            STATUS_OBJECT_NAME_INVALID, UNICODE_STRING,
        },
        Security::GetKernelObjectSecurity,
        Storage::FileSystem::{
            FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_TAG_INFO, FILE_DISPOSITION_FLAG_DELETE,
            FILE_DISPOSITION_FLAG_POSIX_SEMANTICS, FILE_DISPOSITION_INFO, FILE_DISPOSITION_INFO_EX,
            FILE_FULL_DIR_INFO, FileAttributeTagInfo, FileDispositionInfo, FileDispositionInfoEx,
            FileFullDirectoryInfo, FileFullDirectoryRestartInfo, GetFileInformationByHandleEx,
            SYNCHRONIZE, SetFileInformationByHandle,
        },
        System::IO::IO_STATUS_BLOCK,
    },
};

/// The `NTSTATUS` an open failed with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Status(pub i32);

impl Status {
    /// The status as the Win32 error it corresponds to.
    #[must_use]
    pub fn io_error(self) -> io::Error {
        // SAFETY: the call only maps one number to another.
        let code = unsafe { RtlNtStatusToDosError(self.0) };
        io::Error::from_raw_os_error(i32::try_from(code).unwrap_or(i32::MAX))
    }
}

/// How `open_beneath` opens an entry: the arguments of `NtCreateFile`.
/// `SYNCHRONIZE` access and the `FILE_OPEN_REPARSE_POINT` and
/// `FILE_SYNCHRONOUS_IO_NONALERT` options are always added.
#[derive(Clone, Copy, Debug)]
pub struct Open {
    /// The access mask, such as `FILE_LIST_DIRECTORY` or `DELETE`.
    pub access: u32,
    /// The `FILE_SHARE_*` flags.
    pub share: u32,
    /// The disposition, such as `FILE_OPEN` or `FILE_CREATE`.
    pub disposition: u32,
    /// The options, such as `FILE_DIRECTORY_FILE`.
    pub options: u32,
}

/// A name as `UTF-16`, or `None` if it could name more than one entry of a
/// directory: it has a `\`, `/`, `:` or NUL, is `.` or `..`, or is too
/// long. The empty name is allowed only where `empty`.
fn wide(name: &str, empty: bool) -> Option<Vec<u16>> {
    let bad = (name.is_empty() && !empty)
        || name == "."
        || name == ".."
        || name.contains(['\\', '/', ':', '\0']);
    let wide: Vec<u16> = name.encode_utf16().collect();
    // A `UNICODE_STRING` counts bytes in a `u16`.
    (!bad && wide.len() <= usize::from(u16::MAX / 2)).then_some(wide)
}

/// Opens `name`, one entry of the directory `directory` is a handle on,
/// relative to that handle. A reparse point at `name` is opened itself and
/// never followed. The empty name opens `directory` again, with the access
/// and sharing asked. A name that is not a single entry is refused with
/// `STATUS_OBJECT_NAME_INVALID`. New files and directories get the
/// security their directory gives them.
pub fn open_beneath(
    directory: BorrowedHandle<'_>,
    name: &str,
    open: &Open,
) -> Result<OwnedHandle, Status> {
    let Some(mut wide) = wide(name, true) else {
        return Err(Status(STATUS_OBJECT_NAME_INVALID));
    };
    let length = u16::try_from(wide.len() * 2).map_err(|_| Status(STATUS_OBJECT_NAME_INVALID))?;
    let object_name = UNICODE_STRING {
        Length: length,
        MaximumLength: length,
        Buffer: wide.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: u32::try_from(size_of::<OBJECT_ATTRIBUTES>()).unwrap_or(u32::MAX),
        RootDirectory: directory.as_raw_handle(),
        ObjectName: &raw const object_name,
        Attributes: OBJ_CASE_INSENSITIVE,
        SecurityDescriptor: ptr::null(),
        SecurityQualityOfService: ptr::null(),
    };
    let mut handle: HANDLE = ptr::null_mut();
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: every pointer is to a live local for the whole call: the
    // handle and status block it writes, the attributes and the name they
    // point to, whose buffer outlives the call. `directory` is an open
    // handle for as long as it is borrowed. No allocation size or extended
    // attributes are passed.
    let status = unsafe {
        NtCreateFile(
            &raw mut handle,
            open.access | SYNCHRONIZE,
            &raw const attributes,
            &raw mut status_block,
            ptr::null(),
            FILE_ATTRIBUTE_NORMAL,
            open.share,
            open.disposition,
            open.options | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            ptr::null(),
            0,
        )
    };
    if status < 0 || handle.is_null() {
        return Err(Status(status));
    }
    // SAFETY: `NtCreateFile` succeeded, so `handle` is an open handle that
    // nothing else owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

/// The attributes of the entry `handle` is open on, and its reparse tag if
/// it is a reparse point. The handle needs `FILE_READ_ATTRIBUTES` access.
pub fn attributes(handle: BorrowedHandle<'_>) -> io::Result<(u32, u32)> {
    let mut information = FILE_ATTRIBUTE_TAG_INFO::default();
    // SAFETY: the buffer is a live `FILE_ATTRIBUTE_TAG_INFO` of the size
    // passed, as the information class asks; the handle is open.
    let done = unsafe {
        GetFileInformationByHandleEx(
            handle.as_raw_handle(),
            FileAttributeTagInfo,
            (&raw mut information).cast(),
            size_of_u32::<FILE_ATTRIBUTE_TAG_INFO>(),
        )
    };
    if done == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((information.FileAttributes, information.ReparseTag))
}

fn size_of_u32<T>() -> u32 {
    u32::try_from(size_of::<T>()).unwrap_or(u32::MAX)
}

/// Whether a `SetFileInformationByHandle` failure says only that this file
/// system does not have the newer information class.
fn unsupported(error: &io::Error) -> bool {
    error
        .raw_os_error()
        .and_then(|code| u32::try_from(code).ok())
        .is_some_and(|code| {
            matches!(
                code,
                ERROR_INVALID_PARAMETER | ERROR_NOT_SUPPORTED | ERROR_INVALID_FUNCTION
            )
        })
}

/// Whether a failed `NtSetInformationFile` says only that this file system
/// does not have the newer information class.
fn unsupported_class(status: i32) -> bool {
    matches!(
        status,
        STATUS_INVALID_PARAMETER
            | STATUS_INVALID_INFO_CLASS
            | STATUS_NOT_SUPPORTED
            | STATUS_NOT_IMPLEMENTED
            | STATUS_INVALID_DEVICE_REQUEST
    )
}

/// Renames the file `file` is open on to `name` in the directory
/// `directory` is a handle on, replacing an entry by that name that is
/// not a directory. `file` must be open with `DELETE` access. The new name
/// is resolved by the kernel relative to `directory`, never through a path.
pub fn rename_beneath(
    file: BorrowedHandle<'_>,
    directory: BorrowedHandle<'_>,
    name: &str,
) -> io::Result<()> {
    let Some(wide) = wide(name, false) else {
        return Err(io::Error::from(io::ErrorKind::InvalidInput));
    };
    let header = offset_of!(FILE_RENAME_INFORMATION, FileName);
    debug_assert_eq!(header % 2, 0);
    let name_bytes = wide.len() * 2;
    let size = (header + name_bytes).max(size_of::<FILE_RENAME_INFORMATION>());
    let length = u32::try_from(name_bytes).map_err(|_| io::ErrorKind::InvalidInput)?;
    let size_u32 = u32::try_from(size).map_err(|_| io::ErrorKind::InvalidInput)?;
    let set = |class, flags: FILE_RENAME_INFORMATION_0| -> Result<(), Status> {
        // Zeroed and as aligned as a `u64`, which the structure is.
        let mut buffer = vec![0_u64; size.div_ceil(8)];
        let base = buffer.as_mut_ptr();
        let information = base.cast::<FILE_RENAME_INFORMATION>();
        // The name starts at `header`, an even offset into the buffer.
        let destination = base.cast::<u16>().wrapping_add(header / 2);
        // SAFETY: the buffer is aligned for and at least as large as a
        // `FILE_RENAME_INFORMATION`, and `information` points to its start.
        unsafe {
            information.write(FILE_RENAME_INFORMATION {
                Anonymous: flags,
                RootDirectory: directory.as_raw_handle(),
                FileNameLength: length,
                FileName: [0],
            });
        }
        // SAFETY: `destination` is aligned for `u16`, and the name's bytes
        // fit between `header` and `size`, inside the buffer, which does not
        // overlap `wide`.
        unsafe { ptr::copy_nonoverlapping(wide.as_ptr(), destination, wide.len()) };
        let mut status_block = IO_STATUS_BLOCK::default();
        // SAFETY: the buffer holds a complete `FILE_RENAME_INFORMATION`
        // with its name, `size_u32` bytes long; the status block is a live
        // local; both handles are open.
        let status = unsafe {
            NtSetInformationFile(
                file.as_raw_handle(),
                &raw mut status_block,
                buffer.as_ptr().cast(),
                size_u32,
                class,
            )
        };
        if status < 0 {
            Err(Status(status))
        } else {
            Ok(())
        }
    };
    // POSIX semantics replace a target others have open; file systems
    // without them get the older class.
    match set(
        FileRenameInformationEx,
        FILE_RENAME_INFORMATION_0 {
            Flags: FILE_RENAME_REPLACE_IF_EXISTS | FILE_RENAME_POSIX_SEMANTICS,
        },
    ) {
        // `ReplaceIfExists`, the union's first byte, set and the rest zero.
        Err(status) if unsupported_class(status.0) => set(
            FileRenameInformation,
            FILE_RENAME_INFORMATION_0 { Flags: 1 },
        ),
        result => result,
    }
    .map_err(Status::io_error)
}

/// Deletes the file, or empty directory, `handle` is open on, which must
/// be open with `DELETE` access. Where the file system can, its name goes
/// at once, whoever else has it open; elsewhere when the last handle on it
/// is closed.
pub fn delete(handle: BorrowedHandle<'_>) -> io::Result<()> {
    let information = FILE_DISPOSITION_INFO_EX {
        Flags: FILE_DISPOSITION_FLAG_DELETE | FILE_DISPOSITION_FLAG_POSIX_SEMANTICS,
    };
    // SAFETY: the buffer is a live `FILE_DISPOSITION_INFO_EX` of the size
    // passed; the handle is open.
    let done = unsafe {
        SetFileInformationByHandle(
            handle.as_raw_handle(),
            FileDispositionInfoEx,
            (&raw const information).cast(),
            size_of_u32::<FILE_DISPOSITION_INFO_EX>(),
        )
    };
    if done != 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if !unsupported(&error) {
        return Err(error);
    }
    let information = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: the buffer is a live `FILE_DISPOSITION_INFO` of the size
    // passed; the handle is open.
    let done = unsafe {
        SetFileInformationByHandle(
            handle.as_raw_handle(),
            FileDispositionInfo,
            (&raw const information).cast(),
            size_of_u32::<FILE_DISPOSITION_INFO>(),
        )
    };
    if done == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// The largest security descriptor read, far above any real one.
const SECURITY_LIMIT: usize = 1 << 20;

/// The security descriptor of what `handle` is open on, in self-relative
/// form, with the parts `information` asks for (`*_SECURITY_INFORMATION`
/// flags). The handle needs `READ_CONTROL` access.
pub fn security(handle: BorrowedHandle<'_>, information: u32) -> io::Result<Vec<u8>> {
    // As aligned as a `u64`, which is more than a descriptor needs.
    let mut buffer = vec![0_u64; 64];
    loop {
        let size = u32::try_from(buffer.len() * 8).unwrap_or(u32::MAX);
        let mut needed = 0_u32;
        // SAFETY: the buffer is live and `size` bytes long; the handle is
        // open; the call writes the length it needs to a live local.
        let done = unsafe {
            GetKernelObjectSecurity(
                handle.as_raw_handle(),
                information,
                buffer.as_mut_ptr().cast(),
                size,
                &raw mut needed,
            )
        };
        let needed = usize::try_from(needed).unwrap_or(usize::MAX);
        if done != 0 {
            let bytes: Vec<u8> = buffer.iter().flat_map(|word| word.to_ne_bytes()).collect();
            return Ok(bytes[..needed.min(bytes.len())].to_vec());
        }
        let error = io::Error::last_os_error();
        let short = error.raw_os_error() == i32::try_from(ERROR_INSUFFICIENT_BUFFER).ok();
        if !short || needed <= buffer.len() * 8 || needed > SECURITY_LIMIT {
            return Err(error);
        }
        buffer = vec![0_u64; needed.div_ceil(8)];
    }
}

/// One entry of a directory listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The name, as the file system holds it.
    pub name: Vec<u16>,
    /// The `FILE_ATTRIBUTE_*` flags.
    pub attributes: u32,
}

/// The size of each read of a directory.
const LISTING_BYTES: usize = 64 * 1024;

/// Reads the next entries of the directory `directory` is a handle on,
/// which needs `FILE_LIST_DIRECTORY` access; from the first entry when
/// `restart`. `None` when there are no more. `.` and `..` are included.
pub fn read_directory(
    directory: BorrowedHandle<'_>,
    restart: bool,
) -> io::Result<Option<Vec<Entry>>> {
    let mut buffer = vec![0_u64; LISTING_BYTES / 8];
    let class = if restart {
        FileFullDirectoryRestartInfo
    } else {
        FileFullDirectoryInfo
    };
    // SAFETY: the buffer is live, as aligned as the records the class
    // writes, and as long as the size passed; the handle is open.
    let done = unsafe {
        GetFileInformationByHandleEx(
            directory.as_raw_handle(),
            class,
            buffer.as_mut_ptr().cast(),
            u32::try_from(LISTING_BYTES).unwrap_or(u32::MAX),
        )
    };
    if done == 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() == i32::try_from(ERROR_NO_MORE_FILES).ok() {
            return Ok(None);
        }
        return Err(error);
    }
    let bytes: Vec<u8> = buffer.iter().flat_map(|word| word.to_ne_bytes()).collect();
    parse_listing(&bytes).map(Some)
}

/// The entries of a `FILE_FULL_DIR_INFO` listing, every offset and length
/// checked against the buffer.
fn parse_listing(bytes: &[u8]) -> io::Result<Vec<Entry>> {
    let bad = || io::Error::new(io::ErrorKind::InvalidData, "malformed directory listing");
    let read = |at: usize| -> io::Result<u32> {
        bytes
            .get(at..at + 4)
            .and_then(|field| field.try_into().ok())
            .map(u32::from_ne_bytes)
            .ok_or_else(bad)
    };
    let mut entries = Vec::new();
    let mut at = 0_usize;
    loop {
        let next = usize::try_from(read(at + offset_of!(FILE_FULL_DIR_INFO, NextEntryOffset))?)
            .map_err(|_| bad())?;
        let attributes = read(at + offset_of!(FILE_FULL_DIR_INFO, FileAttributes))?;
        let length = usize::try_from(read(at + offset_of!(FILE_FULL_DIR_INFO, FileNameLength))?)
            .map_err(|_| bad())?;
        let start = at + offset_of!(FILE_FULL_DIR_INFO, FileName);
        let name = bytes
            .get(start..start.checked_add(length).ok_or_else(bad)?)
            .ok_or_else(bad)?
            .chunks_exact(2)
            .map(|pair| u16::from_ne_bytes([pair[0], pair[1]]))
            .collect();
        entries.push(Entry { name, attributes });
        if next == 0 {
            return Ok(entries);
        }
        at = at.checked_add(next).ok_or_else(bad)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_single_entries() {
        assert!(wide("a.jsonc", false).is_some());
        assert!(wide("", true).is_some());
        for name in ["", ".", "..", "a\\b", "a/b", "c:", "a\0b"] {
            assert!(wide(name, false).is_none(), "{name:?}");
        }
        assert!(wide(&"x".repeat(40_000), false).is_none());
    }

    #[test]
    fn listings_are_bounds_checked() {
        assert!(parse_listing(&[0; 8]).is_err());
        let mut record = vec![0_u8; offset_of!(FILE_FULL_DIR_INFO, FileName) + 4];
        let length = offset_of!(FILE_FULL_DIR_INFO, FileNameLength);
        record[length..length + 4].copy_from_slice(&4_u32.to_ne_bytes());
        let name = offset_of!(FILE_FULL_DIR_INFO, FileName);
        record[name..name + 4].copy_from_slice(&[b'a', 0, b'b', 0]);
        let entries = parse_listing(&record).unwrap();
        assert_eq!(entries[0].name, [u16::from(b'a'), u16::from(b'b')]);
        record[length..length + 4].copy_from_slice(&8_u32.to_ne_bytes());
        assert!(parse_listing(&record).is_err());
        record[length..length + 4].copy_from_slice(&4_u32.to_ne_bytes());
        record[0..4].copy_from_slice(&1000_u32.to_ne_bytes());
        assert!(parse_listing(&record).is_err());
    }
}
