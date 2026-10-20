// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Terrain transitions (`docs/spec/FORMAT.md` section 13): the profile's
//! rules that choose the edge, corner and variant tiles around painted
//! terrain, and the deterministic resolver that applies them.
//!
//! A world's source stores only the terrain a brush paints.
//! [`Transitions::resolve_chunk`] derives a chunk's ground tiles from its
//! terrain and its neighbours', across chunk edges, so the compiler, the
//! editor and every other tool draw the same tiles.

mod read;
mod resolve;

pub(crate) use read::read_transitions;
pub use resolve::{ResolvedChunk, TerrainSource, variant_hash};

use std::{collections::BTreeMap, ops::Range};

/// The `format` of a transitions document.
pub const TRANSITIONS_FORMAT: &str = "tilewright.transitions/1";

/// Most terrains a transitions document declares.
pub const MAXIMUM_TERRAINS: usize = 1_024;

/// Most prototypes one terrain lists.
pub const MAXIMUM_TERRAIN_PROTOS: usize = 1_024;

/// Most transitions one terrain has.
pub const MAXIMUM_TERRAIN_TRANSITIONS: usize = 8;

/// Most transitions a transitions document has in all.
pub const MAXIMUM_TRANSITIONS: usize = 256;

/// Most transitions resolving one tile evaluates, however large the
/// document: the transitions of the tile's own terrain and of the terrains
/// of its eight neighbours, since an outside transition draws only next to
/// its terrain.
pub const MAXIMUM_TILE_TRANSITIONS: usize = 9 * MAXIMUM_TERRAIN_TRANSITIONS;

/// Most variants one pattern lists.
pub const MAXIMUM_VARIANTS: usize = 256;

/// Largest weight of a variant.
pub const MAXIMUM_WEIGHT: u32 = 65_535;

/// Where a transition draws.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Draw {
    /// On tiles of the transition's own terrain, showing where their
    /// neighbours differ.
    Inside,
    /// On tiles of the terrains it blends with, showing its terrain
    /// reaching in from their neighbours.
    Outside,
}

impl Draw {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inside => "inside",
            Self::Outside => "outside",
        }
    }

    /// The draw a document spells `text`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "inside" => Some(Self::Inside),
            "outside" => Some(Self::Outside),
            _ => None,
        }
    }
}

/// The neighbourhood a transition reads, and the patterns it maps to tiles.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Model {
    /// No neighbours: every tile has pattern 0. Inside only; for variants.
    None,
    /// The four sides, N 1, E 2, S 4, W 8: 16 patterns.
    Sides,
    /// The four corners, NE 1, SE 2, SW 4, NW 8: the 16 patterns of a
    /// corner (Wang or marching-squares) set.
    Corners,
    /// Sides and corners, N 1, NE 2, E 4, SE 8, S 16, SW 32, W 64, NW 128,
    /// with each corner counted only where its sides allow: 47 patterns.
    Blob,
}

/// A side bit of the blob model and the corners next to it, clockwise from
/// north: `(corner, side before it, side after it)`.
const BLOB_CORNERS: [(u8, u8, u8); 4] = [(2, 1, 4), (8, 4, 16), (32, 16, 64), (128, 64, 1)];

