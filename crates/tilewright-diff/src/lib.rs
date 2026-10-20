// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Compares the IDs of two Tilewright worlds (`docs/spec/DIFF.md`).
//!
//! Games keep IDs in saves, server state and scripts: planes, zones,
//! anchors, stairwells, definitions and render layers. Removing or renaming
//! one breaks whatever refers to it. A [`Snapshot`] holds the IDs of one
//! world, read from a source tree or a content pack, with a fingerprint of
//! each one's content. [`diff`] compares two snapshots and reports which IDs
//! were removed, added and renamed, and which removals an [`Allowlist`]
//! declares intended.
//!
//! ```no_run
//! # use std::path::Path;
//! let limits = tilewright_diff::Limits::default();
//! let old = tilewright_diff::load(Path::new("baseline.twpack"), &limits)?;
//! let new = tilewright_diff::load(Path::new("content"), &limits)?;
//! let report = tilewright_diff::diff(&old, &new, &tilewright_diff::Allowlist::default());
//! print!("{}", report.to_text());
//! if report.breaking() {
//!     eprintln!("IDs were removed or renamed");
//! }
//! # Ok::<(), tilewright_diff::Error>(())
//! ```

mod allow;
mod compare;
mod report;
mod snapshot;

pub use allow::{Allowance, Allowlist, AllowlistError};
pub use compare::diff;
pub use report::{Added, Basis, Reason, Removed, Renamed, Report, Unused};
pub use snapshot::{Input, Side, Snapshot};

use std::{fmt, path::Path};
use tilewright_source::Diagnostic;

/// The `format` of a JSON report.
pub const FORMAT: &str = "tilewright.diff/1";

/// The `format` of an allowlist document.
pub const ALLOWLIST_FORMAT: &str = "tilewright.diff-allow/1";

/// The default [`Limits::maximum_ids`].
pub const DEFAULT_MAXIMUM_IDS: usize = 1_000_000;

/// Bounds on what a snapshot may hold. A snapshot keeps every ID of its
/// world with a fingerprint, so its memory grows with its IDs.
///
/// These bound the snapshot, not loading: a pack's rows are counted as
/// they are read, so a pack with too many IDs stops at the first one past
/// the limit, but a source tree is loaded whole first, within the bounds of
/// [`tilewright_source::LoadOptions`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    /// Most IDs of all kinds one snapshot holds. Building a snapshot of a
    /// world with more stops at the first ID past the limit, before it is
    /// kept.
    pub maximum_ids: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            maximum_ids: DEFAULT_MAXIMUM_IDS,
        }
    }
}

/// A kind of ID. Kinds sort by name.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Kind {
    /// An anchor of a plane, written `<plane>/<id>`.
    Anchor,
    /// A definition, written `<type>/<id>`.
    Definition,
    /// A plane, written `<id>`.
    Plane,
    /// A render layer of the profile, written `<name>`.
    RenderLayer,
    /// A stairwell of a plane, written `<plane>/<id>`.
    Stairwell,
    /// A zone of a plane, written `<plane>/<id>`.
    Zone,
}

impl Kind {
    /// Every kind, in order.
    pub const ALL: [Self; 6] = [
        Self::Anchor,
        Self::Definition,
        Self::Plane,
        Self::RenderLayer,
        Self::Stairwell,
        Self::Zone,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Anchor => "anchor",
            Self::Definition => "definition",
            Self::Plane => "plane",
            Self::RenderLayer => "render_layer",
            Self::Stairwell => "stairwell",
            Self::Zone => "zone",
        }
    }

    /// The kind spelled `text`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == text)
    }

    /// Whether IDs of this kind belong to a plane and are written
    /// `<plane>/<id>`.
    #[must_use]
    pub const fn in_plane(self) -> bool {
        matches!(self, Self::Anchor | Self::Stairwell | Self::Zone)
    }

    /// Whether `id` is written as IDs of this kind are: two identifiers
    /// joined by `/` for kinds in a plane and for definitions, else one
    /// identifier.
    #[must_use]
    pub fn is_id(self, id: &str) -> bool {
        if self.in_plane() || self == Self::Definition {
            id.split_once('/')
                .is_some_and(|(scope, local)| is_identifier(scope) && is_identifier(local))
        } else {
            is_identifier(id)
        }
    }

    /// What an ID of this kind is unique within: its plane or definition
    /// type, or `""` for planes and render layers.
    pub(crate) fn scope(self, id: &str) -> &str {
        if self.in_plane() || self == Self::Definition {
            id.split_once('/').map_or("", |(scope, _)| scope)
        } else {
            ""
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Whether `text` matches `^[a-z0-9][a-z0-9_.-]{0,127}$`, the source
/// format's identifier pattern.
pub(crate) fn is_identifier(text: &str) -> bool {
    let bytes = text.as_bytes();
    (1..=128).contains(&bytes.len())
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_.-".contains(byte))
}

/// Why a side of a comparison could not be read.
#[derive(Debug)]
pub enum Error {
    /// The source tree did not load; its diagnostics, which include every
    /// reference that does not resolve.
    Source(Vec<Diagnostic>),
    /// The pack did not open, verify or read.
    Pack(tilewright_pack::Error),
    /// The world has more IDs than the limit, which this holds.
    TooManyIds(usize),
    /// A document that loaded could not be read again.
    Invalid(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(diagnostics) => {
                write!(formatter, "{} problem(s) in the source", diagnostics.len())
            }
            Self::Pack(error) => error.fmt(formatter),
            Self::TooManyIds(maximum) => {
                write!(formatter, "more than {maximum} IDs, the ID limit")
            }
            Self::Invalid(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for Error {}

/// Reads one side of a comparison: a content pack when `path` is a file,
/// which is opened and fully verified with the default bounds first, and
/// otherwise a source tree, which is loaded and validated with the default
/// bounds. The snapshot holds at most `limits.maximum_ids` IDs; see
/// [`Limits`] for what that bounds.
///
/// # Errors
/// Returns the source diagnostics, the pack's first problem, or
/// [`Error::TooManyIds`].
pub fn load(path: &Path, limits: &Limits) -> Result<Snapshot, Error> {
    if std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) {
        let pack = tilewright_pack::Pack::open(path).map_err(Error::Pack)?;
        pack.verify().map_err(Error::Pack)?;
        Snapshot::from_pack(&pack, limits)
    } else {
        let world = tilewright_source::load(path).map_err(Error::Source)?;
        Snapshot::from_world(&world, limits)
    }
}

#[cfg(test)]
mod tests;
