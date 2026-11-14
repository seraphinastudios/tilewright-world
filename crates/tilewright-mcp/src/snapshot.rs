// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! The tree as the server last loaded it. Every result is computed from one
//! snapshot and names its revision; `refresh` replaces it.

use crate::json::{fingerprint, hex};
use std::{collections::BTreeMap, path::Path};
use tilewright_source::{Diagnostic, LoadOptions, Overlay, SourceRoot, World};

/// One load of the tree.
pub struct Snapshot {
    /// What cursors are bound to: the revision of a tree that loads, or,
    /// for one that does not, `x` and a fingerprint of its diagnostics, so
    /// a reload that finds the same problems keeps its cursors.
    pub binding: String,
    pub state: State,
}

pub enum State {
    Valid(Box<Valid>),
    /// The tree does not load. Today's loader gives no revision for it.
    Invalid(Vec<Diagnostic>),
}

pub struct Valid {
    pub world: World,
    /// The lowercase hex `source_digest`.
    pub revision: String,
    /// The world's IDs by kind, or `None` when it has more than an ID
    /// snapshot holds (`tilewright_diff::DEFAULT_MAXIMUM_IDS`).
    pub ids: Option<tilewright_diff::Snapshot>,
    /// Each chunk's object count, inventory entries included, by plane and
    /// chunk key, so a search resuming past a chunk need not walk it.
    pub chunk_objects: ChunkCounts,
}

/// Object counts by plane ID, then chunk key.
pub type ChunkCounts = BTreeMap<String, BTreeMap<(i32, i64, i64), usize>>;

impl Snapshot {
    /// Loads the tree at `root`, with the bounds `tilewright validate`
    /// uses. Loading only reads. The root is opened afresh each time, so a
    /// root created, replaced or repaired since the last load is the one
    /// read, on every platform; a root that is missing, or not a
    /// directory, is reported as such.
    pub fn load(root: &Path) -> Self {
        let problem = match std::fs::metadata(root) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some((
                "root.missing",
                "the root directory does not exist: create it, then call refresh",
            )),
            Ok(metadata) if !metadata.is_dir() => Some((
                "root.not_directory",
                "the root is not a directory: replace it with the tree, then call refresh",
            )),
            // Anything else, the load reports in its own words.
            _ => None,
        };
        let loaded = match problem {
            Some((code, message)) => Err(vec![Diagnostic {
                path: ".".to_owned(),
                start: 0,
                end: 0,
                line: 0,
                column: 0,
                code,
                message: message.to_owned(),
            }]),
            None => tilewright_source::load_edited_in(
                &SourceRoot::new(root),
                &LoadOptions::default(),
                &Overlay::new(),
            ),
        };
        match loaded {
            Ok(world) => {
                let revision = hex(&world.source_digest());
                let ids = tilewright_diff::Snapshot::from_world(
                    &world,
                    &tilewright_diff::Limits::default(),
                )
                .ok();
                let chunk_objects = world
                    .planes()
                    .iter()
                    .map(|(id, plane)| {
                        let counts = plane
                            .chunks
                            .iter()
                            .map(|(key, chunk)| (*key, chunk.depth_first().count()))
                            .collect();
                        (id.clone(), counts)
                    })
                    .collect();
                Self {
                    binding: revision.clone(),
                    state: State::Valid(Box::new(Valid {
                        world,
                        revision,
                        ids,
                        chunk_objects,
                    })),
                }
            }
            Err(diagnostics) => {
                let mut parts: Vec<Vec<u8>> = Vec::new();
                for diagnostic in &diagnostics {
                    parts.push(diagnostic.path.as_bytes().to_vec());
                    parts.push(
                        format!(
                            "{} {} {} {} {}",
                            diagnostic.start,
                            diagnostic.end,
                            diagnostic.line,
                            diagnostic.column,
                            diagnostic.code
                        )
                        .into_bytes(),
                    );
                    parts.push(diagnostic.message.as_bytes().to_vec());
                }
                let parts: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
                Self {
                    binding: format!("x{:016x}", fingerprint(&parts)),
                    state: State::Invalid(diagnostics),
                }
            }
        }
    }

    /// A snapshot that holds nothing, to stand in while the tree loads
    /// again, so that two worlds are never held at once.
    pub fn empty() -> Self {
        Self {
            binding: String::new(),
            state: State::Invalid(Vec::new()),
        }
    }

    /// The revision, for a tree that loads.
    pub fn revision(&self) -> Option<&str> {
        match &self.state {
            State::Valid(valid) => Some(&valid.revision),
            State::Invalid(_) => None,
        }
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        match &self.state {
            State::Valid(_) => &[],
            State::Invalid(diagnostics) => diagnostics,
        }
    }
}
