// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Loads and validates a Tilewright world source tree
//! (`docs/spec/FORMAT.md`) into a typed, game-neutral [`World`].
//!
//! Loading is bounded and fail-closed: only the files the format defines are
//! read, symbolic links and unexpected files are errors, every document is
//! parsed as strict JSONC under the format's limits, and every problem is
//! reported as a [`Diagnostic`] with its file, byte span and a stable code.
//! A world is returned only when there are no diagnostics at all.
//!
//! The root path is resolved as the caller gives it and may be a link;
//! nothing beneath it is followed. On Unix every path component beneath the
//! root is opened through its parent directory's handle without following
//! symbolic links, so a link swapped into the tree during a load is never
//! followed. Other platforms check each component by
//! path before using it; that is a best effort which a local process changing
//! the tree during the load can race. Race-free loading is Unix-only.
//!
//! ```no_run
//! let world = tilewright_source::load(std::path::Path::new("content"))
//!     .map_err(|diagnostics| diagnostics.len())?;
//! println!("{} planes", world.planes().len());
//! # Ok::<(), usize>(())
//! ```

mod chunk;
mod defs;
mod diagnostic;
mod files;
mod plane;
mod profile;
mod read;
mod schema;

use std::{collections::BTreeMap, path::Path, sync::Arc};
use tilewright_jsonc::Limits;

use diagnostic::Diagnostics;
pub use diagnostic::{Code, Diagnostic};
pub use schema::{FieldSpec, FieldType, Schema};

/// Tiles along each side of a chunk in format 1.
pub const CHUNK_SIZE: u16 = 32;

/// Most ground layers a profile may declare.
pub const MAXIMUM_GROUND_LAYERS: usize = 8;

/// Bounds on what a load may read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoadOptions {
    /// Limits for every JSONC document. They can only tighten the format's
    /// limits: a value above [`Limits::default`] is lowered to it.
    pub limits: Limits,
    /// Most files read in one load.
    pub maximum_files: usize,
    /// Most directory entries listed in one load, counted before they are
    /// examined.
    pub maximum_entries: usize,
    /// Most bytes read in one load.
    pub maximum_total_bytes: u64,
    /// Most diagnostics kept; past this, one `source.budget` diagnostic
    /// notes that the rest were dropped.
    pub maximum_diagnostics: usize,
}

impl Default for LoadOptions {
    fn default() -> Self {
        Self {
            limits: Limits::default(),
            maximum_files: 500_000,
            maximum_entries: 1_000_000,
            maximum_total_bytes: 8 * 1024 * 1024 * 1024,
            maximum_diagnostics: 10_000,
        }
    }
}

/// A validated world source tree.
///
/// A `World` can only be produced by [`load`] or [`load_with`], so every one
/// has passed validation; its parts are read through accessors and cannot be
/// changed in place.
///
/// ```compile_fail
/// # fn planes(world: tilewright_source::World) {
/// let planes = world.planes; // the fields are private
/// # }
/// ```
#[derive(Clone, Debug, PartialEq)]
pub struct World {
    profile: Profile,
    planes: BTreeMap<String, Plane>,
    definitions: BTreeMap<(String, String), Definition>,
    source_digest: [u8; 32],
}

impl World {
    #[must_use]
    pub const fn profile(&self) -> &Profile {
        &self.profile
    }

    /// Planes by ID.
    #[must_use]
    pub const fn planes(&self) -> &BTreeMap<String, Plane> {
        &self.planes
    }

    /// Definitions keyed by `(type, id)`.
    #[must_use]
    pub const fn definitions(&self) -> &BTreeMap<(String, String), Definition> {
        &self.definitions
    }

    /// SHA-256 over every file read, as defined in `docs/spec/PACK.md`.
    #[must_use]
    pub const fn source_digest(&self) -> [u8; 32] {
        self.source_digest
    }
}

/// The game profile.
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    pub id: String,
    pub version: String,
    pub projection: Projection,
    pub render_layers: Vec<String>,
    /// Ground layers in profile order; each chunk stores one grid per entry.
    pub ground_layers: Vec<String>,
    /// Schemas by definition type. Roles that name the same schema path
    /// share one schema.
    pub definition_types: BTreeMap<String, Arc<Schema>>,
    pub object_fields: Arc<Schema>,
    pub zone_properties: Arc<Schema>,
    pub movement: Option<Movement>,
    /// Canonical JSON of the profile document.
    pub canonical: String,
}

impl Profile {
    /// The position of a render layer in the profile's order.
    #[must_use]
    pub fn render_layer_index(&self, name: &str) -> Option<usize> {
        self.render_layers.iter().position(|layer| layer == name)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Projection {
    Isometric { tile_width: u32, tile_height: u32 },
    TopDown { tile_size: u32 },
}

/// Movement analysis rules for the editor; the format does not interpret them.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Movement {
    pub max_step: u32,
    pub fall_unit: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaneKind {
    Shared,
    Instanced,
    PerPlayer,
}

impl PlaneKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Instanced => "instanced",
            Self::PerPlayer => "per_player",
        }
    }
}