impl Model {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Sides => "sides",
            Self::Corners => "corners",
            Self::Blob => "blob",
        }
    }

    /// The model a document spells `text`.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "none" => Some(Self::None),
            "sides" => Some(Self::Sides),
            "corners" => Some(Self::Corners),
            "blob" => Some(Self::Blob),
            _ => None,
        }
    }

    /// The pattern of a tile whose neighbours, N, NE, E, SE, S, SW, W and
    /// NW, show the transition's terrain where `shows` is `true`.
    #[must_use]
    pub fn pattern(self, draw: Draw, shows: [bool; 8]) -> u8 {
        let [n, ne, e, se, s, sw, w, nw] = shows;
        let bit = |on: bool, value: u8| if on { value } else { 0 };
        match self {
            Self::None => 0,
            Self::Sides => bit(n, 1) | bit(e, 2) | bit(s, 4) | bit(w, 8),
            Self::Corners => {
                // Inside, a corner shows the terrain where everything
                // around it does; outside, where anything does.
                let corner = |a: bool, diagonal: bool, b: bool| match draw {
                    Draw::Inside => a && diagonal && b,
                    Draw::Outside => a || diagonal || b,
                };
                bit(corner(n, ne, e), 1)
                    | bit(corner(e, se, s), 2)
                    | bit(corner(s, sw, w), 4)
                    | bit(corner(w, nw, n), 8)
            }
            Self::Blob => {
                // Inside, a corner counts only where both its sides
                // continue; outside, only where neither side already
                // draws the edge over it.
                let corner = |a: bool, diagonal: bool, b: bool| match draw {
                    Draw::Inside => a && diagonal && b,
                    Draw::Outside => diagonal && !a && !b,
                };
                bit(n, 1)
                    | bit(corner(n, ne, e), 2)
                    | bit(e, 4)
                    | bit(corner(e, se, s), 8)
                    | bit(s, 16)
                    | bit(corner(s, sw, w), 32)
                    | bit(w, 64)
                    | bit(corner(w, nw, n), 128)
            }
        }
    }

    /// Whether this model produces `pattern` when drawing `draw`: the keys
    /// a transition's `tiles` may have. An outside transition never draws
    /// pattern 0, where no neighbour shows its terrain.
    #[must_use]
    pub fn produces(self, draw: Draw, pattern: u8) -> bool {
        if draw == Draw::Outside && pattern == 0 {
            return false;
        }
        match self {
            Self::None => pattern == 0,
            Self::Sides | Self::Corners => pattern < 16,
            Self::Blob => BLOB_CORNERS.iter().all(|&(corner, a, b)| {
                pattern & corner == 0
                    || match draw {
                        Draw::Inside => pattern & a != 0 && pattern & b != 0,
                        Draw::Outside => pattern & a == 0 && pattern & b == 0,
                    }
            }),
        }
    }
}

/// One variant of a pattern's tile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Variant {
    /// A `proto` definition.
    pub proto: String,
    /// From 1 to [`MAXIMUM_WEIGHT`].
    pub weight: u32,
}

/// The variants of one pattern, at least one, with the running sum of their
/// weights.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Pattern {
    variants: Vec<Variant>,
    /// For each variant, the sum of its weight and those before it; the
    /// last is the total.
    cumulative: Vec<u64>,
}

impl Pattern {
    /// The variants in document order.
    #[must_use]
    pub fn variants(&self) -> &[Variant] {
        &self.variants
    }

    pub(crate) fn new(variants: Vec<Variant>) -> Self {
        let cumulative = variants
            .iter()
            .scan(0_u64, |sum, variant| {
                *sum += u64::from(variant.weight);
                Some(*sum)
            })
            .collect();
        Self {
            variants,
            cumulative,
        }
    }

    /// The variant a tile with variant hash `hash` draws: the first at
    /// which the running sum of weights exceeds `hash` modulo their total.
    /// A binary search finds it, so a pick costs time in proportion to the
    /// logarithm of the number of variants.
    #[must_use]
    pub fn pick(&self, hash: u64) -> &str {
        let total = self.cumulative.last().copied().unwrap_or(0).max(1);
        let point = hash % total;
        let index = self.cumulative.partition_point(|sum| *sum <= point);
        // A pattern always has a variant, and the point is below the total.
        self.variants
            .get(index)
            .or_else(|| self.variants.last())
            .map_or("", |variant| variant.proto.as_str())
    }
}

/// One transition of a terrain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Transition {
    terrain: usize,
    draw: Draw,
    model: Model,
    layer: usize,
    /// Whether it blends with each terrain, by index.
    blends: Vec<bool>,
    seed: u32,
    tiles: BTreeMap<u8, Pattern>,
    /// For each neighbourhood, the 8 bits of which neighbours show the
    /// terrain (N 1, NE 2, ... NW 128), the pattern it draws, if `tiles`
    /// has one for it: resolving a tile looks a transition up here instead
    /// of computing its pattern.
    draws: Box<[Option<u8>; 256]>,
}

impl Transition {
    pub(crate) fn new(
        (terrain, draw, model, layer): (usize, Draw, Model, usize),
        blends: Vec<bool>,
        seed: u32,
        tiles: BTreeMap<u8, Pattern>,
    ) -> Self {
        let mut draws = Box::new([None; 256]);
        for (bits, entry) in (0..=255_u8).zip(draws.iter_mut()) {
            let shows = std::array::from_fn(|bit| bits & (1 << bit) != 0);
            let pattern = model.pattern(draw, shows);
            if (draw == Draw::Inside || pattern != 0) && tiles.contains_key(&pattern) {
                *entry = Some(pattern);
            }
        }
        Self {
            terrain,
            draw,
            model,
            layer,
            blends,
            seed,
            tiles,
            draws,
        }
    }

