// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! The files of a revision: [`World::files`](crate::World::files) lists
//! exactly the files whose bytes the source digest covers.

use super::{copy_tree, fixture, load};
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

/// Every file under `root`, by `/`-separated relative path.
fn every_file(root: &Path, prefix: &str, found: &mut BTreeSet<String>) {
    for entry in fs::read_dir(root.join(prefix)).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().into_string().unwrap();
        let path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if entry.file_type().unwrap().is_dir() {
            every_file(root, &path, found);
        } else {
            found.insert(path);
        }
    }
}

fn fixtures() -> [PathBuf; 2] {
    [
        fixture(),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/terrain"),
    ]
}

#[test]
fn the_files_of_a_revision_are_those_whose_change_changes_it() {
    for from in fixtures() {
        let world = load(&from).unwrap();
        let listed: BTreeSet<String> = world.files().keys().cloned().collect();
        let mut on_disk = BTreeSet::new();
        every_file(&from, "", &mut on_disk);
        // What is listed is on disk, as it was read.
        for (path, file) in world.files() {
            let bytes = fs::read(from.join(path)).unwrap();
            assert_eq!(file.length(), u64::try_from(bytes.len()).unwrap(), "{path}");
            let digest: [u8; 32] = Sha256::digest(&bytes).into();
            assert_eq!(file.sha256(), digest, "{path}");
        }
        assert!(listed.is_subset(&on_disk));
        // Each file on disk changes the revision exactly when it is listed:
        // one more byte, which every document allows at its end.
        for path in &on_disk {
            let copy = tempfile::tempdir().unwrap();
            copy_tree(&from, copy.path());
            fs::OpenOptions::new()
                .append(true)
                .open(copy.path().join(path))
                .unwrap()
                .write_all(b" ")
                .unwrap();
            let changed = load(copy.path()).unwrap();
            assert_eq!(
                changed.source_digest() != world.source_digest(),
                listed.contains(path),
                "{path}"
            );
            if listed.contains(path) {
                assert_eq!(
                    changed.files()[path].length(),
                    world.files()[path].length() + 1
                );
                assert_eq!(changed.files().len(), world.files().len());
            } else {
                assert_eq!(changed.files(), world.files(), "{path}");
            }
        }
        // A file the loader does not read is not listed, and does not
        // change the revision.
        let copy = tempfile::tempdir().unwrap();
        copy_tree(&from, copy.path());
        fs::write(copy.path().join("profile/extra.jsonc"), "{\"a\": 1}\n").unwrap();
        let extra = load(copy.path()).unwrap();
        assert!(!extra.files().contains_key("profile/extra.jsonc"));
        assert_eq!(extra.source_digest(), world.source_digest());
        assert_eq!(extra.files(), world.files());
    }
}

#[test]
fn the_terrain_fixture_has_a_file_the_loader_does_not_read() {
    // So the test above checks both sides on it.
    let [_, terrain] = fixtures();
    let world = load(&terrain).unwrap();
    assert!(terrain.join("resolved.txt").is_file());
    assert!(!world.files().contains_key("resolved.txt"));
    assert!(world.files().contains_key("profile/transitions.jsonc"));
}

#[test]
fn an_overlay_file_is_listed_as_the_overlay_gives_it() {
    let coin = fs::read_to_string(fixture().join("defs/proto/coin.jsonc")).unwrap();
    let extra = coin.replacen("\"coin\"", "\"extra\"", 1);
    let edited = format!("{coin}// edited\n");
    let overlay = crate::Overlay::from([
        ("defs/proto/extra.jsonc".to_owned(), Some(extra.clone())),
        ("defs/proto/coin.jsonc".to_owned(), Some(edited.clone())),
    ]);
    let world = crate::load_edited(&fixture(), &crate::LoadOptions::default(), &overlay).unwrap();
    for (path, text) in [
        ("defs/proto/extra.jsonc", &extra),
        ("defs/proto/coin.jsonc", &edited),
    ] {
        let digest: [u8; 32] = Sha256::digest(text.as_bytes()).into();
        assert_eq!(world.files()[path].sha256(), digest, "{path}");
    }
    let plain = load(&fixture()).unwrap();
    assert_eq!(world.files().len(), plain.files().len() + 1);
}
