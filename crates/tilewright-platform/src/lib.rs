// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Safe wrappers over the few operating-system calls that the toolkit's
//! confined file access needs and that neither Rust's standard library nor
//! rustix offers:
//!
//! - on Windows, opening, renaming and deleting an entry relative to a
//!   directory handle without following a reparse point, reading a
//!   handle's security descriptor and listing a directory by its handle;
//! - on macOS, telling whether a file has an access control list.
//!
//! This is the only crate of the workspace that may use `unsafe`. Every
//! unsafe block holds one call and says why it is sound; everything a
//! caller passes in is checked or copied first, so no input can make a
//! call unsound.

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub mod macos;
#[cfg(windows)]
#[allow(unsafe_code)]
pub mod windows;
