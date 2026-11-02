// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Opening packs from their bytes, as the WebAssembly reader does. The
//! conformance fixtures run through this path too; these tests cover what
//! only an image has: a reader that fails or ends early, a size limit
//! checked before any byte is read, and the expected digest.

use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use tilewright_pack::{ErrorCode, Pack, Page, VerifyLimits};

fn valid() -> (PathBuf, Vec<u8>) {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/packs");
    let path =
        directory.join("5830e26e449a1f0d0c786b32801b7ed68e351777db5f586532478cb4384844b2.twpack");
    let bytes = std::fs::read(&path).unwrap();
    (path, bytes)
}

fn code<T>(result: Result<T, tilewright_pack::Error>) -> ErrorCode {
    match result {
        Ok(_) => panic!("expected an error"),
        Err(error) => error.code(),
    }
}

/// Fails with `kind` after yielding `before` bytes of `bytes`.
struct Failing<'b> {
    bytes: &'b [u8],
    before: usize,
}

impl Read for Failing<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.before == 0 {
            return Err(std::io::ErrorKind::ConnectionReset.into());
        }
        let count = buffer.len().min(self.before).min(self.bytes.len());
        buffer[..count].copy_from_slice(&self.bytes[..count]);
        self.bytes = &self.bytes[count..];
        self.before -= count;
        Ok(count)
    }
}

#[test]
fn an_image_opens_reads_and_verifies_like_its_file() {
    let (path, bytes) = valid();
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    let image = Pack::open_bytes(&bytes, Some(digest), &VerifyLimits::default()).unwrap();
    let file = Pack::open(&path).unwrap();
    assert_eq!(image.verify().unwrap(), file.verify().unwrap());
    assert_eq!(image.file_digest().unwrap(), digest);
    assert_eq!(image.meta("format").unwrap(), file.meta("format").unwrap());
    assert_eq!(
        image.chunk("overworld", 0, 0, 0).unwrap(),
        file.chunk("overworld", 0, 0, 0).unwrap()
    );
    // Without an expected digest there is nothing to compare.
    let unnamed = Pack::open_bytes(&bytes, None, &VerifyLimits::default()).unwrap();
    assert_eq!(unnamed.verify().unwrap(), file.verify().unwrap());
    assert_eq!(unnamed.file_digest().unwrap(), digest);
}

#[test]
fn verification_compares_the_expected_digest() {
    let (_, bytes) = valid();
    let mut digest: [u8; 32] = Sha256::digest(&bytes).into();
    digest[31] ^= 1;
    // Opening does not compare the digests; verification does.
    let pack = Pack::open_bytes(&bytes, Some(digest), &VerifyLimits::default()).unwrap();
    assert_eq!(code(pack.verify()), ErrorCode::Digest);
}

#[test]
fn a_wrong_digest_is_found_before_the_integrity_check() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/packs");
    let bytes = std::fs::read(directory.join("verify-integrity.twpack")).unwrap();
    let mut digest: [u8; 32] = Sha256::digest(&bytes).into();
    let right = Pack::open_bytes(&bytes, Some(digest), &VerifyLimits::default()).unwrap();
    assert_eq!(code(right.verify()), ErrorCode::Integrity);
    digest[31] ^= 1;
    let wrong = Pack::open_bytes(&bytes, Some(digest), &VerifyLimits::default()).unwrap();
    assert_eq!(code(wrong.verify()), ErrorCode::Digest);
}

#[test]
fn a_listing_can_ask_for_the_digest() {
    let (_, bytes) = valid();
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    let pack = Pack::open_bytes(&bytes, Some(digest), &VerifyLimits::default()).unwrap();
    let mut planes = 0;
    pack.visit_planes(Page::all(), &mut |_| {
        planes += 1;
        assert_eq!(pack.file_digest()?, digest);
        pack.verify().map(drop)
    })
    .unwrap();
    assert!(planes > 0);
}