/// One plane with its zones, anchors and chunks.
#[derive(Clone, Debug, PartialEq)]
pub struct Plane {
    pub id: String,
    pub title: Option<String>,
    pub kind: PlaneKind,
    pub chunk_size: u16,
    pub default_zone: Option<String>,
    /// Canonical JSON of `plane.jsonc`.
    pub canonical: String,
    pub zones: BTreeMap<String, Zone>,
    pub anchors: BTreeMap<String, Anchor>,
    /// Chunks keyed by `(layer, cx, cy)`.
    pub chunks: BTreeMap<(i32, i64, i64), Chunk>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Zone {
    pub id: String,
    pub parent: Option<String>,
    pub name: Option<String>,
    /// Canonical JSON of the zone entry.
    pub canonical: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Anchor {
    pub layer: i32,
    pub at: [i64; 2],
}

/// A character grid: one palette value per tile.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Grid {
    /// Values in palette declaration order.
    pub palette: Vec<String>,
    /// `chunk_size²` cells, row-major; a cell is a palette index plus one,
    /// or 0 for an empty tile.
    pub cells: Vec<u16>,
}

impl Grid {
    /// The value of a cell, or `None` for an empty tile.
    #[must_use]
    pub fn value(&self, index: usize) -> Option<&str> {
        let cell = usize::from(*self.cells.get(index)?);
        cell.checked_sub(1)
            .and_then(|palette| self.palette.get(palette))
            .map(String::as_str)
    }
}

/// One chunk of one layer.
#[derive(Clone, Debug, PartialEq)]
pub struct Chunk {
    /// Source path, relative to the root.
    pub path: String,
    /// One grid per profile ground layer, in profile order.
    pub ground: Vec<Grid>,
    /// `chunk_size²` tile heights, row-major.
    pub heights: Vec<i32>,
    /// Zone per tile; empty cells take the plane's default zone.
    pub zones: Grid,
    pub objects: Vec<Object>,
}

/// A placed object or an inventory entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Object {
    /// Chunk-local tile; `None` for inventory entries.
    pub at: Option<[u16; 2]>,
    pub proto: String,
    pub layer: Option<String>,
    pub sub_layer: i16,
    pub z: i32,
    pub fields: Vec<(String, FieldValue)>,
    pub portal: Option<Portal>,
    pub inventory: Vec<Object>,
    /// Canonical JSON of the `custom` object.
    pub custom: Option<String>,
}

/// A field value. Arrays and objects are kept as canonical JSON.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldValue {
    Null,
    Bool(bool),
    Integer(i64),
    Number(f64),
    String(String),
    Json(String),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Portal {
    Location {
        plane: Option<String>,
        layer: i32,
        at: [i64; 2],
    },
    Anchor {
        plane: Option<String>,
        anchor: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Definition {
    pub fields: Vec<(String, FieldValue)>,
    /// Canonical JSON of the definition document.
    pub canonical: String,
}

/// Loads a source tree with default bounds.
///
/// # Errors
/// Returns every diagnostic found, sorted by path and position.
pub fn load(root: &Path) -> Result<World, Vec<Diagnostic>> {
    load_with(root, &LoadOptions::default())
}

/// Loads a source tree.
///
/// # Errors
/// Returns every diagnostic found, sorted by path and position.
pub fn load_with(root: &Path, options: &LoadOptions) -> Result<World, Vec<Diagnostic>> {
    let options = &LoadOptions {
        limits: within_format(options.limits),
        ..*options
    };
    let mut tree = files::Tree::new(root, options);
    let mut diagnostics = Diagnostics::new(options.maximum_diagnostics);
    let world = load_tree(&mut tree, &mut diagnostics);
    diagnostics.append(tree.take_diagnostics());
    match world {
        Some(world) if diagnostics.is_empty() => Ok(World {
            source_digest: tree.digest(),
            ..world
        }),
        _ => Err(diagnostics.finish()),
    }
}

/// `limits`, each lowered to the format's own limit where it is higher.
fn within_format(limits: Limits) -> Limits {
    let format = Limits::default();
    Limits {
        max_input_bytes: limits.max_input_bytes.min(format.max_input_bytes),
        max_depth: limits.max_depth.min(format.max_depth),
        max_values: limits.max_values.min(format.max_values),
        max_array_items: limits.max_array_items.min(format.max_array_items),
        max_object_keys: limits.max_object_keys.min(format.max_object_keys),
        max_string_bytes: limits.max_string_bytes.min(format.max_string_bytes),
        max_number_bytes: limits.max_number_bytes.min(format.max_number_bytes),
        max_comments: limits.max_comments.min(format.max_comments),
        max_comment_bytes: limits.max_comment_bytes.min(format.max_comment_bytes),
        max_lines: limits.max_lines.min(format.max_lines),
        max_line_bytes: limits.max_line_bytes.min(format.max_line_bytes),
    }
}

fn load_tree(tree: &mut files::Tree, diagnostics: &mut Diagnostics) -> Option<World> {
    let (manifest_path, plane_ids) = profile::read_manifest(tree, diagnostics)?;
    let profile = profile::read_profile(tree, &manifest_path, diagnostics)?;
    let mut references = schema::References::new(&profile);
    let definitions = defs::read_definitions(tree, &profile, &mut references, diagnostics);
    let mut planes = BTreeMap::new();
    for plane in plane::read_planes(tree, &profile, &plane_ids, &mut references, diagnostics) {
        planes.insert(plane.id.clone(), plane);
    }
    Some(World {
        profile,
        planes,
        definitions,
        source_digest: [0; 32],
    })
}

#[cfg(test)]
mod tests;
