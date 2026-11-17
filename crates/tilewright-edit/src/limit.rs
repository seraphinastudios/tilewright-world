// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Which limit refused a transaction, beside the diagnostics that say so
//! in words.

use crate::{Budgets, limits};
use tilewright_jsonc::{ErrorCode, Limits};
use tilewright_source::Diagnostic;

/// Which limit a `tx.limit` diagnostic reports, as [`Refusal::limit`]
/// gives it. More kinds may be added; each has a stable name,
/// [`LimitKind::as_str`].
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LimitKind {
    /// The files one transaction touches, or the entries of its `base` or
    /// `result`: [`limits::FILES`].
    Files,
    /// The chunk files one terrain operation touches: [`limits::FILES`].
    TerrainChunks,
    /// The bytes read from the files a transaction touches: the read
    /// budget, [`Budgets::read_bytes`].
    ReadBytes,
    /// The size of one file: the format's limit on a document,
    /// [`Limits::max_input_bytes`] of the default [`Limits`].
    FileSize,
    /// The work of carrying a transaction out: the work budget,
    /// [`Budgets::work`].
    Work,
    /// The operations of an inverse: [`limits::OPERATIONS`].
    InverseOperations,
    /// The bytes of an inverse: the format's limit on a document.
    InverseBytes,
    /// A bound of the JSONC parser, met by a document read or edited.
    Document,
    /// The sealed transaction or its inverse would not be a transaction
    /// within the limits.
    Records,
    /// The diagnostics reported for one transaction: [`limits::DIAGNOSTICS`].
    Diagnostics,
    /// The operations of one transaction: [`limits::OPERATIONS`].
    Operations,
    /// The steps of one pointer: [`limits::POINTER`].
    PointerSteps,
    /// The terrain cells of one transaction: [`limits::CELLS`].
    Cells,
}

impl LimitKind {
    /// The kind's stable name: `files`, `terrain_chunks`, `read_bytes`,
    /// `file_size`, `work`, `inverse_operations`, `inverse_bytes`,
    /// `document`, `records`, `diagnostics`, `operations`,
    /// `pointer_steps` or `cells`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Files => "files",
            Self::TerrainChunks => "terrain_chunks",
            Self::ReadBytes => "read_bytes",
            Self::FileSize => "file_size",
            Self::Work => "work",
            Self::InverseOperations => "inverse_operations",
            Self::InverseBytes => "inverse_bytes",
            Self::Document => "document",
            Self::Records => "records",
            Self::Diagnostics => "diagnostics",
            Self::Operations => "operations",
            Self::PointerSteps => "pointer_steps",
            Self::Cells => "cells",
        }
    }
}

/// Where the bound of a [`Limit`] comes from.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LimitOrigin {
    /// A budget of the caller's [`Budgets`] tighter than the library's
    /// limit: under the default budgets the same transaction may plan.
    Budgets,
    /// A limit of the library or the format, which no [`Budgets`] widen,
    /// including the read and work budgets at their defaults.
    Library,
}

/// A limit that refused a transaction: its kind, the bound in force, and
/// where that bound comes from.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Limit {
    kind: LimitKind,
    bound: Option<u64>,
    origin: LimitOrigin,
}

