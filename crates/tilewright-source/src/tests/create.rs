// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! [`SourceDirectory::create`](crate::SourceDirectory::create) through the
//! public API, on every platform: it makes only new files, checks names as
//! `write` does, and of several creates racing for one name exactly one
//! wins.

use crate::{AccessError, SourceRoot};
use std::{fs, sync::Barrier};

#[test]
fn a_create_makes_a_new_file_and_refuses_a_taken_name() {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("dir")).unwrap();
    fs::write(directory.path().join("dir/taken"), "keep").unwrap();
    let root = SourceRoot::new(directory.path());
    let dir = root.directory("dir", false).unwrap().unwrap();

    dir.create("made", None, b"exact bytes").unwrap();
    assert_eq!(
        fs::read(directory.path().join("dir/made")).unwrap(),
        b"exact bytes"
    );
    // A second create of the same name is refused, and the error says why.
    let again = dir.create("made", None, b"other").unwrap_err();
    assert!(again.already_exists(), "{again}");
    assert!(matches!(&again, AccessError::Io(path, error)
        if path == "dir/made" && error.kind() == std::io::ErrorKind::AlreadyExists));
    assert_eq!(
        fs::read(directory.path().join("dir/made")).unwrap(),
        b"exact bytes"
    );
    let taken = dir.create("taken", None, b"x").unwrap_err();
    assert!(taken.already_exists());
    assert_eq!(
        fs::read_to_string(directory.path().join("dir/taken")).unwrap(),
        "keep"
    );
    // So is a directory's name.
    fs::create_dir(directory.path().join("dir/sub")).unwrap();
    assert!(dir.create("sub", None, b"x").unwrap_err().already_exists());
    assert!(directory.path().join("dir/sub").is_dir());

    // Names are checked as for write, before anything is made.
    for name in [
        "",
        ".",
        "..",
        "a/b",
        "a\\b",
        "c:d",
        "nul\0",
        &"n".repeat(256),
    ] {
        assert!(
            matches!(dir.create(name, None, b"x"), Err(AccessError::Path(_))),
            "{name:?}"
        );
        assert!(
            matches!(dir.write(name, None, b"x"), Err(AccessError::Path(_))),
            "{name:?}"
        );
    }
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);

    // Other errors are not mistaken for a taken name.
    assert!(!AccessError::Locked.already_exists());
    assert!(!AccessError::pending_removal("x").already_exists());
    let plain = AccessError::Io("x".to_owned(), std::io::ErrorKind::AlreadyExists.into());
    assert!(!plain.already_exists());
}

#[test]
fn of_creates_racing_for_a_name_exactly_one_wins() {
    const RACERS: usize = 16;
    let directory = tempfile::tempdir().unwrap();
    let root = SourceRoot::new(directory.path());
    let dir = root.directory("", false).unwrap().unwrap();
    // Each writes its own bytes, long enough to take a while.
    let bytes: Vec<Vec<u8>> = (0..RACERS)
        .map(|racer| vec![u8::try_from(racer).unwrap(); 256 << 10])
        .collect();
    for round in 0..8 {
        let name = format!("race-{round}");
        let start = Barrier::new(RACERS);
        let results: Vec<Result<(), AccessError>> = std::thread::scope(|scope| {
            let handles: Vec<_> = bytes
                .iter()
                .map(|mine| {
                    let (dir, start, name) = (&dir, &start, &name);
                    scope.spawn(move || {
                        start.wait();
                        dir.create(name, None, mine)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect()
        });
        let winners: Vec<usize> = results
            .iter()
            .enumerate()
            .filter_map(|(racer, result)| result.is_ok().then_some(racer))
            .collect();
        assert_eq!(winners.len(), 1, "round {round}: {results:?}");
        for result in &results {
            if let Err(error) = result {
                assert!(error.already_exists(), "round {round}: {error}");
            }
        }
        assert_eq!(
            fs::read(directory.path().join(&name)).unwrap(),
            bytes[winners[0]],
            "round {round}"
        );
    }
    // Nothing else was made or left.
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 8);
}

/// Fails every create on this thread right after it made its file, after
/// running `meanwhile`, until dropped.
struct Failing;

impl Failing {
    fn new(mut meanwhile: impl FnMut() -> std::io::Result<()> + 'static) -> Self {
        crate::files::AFTER_CREATE.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                meanwhile()?;
                Err(std::io::ErrorKind::StorageFull.into())
            }));
        });
        Self
    }
}

impl Drop for Failing {
    fn drop(&mut self) {
        crate::files::AFTER_CREATE.with(|slot| *slot.borrow_mut() = None);
    }
}

#[test]
fn a_failed_create_says_whether_its_file_may_be_left() {
    let directory = tempfile::tempdir().unwrap();
    let root = SourceRoot::new(directory.path());
    let dir = root.directory("", false).unwrap().unwrap();
    let at = |name: &str| directory.path().join(name);

    // A refusal and a bad name leave nothing.
    fs::write(at("taken"), "keep").unwrap();
    let taken = dir.create("taken", None, b"x").unwrap_err();
    assert!(taken.already_exists() && !taken.may_be_left());
    assert!(!dir.create("a/b", None, b"x").unwrap_err().may_be_left());
    assert!(!AccessError::Locked.may_be_left());
    assert!(!AccessError::pending_removal("x").may_be_left());

    // A failure after the file was made: on Unix it is removed, being still
    // the file made; on Windows it is never removed. Either way the error
    // keeps its kind, and says which.
    let failed = {
        let _failing = Failing::new(|| Ok(()));
        dir.create("failed", None, b"x").unwrap_err()
    };
    assert!(
        matches!(&failed, AccessError::Io(path, error)
            if path == "failed" && error.kind() == std::io::ErrorKind::StorageFull),
        "{failed}"
    );
    assert!(!failed.already_exists());
    assert_eq!(failed.may_be_left(), cfg!(windows), "{failed}");
    assert_eq!(at("failed").exists(), cfg!(windows));

    // Where another process replaced it at the name meanwhile, its file
    // stays, and the error says the file made may be left.
    let swapped = {
        let (name, moved) = (at("swapped"), at("moved"));
        let _failing = Failing::new(move || {
            fs::rename(&name, &moved)?;
            fs::write(&name, "someone else's")
        });
        dir.create("swapped", None, b"x").unwrap_err()
    };
    assert!(swapped.may_be_left(), "{swapped}");
    assert_eq!(fs::read_to_string(at("swapped")).unwrap(), "someone else's");
}