#[test]
fn the_size_limit_is_checked_before_reading() {
    let (_, bytes) = valid();
    let length = u64::try_from(bytes.len()).unwrap();
    let limits = VerifyLimits {
        maximum_pack_bytes: length - 1,
        ..VerifyLimits::default()
    };
    let unread = Failing {
        bytes: &[],
        before: 0,
    };
    assert_eq!(
        code(Pack::open_from(unread, length, None, &limits)),
        ErrorCode::TooLarge
    );
    // At the limit it opens, and verifying with a lower limit fails.
    let at_limit = VerifyLimits {
        maximum_pack_bytes: length,
        ..VerifyLimits::default()
    };
    let pack = Pack::open_bytes(&bytes, None, &at_limit).unwrap();
    assert_eq!(code(pack.verify_with(&limits)), ErrorCode::TooLarge);
}

#[test]
fn an_image_that_cannot_be_read_is_an_io_error() {
    let (_, bytes) = valid();
    let length = u64::try_from(bytes.len()).unwrap();
    for before in [0, 1, 4096, bytes.len() - 1] {
        let failing = Failing {
            bytes: &bytes,
            before,
        };
        assert_eq!(
            code(Pack::open_from(
                failing,
                length,
                None,
                &VerifyLimits::default()
            )),
            ErrorCode::Io,
            "failing after {before} bytes"
        );
    }
    // A reader that ends early.
    for kept in [0, 100, bytes.len() - 1] {
        assert_eq!(
            code(Pack::open_from(
                &bytes[..kept],
                length,
                None,
                &VerifyLimits::default()
            )),
            ErrorCode::Io,
            "ending after {kept} bytes"
        );
    }
}

#[test]
fn an_image_is_exactly_its_length() {
    let (_, bytes) = valid();
    let length = u64::try_from(bytes.len()).unwrap();
    let mut longer = bytes.clone();
    longer.push(0);
    assert_eq!(
        code(Pack::open_from(
            longer.as_slice(),
            length,
            None,
            &VerifyLimits::default()
        )),
        ErrorCode::FileLength
    );
    // A length past the image's is a read that ends early.
    assert_eq!(
        code(Pack::open_from(
            bytes.as_slice(),
            length + 4096,
            None,
            &VerifyLimits::default()
        )),
        ErrorCode::Io
    );
    // A declared length of zero is checked too.
    assert_eq!(
        code(Pack::open_from(
            bytes.as_slice(),
            0,
            None,
            &VerifyLimits::default()
        )),
        ErrorCode::FileLength
    );
    let failing = Failing {
        bytes: &[],
        before: 0,
    };
    assert_eq!(
        code(Pack::open_from(failing, 0, None, &VerifyLimits::default())),
        ErrorCode::Io
    );
    // Bytes past the pages are the file's own trailing bytes.
    assert_eq!(
        code(Pack::open_bytes(&longer, None, &VerifyLimits::default())),
        ErrorCode::FileLength
    );
}

#[test]
fn an_empty_image_is_an_empty_database() {
    assert_eq!(
        code(Pack::open_bytes(&[], None, &VerifyLimits::default())),
        ErrorCode::Schema
    );
}

/// Yields `bytes` at most `chunk` bytes a read.
struct Trickle<'b> {
    bytes: &'b [u8],
    chunk: usize,
}

impl Read for Trickle<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let count = buffer.len().min(self.chunk).min(self.bytes.len());
        buffer[..count].copy_from_slice(&self.bytes[..count]);
        self.bytes = &self.bytes[count..];
        Ok(count)
    }
}

#[test]
fn the_header_and_digest_are_checked_however_few_bytes_each_read_yields() {
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/packs");
    let (_, valid) = valid();
    let open = |bytes: &[u8], chunk, digest| {
        let length = u64::try_from(bytes.len()).unwrap();
        Pack::open_from(
            Trickle { bytes, chunk },
            length,
            digest,
            &VerifyLimits::default(),
        )
    };
    let digest: [u8; 32] = Sha256::digest(&valid).into();
    for chunk in [1, 7, 19, 20, 21, 4096] {
        let pack = open(&valid, chunk, Some(digest)).unwrap();
        assert_eq!(
            pack.file_digest().unwrap(),
            digest,
            "reads of {chunk} bytes"
        );
        pack.verify().unwrap();
        for name in [
            "open-header-wal",
            "open-header-write-version",
            "open-header-read-version",
        ] {
            let bytes = std::fs::read(directory.join(format!("{name}.twpack"))).unwrap();
            assert_eq!(
                code(open(&bytes, chunk, None)),
                ErrorCode::Identity,
                "{name} in reads of {chunk} bytes"
            );
        }
    }
}
