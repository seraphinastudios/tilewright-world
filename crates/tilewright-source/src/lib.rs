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
//! A profile's terrain transitions ([`transitions`]) derive edge, corner and
//! variant tiles from painted terrain; [`Transitions::resolve_chunk`] is the
//! one resolver the compiler, editors and other tools share.
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

mod access;
mod chunk;
mod defs;
mod diagnostic;
mod files;
mod plane;
mod profile;
mod read;
mod schema;
mod stairwell;
pub mod transitions;
mod uid;

use std::{collections::BTreeMap, path::Path, sync::Arc};
use tilewright_jsonc::Limits;

pub use access::{
    Access, AccessError, Audience, LOCK_NAME, SourceDirectory, SourceLock, SourceRoot,
};
use diagnostic::Diagnostics;
pub use diagnostic::{Code, Diagnostic};
pub use files::is_jsonc_path;
pub use read::{is_identifier, is_namespace};
pub use schema::{FieldSpec, FieldType, MAXIMUM_FIELD_DEPTH, MAXIMUM_LIST_ITEMS, Schema};
pub use transitions::{ResolvedChunk, TerrainSource, Transitions};

/// Tiles along each side of a chunk in format 1.
pub const CHUNK_SIZE: u16 = 32;

/// Most ground layers a profile may declare.
pub const MAXIMUM_GROUND_LAYERS: usize = 8;

/// Largest `layer_height` a profile may declare. A stacked elevation,
/// `layer × layer_height + height + z`, then stays within the format's
/// integer range for every 32-bit layer, height and `z`.
pub const MAXIMUM_LAYER_HEIGHT: u32 = 65_535;

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
    /// Most diagnostics kept, at least one; past this, one `source.budget`
    /// diagnostic notes that the rest were dropped.
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
    object_uids: BTreeMap<String, ObjectLocation>,
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

    /// Every object uid of the world, with where its object is. Uids are
    /// unique in the whole world, so a uid alone names one object.
    #[must_use]
    pub const fn object_uids(&self) -> &BTreeMap<String, ObjectLocation> {
        &self.object_uids
    }

    /// Where the object with `uid` is.
    #[must_use]
    pub fn object_location(&self, uid: &str) -> Option<&ObjectLocation> {
        self.object_uids.get(uid)
    }

    /// The object with `uid`.
    #[must_use]
    pub fn object(&self, uid: &str) -> Option<&Object> {
        let location = self.object_uids.get(uid)?;
        self.planes
            .get(&location.plane)?
            .chunks
            .get(&location.chunk)?
            .object(&location.path)
    }
}

/// Where an object is: its chunk, and its place in the chunk.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ObjectLocation {
    pub plane: String,
    /// The chunk's key in [`Plane::chunks`]: `(layer, cx, cy)`.
    pub chunk: (i32, i64, i64),
    /// The object's index in the chunk's `objects`, then, for an inventory
    /// entry, its index in the inventory of each object down to it. A
    /// top-level object's path has one index.
    pub path: Vec<usize>,
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
    /// Height units per layer: layer `L` sits at `L × layer_height`, and
    /// tile heights stay relative to their layer. `None` when the profile
    /// does not stack layers.
    pub layer_height: Option<u32>,
    pub movement: Option<Movement>,
    /// The terrain transitions the profile names (`docs/spec/FORMAT.md`
    /// section 13), which derive edge, corner and variant tiles from
    /// painted terrain.
    pub transitions: Option<Arc<Transitions>>,
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

/// One plane with its zones, anchors, stairwells and chunks.
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
    pub stairwells: BTreeMap<String, Stairwell>,
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