/// `value` as a `u64`, saturating.
fn wide(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

impl Limit {
    /// Which limit it is.
    #[must_use]
    pub const fn kind(&self) -> LimitKind {
        self.kind
    }

    /// The bound in force, in the limit's unit (files, bytes, operations,
    /// steps, cells, diagnostics), the number its diagnostic names when it
    /// names one. `None` when no one number is the bound: a JSONC comment
    /// bound, which is a count and a size together, or a sealed form or
    /// inverse that would not be a transaction for a reason other than one
    /// limit.
    #[must_use]
    pub const fn bound(&self) -> Option<u64> {
        self.bound
    }

    /// Where the bound comes from.
    #[must_use]
    pub const fn origin(&self) -> LimitOrigin {
        self.origin
    }

    /// A limit of the library or the format.
    pub(crate) const fn library(kind: LimitKind, bound: Option<u64>) -> Self {
        Self {
            kind,
            bound,
            origin: LimitOrigin::Library,
        }
    }

    /// A limit of [`limits`], counted in `usize`.
    pub(crate) fn count(kind: LimitKind, bound: usize) -> Self {
        Self::library(kind, Some(wide(bound)))
    }

    /// The read budget of `budgets`.
    pub(crate) fn read_bytes(budgets: Budgets) -> Self {
        Self::budget(
            LimitKind::ReadBytes,
            budgets.read_bytes(),
            limits::READ_BYTES,
        )
    }

    /// The work budget of `budgets`.
    pub(crate) fn work(budgets: Budgets) -> Self {
        Self::budget(LimitKind::Work, budgets.work(), limits::WORK)
    }

    /// A budget at `bound`, the caller's when below the library's `limit`.
    const fn budget(kind: LimitKind, bound: u64, limit: u64) -> Self {
        Self {
            kind,
            bound: Some(bound),
            origin: if bound < limit {
                LimitOrigin::Budgets
            } else {
                LimitOrigin::Library
            },
        }
    }

    /// The format's limit on a document's size.
    pub(crate) fn document_size(kind: LimitKind) -> Self {
        Self::count(kind, Limits::default().max_input_bytes)
    }

    /// The parser bound that `code` reports, if it reports one.
    pub(crate) fn parser(code: ErrorCode) -> Option<Self> {
        let limits = Limits::default();
        let bound = match code {
            ErrorCode::InputTooLarge => Some(limits.max_input_bytes),
            ErrorCode::TooManyLines => Some(limits.max_lines),
            ErrorCode::LineTooLong => Some(limits.max_line_bytes),
            ErrorCode::NumberTooLong => Some(limits.max_number_bytes),
            ErrorCode::StringTooLong => Some(limits.max_string_bytes),
            ErrorCode::DepthLimit => Some(limits.max_depth),
            ErrorCode::ValueLimit => Some(limits.max_values),
            ErrorCode::ArrayLimit => Some(limits.max_array_items),
            ErrorCode::ObjectLimit => Some(limits.max_object_keys),
            // The comment bound is a count and a size together.
            ErrorCode::TooManyComments => None,
            _ => return None,
        };
        Some(Self::library(LimitKind::Document, bound.map(wide)))
    }
}

/// Why a transaction was refused: its diagnostics, as the functions that
/// return only diagnostics give them, and for each one at a limit
/// (`tx.limit`), which limit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Refusal {
    diagnostics: Vec<Diagnostic>,
    /// One for each diagnostic.
    limits: Vec<Option<Limit>>,
}

impl Refusal {
    /// The diagnostics, in order.
    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// The diagnostics, in order.
    #[must_use]
    pub fn into_diagnostics(self) -> Vec<Diagnostic> {
        self.diagnostics
    }

    /// The limit the diagnostic at `index` of [`Refusal::diagnostics`]
    /// reports: `Some` for each `tx.limit` diagnostic, `None` for every
    /// other one and past the end.
    #[must_use]
    pub fn limit(&self, index: usize) -> Option<Limit> {
        self.limits.get(index).copied().flatten()
    }

    /// Diagnostics paired with their limits.
    pub(crate) fn new(found: Vec<(Diagnostic, Option<Limit>)>) -> Self {
        let (diagnostics, limits) = found.into_iter().unzip();
        Self {
            diagnostics,
            limits,
        }
    }

    /// Diagnostics that report no limit.
    pub(crate) fn plain(diagnostics: Vec<Diagnostic>) -> Self {
        let limits = vec![None; diagnostics.len()];
        Self {
            diagnostics,
            limits,
        }
    }

    /// The diagnostics paired with their limits.
    pub(crate) fn into_pairs(self) -> Vec<(Diagnostic, Option<Limit>)> {
        self.diagnostics.into_iter().zip(self.limits).collect()
    }
}

/// Sorts `found` by diagnostic and drops repeats, as diagnostics alone are
/// sorted and deduplicated: equal diagnostics keep the first one's limit.
pub(crate) fn sort(found: &mut Vec<(Diagnostic, Option<Limit>)>) {
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found.dedup_by(|a, b| a.0 == b.0);
}