    /// The tile this transition draws on a tile whose neighbours, as bits
    /// N 1, NE 2, ... NW 128, show its terrain where set, for variant hash
    /// `hash`; `None` where its table has no tile for the pattern.
    #[must_use]
    pub fn draw_at(&self, shows: u8, hash: u64) -> Option<&str> {
        let pattern = self.draws.get(usize::from(shows)).copied().flatten()?;
        Some(self.tiles.get(&pattern)?.pick(hash))
    }

    /// Whether this transition draws anything on a tile whose neighbours
    /// show its terrain as the bits of `shows` do.
    #[must_use]
    pub fn draws(&self, shows: u8) -> bool {
        self.draws
            .get(usize::from(shows))
            .is_some_and(Option::is_some)
    }

    /// The index of its terrain in [`Transitions::terrains`].
    #[must_use]
    pub const fn terrain(&self) -> usize {
        self.terrain
    }

    #[must_use]
    pub const fn draw(&self) -> Draw {
        self.draw
    }

    #[must_use]
    pub const fn model(&self) -> Model {
        self.model
    }

    /// The index in the profile's ground layers of the layer its tiles go
    /// into.
    #[must_use]
    pub const fn layer(&self) -> usize {
        self.layer
    }

    /// Whether it blends with the terrain at index `terrain`: the terrains
    /// an outside transition draws onto, or the neighbours an inside one
    /// draws an edge against.
    #[must_use]
    pub fn blends_with(&self, terrain: usize) -> bool {
        self.blends.get(terrain).copied().unwrap_or(false)
    }

    #[must_use]
    pub const fn seed(&self) -> u32 {
        self.seed
    }

    /// The tile of each pattern it draws.
    #[must_use]
    pub const fn tiles(&self) -> &BTreeMap<u8, Pattern> {
        &self.tiles
    }
}

/// One terrain: the prototypes that are it when painted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Terrain {
    pub id: String,
    /// At least one `proto` definition; a brush paints the first.
    pub protos: Vec<String>,
    /// Its transitions, as indexes into [`Transitions::transitions`].
    pub transitions: Range<usize>,
}

/// A profile's terrain transitions, as read and checked by the loader.
///
/// Every `Transitions` comes from a load, so its terrains, prototypes,
/// layers and patterns are consistent; its parts are read through
/// accessors.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Transitions {
    /// Source path of the document, relative to the root.
    path: String,
    terrain_layer: usize,
    ground_layers: usize,
    terrains: Vec<Terrain>,
    /// Every transition, in the order they run.
    list: Vec<Transition>,
    /// The terrain of each member prototype.
    by_proto: BTreeMap<String, usize>,
}

impl Transitions {
    /// Source path of the document, relative to the root.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The index in the profile's ground layers of the layer that holds
    /// painted terrain.
    #[must_use]
    pub const fn terrain_layer(&self) -> usize {
        self.terrain_layer
    }

    /// How many ground layers the profile has: the grids a resolved chunk
    /// holds.
    #[must_use]
    pub const fn ground_layers(&self) -> usize {
        self.ground_layers
    }

    /// Terrains from lowest to highest precedence.
    #[must_use]
    pub fn terrains(&self) -> &[Terrain] {
        &self.terrains
    }

    /// Every transition, in the order they run: terrains in order, each
    /// terrain's transitions in order.
    #[must_use]
    pub fn transitions(&self) -> &[Transition] {
        &self.list
    }

    /// The index of the terrain `id`.
    #[must_use]
    pub fn terrain(&self, id: &str) -> Option<usize> {
        self.terrains.iter().position(|terrain| terrain.id == id)
    }

    /// The index of the terrain `proto` paints, if it paints one.
    #[must_use]
    pub fn terrain_of(&self, proto: &str) -> Option<usize> {
        self.by_proto.get(proto).copied()
    }

    /// Every prototype the document names, with repeats: the terrains'
    /// prototypes and every variant a transition may draw. Resolved tiles
    /// hold only these and the source's own prototypes.
    pub fn protos(&self) -> impl Iterator<Item = &str> + '_ {
        let members = self
            .terrains
            .iter()
            .flat_map(|terrain| terrain.protos.iter().map(String::as_str));
        let tiles = self.list.iter().flat_map(|transition| {
            transition.tiles.values().flat_map(|pattern| {
                pattern
                    .variants
                    .iter()
                    .map(|variant| variant.proto.as_str())
            })
        });
        members.chain(tiles)
    }

    /// The prototype a brush writes into the terrain layer to paint the
    /// terrain at index `terrain`: its first.
    #[must_use]
    pub fn paint(&self, terrain: usize) -> Option<&str> {
        self.terrains
            .get(terrain)?
            .protos
            .first()
            .map(String::as_str)
    }
}

#[cfg(test)]
mod tests;
