// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! The tree as the server last loaded it. Every result is computed from one
//! snapshot and names its revision; `refresh` replaces it.

use crate::json::{fingerprint, hex};
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
}

impl Snapshot {
    /// Loads the tree of `root`, with the bounds `tilewright validate` uses.
    /// Loading only reads.
    pub fn load(root: &SourceRoot) -> Self {
        match tilewright_source::load_edited_in(root, &LoadOptions::default(), &Overlay::new()) {
            Ok(world) => {
                let revision = hex(&world.source_digest());
                let ids = tilewright_diff::Snapshot::from_world(
                    &world,
                    &tilewright_diff::Limits::default(),
                )
                .ok();
                Self {
                    binding: revision.clone(),
                    state: State::Valid(Box::new(Valid {
                        world,
                        revision,
                        ids,
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