/// One exit spanning several layers: steps on adjacent layers, used end to
/// end in one move.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Stairwell {
    pub id: String,
    /// Steps in source order, at least two, each on the layer next to the
    /// one before it, in one direction, and at most one tile away on each
    /// axis.
    pub steps: Vec<StairwellStep>,
    pub direction: StairwellDirection,
    /// Whether consumers draw only the step on the viewer's layer.
    pub hidden_on_other_layers: bool,
    /// Canonical JSON of the `custom` object.
    pub custom: Option<String>,
    /// Canonical JSON of the stairwell entry.
    pub canonical: String,
}

impl Stairwell {
    /// Whether `steps` form a stairwell: at least two, each on the layer
    /// next to the one before it, all going up or all going down, and at
    /// most one tile away from the step before it on each axis.
    #[must_use]
    pub fn connects(steps: &[StairwellStep]) -> bool {
        let [first, second, ..] = steps else {
            return false;
        };
        let up = second.layer > first.layer;
        steps.windows(2).all(|pair| pair[0].leads_to(&pair[1], up))
    }

    /// The step on the lowest layer.
    #[must_use]
    pub fn lower_end(&self) -> Option<&StairwellStep> {
        self.steps.iter().min_by_key(|step| step.layer)
    }

    /// The step on the highest layer.
    #[must_use]
    pub fn upper_end(&self) -> Option<&StairwellStep> {
        self.steps.iter().max_by_key(|step| step.layer)
    }
}

/// One step of a stairwell, in plane coordinates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StairwellStep {
    pub layer: i32,
    pub at: [i64; 2],
    pub proto: String,
}

impl StairwellStep {
    /// Whether `next` may follow this step in a stairwell that goes `up`
    /// (or down): it is on the next layer that way and at most one tile
    /// away on each axis.
    #[must_use]
    pub fn leads_to(&self, next: &Self, up: bool) -> bool {
        adjacent((self.layer, self.at), (next.layer, next.at), up)
    }
}

/// Whether a step at position `next` (layer and plane coordinates) may
/// follow one at `before` in a stairwell that goes `up` (or down).
pub(crate) fn adjacent(before: (i32, [i64; 2]), next: (i32, [i64; 2]), up: bool) -> bool {
    let change = if up { 1 } else { -1 };
    i64::from(next.0) - i64::from(before.0) == change
        && before
            .1
            .iter()
            .zip(next.1)
            .all(|(before, after)| (i128::from(*before) - i128::from(after)).abs() <= 1)
}

/// Which way a stairwell may be used.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum StairwellDirection {
    /// From either end to the other.
    #[default]
    Both,
    /// Only from the upper end to the lower end.
    Down,
    /// Only from the lower end to the upper end.
    Up,
}

impl StairwellDirection {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Both => "both",
            Self::Down => "down",
            Self::Up => "up",
        }
    }

    /// The direction a source or pack spells `text`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "both" => Some(Self::Both),
            "down" => Some(Self::Down),
            "up" => Some(Self::Up),
            _ => None,
        }
    }
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

impl Chunk {
    /// The object at `path`: an index into `objects`, then one into the
    /// inventory of each object down to it, as in [`ObjectLocation::path`].
    #[must_use]
    pub fn object(&self, path: &[usize]) -> Option<&Object> {
        let (first, rest) = path.split_first()?;
        rest.iter()
            .try_fold(self.objects.get(*first)?, |object, index| {
                object.inventory.get(*index)
            })
    }

    /// Every object of the chunk, inventory entries included, in
    /// depth-first order: each object comes before its inventory entries,
    /// and those before the object after it. An object's position in this
    /// order is its index in the chunk (`docs/spec/FORMAT.md` section 7).
    #[must_use]
    pub fn depth_first(&self) -> DepthFirst<'_> {
        DepthFirst {
            levels: vec![self.objects.iter()],
        }
    }
}

/// The objects of a chunk in depth-first order; see [`Chunk::depth_first`].
#[derive(Clone, Debug)]
pub struct DepthFirst<'c> {
    /// The objects left at each depth, outermost first.
    levels: Vec<std::slice::Iter<'c, Object>>,
}

