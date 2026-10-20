// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! macOS: access control lists, which neither the mode bits nor an
//! extended attribute an unprivileged process may read reveal.

use std::{
    ffi::{c_int, c_uint, c_void},
    io,
    os::fd::{AsRawFd as _, BorrowedFd},
};

/// `acl_type_t`'s `ACL_TYPE_EXTENDED`, the one kind of list macOS keeps.
const ACL_TYPE_EXTENDED: c_uint = 0x0000_0100;
/// `errno` values of macOS: no list, and no lists on this file system.
const ENOENT: i32 = 2;
const ENOTSUP: i32 = 45;
const EOPNOTSUPP: i32 = 102;

// In libSystem, which every macOS program links.
unsafe extern "C" {
    fn acl_get_fd_np(fd: c_int, kind: c_uint) -> *mut c_void;
    fn acl_free(object: *mut c_void) -> c_int;
}

/// Whether the open file or directory has an access control list, even
/// one without entries.
pub fn has_access_control_list(file: BorrowedFd<'_>) -> io::Result<bool> {
    // SAFETY: the descriptor is open for as long as `file` borrows it, and
    // the call only reads its list into memory it allocates.
    let list = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
    if list.is_null() {
        let error = io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(ENOENT | ENOTSUP | EOPNOTSUPP) => Ok(false),
            _ => Err(error),
        };
    }
    // SAFETY: `list` came from `acl_get_fd_np` and is freed exactly once.
    unsafe { acl_free(list) };
    Ok(true)
}