impl<'c> Iterator for DepthFirst<'c> {
    type Item = &'c Object;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            let level = self.levels.last_mut()?;
            if let Some(object) = level.next() {
                self.levels.push(object.inventory.iter());
                return Some(object);
            }
            self.levels.pop();
        }
    }
}

/// A placed object or an inventory entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Object {
    /// The object's stable ID, unique in the world.
    pub uid: Option<String>,
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

/// A field value. Arrays and objects, the values of `json`, `list` and
/// `object` fields, are kept as canonical JSON.
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
    load_edited(root, options, &Overlay::new())
}

/// Edits applied over the files of a tree for [`load_edited`], keyed by
/// root-relative `/`-separated path: `Some(text)` replaces or creates the
/// file, and `None` removes it.
pub type Overlay = BTreeMap<String, Option<String>>;

/// Loads a source tree as it would be with `overlay` applied, without
/// writing anything. Overlay files are read under the same limits and
/// budget as files on disk, and a directory that holds one exists. An
/// overlay of more entries than `maximum_entries` is refused before
/// anything is read.
///
/// # Errors
/// Returns every diagnostic found, sorted by path and position.
pub fn load_edited(
    root: &Path,
    options: &LoadOptions,
    overlay: &Overlay,
) -> Result<World, Vec<Diagnostic>> {
    load_edited_in(&SourceRoot::new(root), options, overlay)
}

/// Loads the tree of an opened root as [`load_edited`] does, through the
/// handle rather than its path, so that what is validated is the directory
/// the caller opened.
///
/// # Errors
/// Returns every diagnostic found, sorted by path and position.
pub fn load_edited_in(
    root: &SourceRoot,
    options: &LoadOptions,
    overlay: &Overlay,
) -> Result<World, Vec<Diagnostic>> {
    // At least one diagnostic is kept, so a failed load always says why.
    let options = &LoadOptions {
        limits: within_format(options.limits),
        maximum_diagnostics: options.maximum_diagnostics.max(1),
        ..*options
    };
    // Each entry adds, replaces or removes a directory entry; more than a
    // load may list cannot load, and is not looked at.
    if overlay.len() > options.maximum_entries {
        return Err(vec![Diagnostic::file(
            ".",
            Code::Budget,
            format!(
                "the edits hold more than {} entries",
                options.maximum_entries
            ),
        )]);
    }
    // Every key must be a path a file could have, so no entry costs more
    // than a path does.
    if let Some(key) = overlay
        .keys()
        .find(|key| key.is_empty() || access::checked(key).is_err())
    {
        let shown = if key.len() <= 128 {
            format!("{key:?}")
        } else {
            format!("a path of {} bytes", key.len())
        };
        return Err(vec![Diagnostic::file(
            ".",
            Code::Path,
            format!("the edits name {shown}, which is not a path a file of the tree can have"),
        )]);
    }
    let mut diagnostics = Diagnostics::new(options.maximum_diagnostics);
    let mut tree = files::Tree::in_root(root.platform(), options, overlay, diagnostics.total());
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
    let (mut profile, transitions_path) = profile::read_profile(tree, &manifest_path, diagnostics)?;
    let mut references = schema::References::new(&profile);
    let definitions = defs::read_definitions(tree, &profile, &mut references, diagnostics);
    // A transitions document that fails has reported why, so the load
    // fails with it.
    if let Some(path) = transitions_path {
        profile.transitions =
            transitions::read_transitions(tree, &path, &profile, &references, diagnostics)
                .map(Arc::new);
    }
    let mut planes = BTreeMap::new();
    for plane in plane::read_planes(tree, &profile, &plane_ids, &mut references, diagnostics) {
        planes.insert(plane.id.clone(), plane);
    }
    let object_uids = uid::index(&planes);
    Some(World {
        profile,
        planes,
        definitions,
        object_uids,
        source_digest: [0; 32],
    })
}

#[cfg(test)]
mod tests;
