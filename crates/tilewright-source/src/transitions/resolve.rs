// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Resolving transitions: a chunk's ground tiles from painted terrain.

use super::{Draw, Transitions};
use crate::{CHUNK_SIZE, Grid, Plane};
use std::collections::BTreeMap;

/// Tiles along a chunk's side.
const SIDE: usize = CHUNK_SIZE as usize;

/// Tiles in a chunk.
const CELLS: usize = SIDE * SIDE;

/// Tiles along the side of a chunk with a border of one tile around it.
const WINDOW: usize = SIDE + 2;

/// Where the resolver reads painted terrain: the ground grids of chunks by
/// address. A [`Plane`] is one; an editor can implement it over its own
/// chunks, such as ones with unsaved edits.
pub trait TerrainSource {
    /// The ground grids of the chunk at `(layer, cx, cy)`, one per profile
    /// ground layer in order, or `None` where there is no chunk.
    fn chunk_ground(&self, layer: i32, cx: i64, cy: i64) -> Option<&[Grid]>;
}

impl TerrainSource for Plane {
    fn chunk_ground(&self, layer: i32, cx: i64, cy: i64) -> Option<&[Grid]> {
        self.chunks
            .get(&(layer, cx, cy))
            .map(|chunk| chunk.ground.as_slice())
    }
}

/// The output function of `SplitMix64`.
const fn mix(value: u64) -> u64 {
    let value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    let value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

/// The hash that picks a tile's variant (`docs/spec/FORMAT.md` section 13):
/// the transition's seed and the tile's layer and plane coordinates, mixed
/// in that order, so the same world always picks the same variants.
#[must_use]
pub fn variant_hash(seed: u32, layer: i32, x: i64, y: i64) -> u64 {
    let hash = mix(u64::from(seed));
    let hash = mix(hash ^ i64::from(layer).cast_unsigned());
    let hash = mix(hash ^ x.cast_unsigned());
    mix(hash ^ y.cast_unsigned())
}

/// A chunk's ground tiles with transitions applied: for each ground layer,
/// each tile's prototype, or `None` for an empty tile. Values borrow from
/// the source grids and the transitions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedChunk<'a> {
    /// One list of `chunk_size²` tiles per ground layer, row-major.
    layers: Vec<Vec<Option<&'a str>>>,
}

impl<'a> ResolvedChunk<'a> {
    /// The prototype at row-major tile `index` of ground layer `ground`.
    #[must_use]
    pub fn value(&self, ground: usize, index: usize) -> Option<&'a str> {
        self.layers.get(ground)?.get(index).copied().flatten()
    }

    /// The prototype at chunk-local `[x, y]` of ground layer `ground`.
    #[must_use]
    pub fn at(&self, ground: usize, [x, y]: [u16; 2]) -> Option<&'a str> {
        let (x, y) = (usize::from(x), usize::from(y));
        if x < SIDE && y < SIDE {
            self.value(ground, y * SIDE + x)
        } else {
            None
        }
    }

    /// How many ground layers it holds.
    #[must_use]
    pub fn ground_layers(&self) -> usize {
        self.layers.len()
    }

    /// Every prototype a tile of any ground layer holds, with repeats.
    pub fn values(&self) -> impl Iterator<Item = &'a str> + '_ {
        self.layers
            .iter()
            .flat_map(|tiles| tiles.iter().filter_map(|tile| *tile))
    }

    /// The tiles as grids, one per ground layer: each palette lists the
    /// values its tiles use in the order they first appear, row-major.
    #[must_use]
    pub fn grids(&self) -> Vec<Grid> {
        self.layers
            .iter()
            .map(|tiles| {
                let mut indexes: BTreeMap<&str, u16> = BTreeMap::new();
                let mut palette = Vec::new();
                let cells = tiles
                    .iter()
                    .map(|tile| {
                        tile.map_or(0, |value| {
                            *indexes.entry(value).or_insert_with(|| {
                                palette.push(value.to_owned());
                                // A chunk has fewer tiles than u16 values.
                                u16::try_from(palette.len()).unwrap_or(u16::MAX)
                            })
                        })
                    })
                    .collect();
                Grid { palette, cells }
            })
            .collect()
    }
}

/// The neighbours that satisfy `test`, as bits N 1, NE 2, ... NW 128.
fn neighbour_bits(neighbours: &[Option<usize>; 8], test: impl Fn(Option<usize>) -> bool) -> u8 {
    let mut bits = 0;
    for (bit, neighbour) in neighbours.iter().enumerate() {
        if test(*neighbour) {
            bits |= 1 << bit;
        }
    }
    bits
}

/// The value of a grid's cell, or `None` for an empty one or one the grid
/// does not have.
fn cell(grid: Option<&Grid>, index: usize) -> Option<&str> {
    grid?.value(index)
}

impl Transitions {
    /// Resolves the chunk at `(layer, cx, cy)` of `source`: its ground
    /// tiles, one list per profile ground layer, with the transitions
    /// applied as `docs/spec/FORMAT.md` section 13 defines.
    ///
    /// It reads the chunk's grids and the edge tiles of its eight
    /// neighbours' terrain grids, so tiles along chunk edges agree across
    /// them. A chunk the source does not have resolves to empty tiles. The
    /// result depends only on the source's grids and the transitions, so it
    /// is the same for every caller. A tile looks up only the transitions
    /// of its own terrain and of its neighbours' terrains, at most
    /// [`MAXIMUM_TILE_TRANSITIONS`](super::MAXIMUM_TILE_TRANSITIONS), each
    /// in a table of its neighbourhoods, so the cost is linear in the
    /// chunk's tiles however large the document.
    #[must_use]
    pub fn resolve_chunk<'a, S>(
        &'a self,
        source: &'a S,
        layer: i32,
        cx: i64,
        cy: i64,
    ) -> ResolvedChunk<'a>
    where
        S: TerrainSource + ?Sized,
    {
        let own = source.chunk_ground(layer, cx, cy).unwrap_or(&[]);
        let mut layers: Vec<Vec<Option<&'a str>>> = (0..self.ground_layers)
            .map(|ground| {
                (0..CELLS)
                    .map(|index| cell(own.get(ground), index))
                    .collect()
            })
            .collect();
        let window = self.window(source, layer, cx, cy);
        let terrain_at = |x: usize, y: usize| window.get(y * WINDOW + x).copied().flatten();
        let mut drawn: Vec<Option<(usize, &'a str)>> = vec![None; self.ground_layers];
        // Plane coordinates of the chunk's first tile; a source chunk keeps
        // them in range, and they are only hashed.
        let side = i64::from(CHUNK_SIZE);
        let (left, top) = (cx.wrapping_mul(side), cy.wrapping_mul(side));
        for (y, plane_y) in (1..=SIDE).zip((0..).map(|offset: i64| top.wrapping_add(offset))) {
            for (x, plane_x) in (1..=SIDE).zip((0..).map(|offset: i64| left.wrapping_add(offset))) {
                let Some(terrain) = terrain_at(x, y) else {
                    continue;
                };
                let neighbours = [
                    terrain_at(x, y - 1),
                    terrain_at(x + 1, y - 1),
                    terrain_at(x + 1, y),
                    terrain_at(x + 1, y + 1),
                    terrain_at(x, y + 1),
                    terrain_at(x - 1, y + 1),
                    terrain_at(x - 1, y),
                    terrain_at(x - 1, y - 1),
                ];
                let index = (y - 1) * SIDE + (x - 1);
                // In the terrain layer a transition replaces the painted
                // prototype; elsewhere a prototype the source places wins.
                let blocked = (0..self.ground_layers.min(8)).fold(0_u8, |blocked, output| {
                    if output != self.terrain_layer && cell(own.get(output), index).is_some() {
                        blocked | (1 << output)
                    } else {
                        blocked
                    }
                });
                self.draw_tile(
                    terrain,
                    &neighbours,
                    blocked,
                    (layer, plane_x, plane_y),
                    &mut drawn,
                );
                for (output, entry) in drawn.iter().enumerate() {
                    if let (Some((_, tile)), Some(slot)) = (
                        entry,
                        layers
                            .get_mut(output)
                            .and_then(|tiles| tiles.get_mut(index)),
                    ) {
                        *slot = Some(*tile);
                    }
                }
            }
        }
        ResolvedChunk { layers }
    }

    /// Sets `drawn`, for each ground layer, to the latest transition that
    /// draws on the tile `(x, y)` of `layer`, by position, and its tile, or
    /// to `None`. The tile's terrain is `terrain` and its neighbours' are
    /// `neighbours`; a transition into a ground layer whose bit is set in
    /// `blocked` draws nothing.
    ///
    /// Only the transitions of the tile's own terrain and of its
    /// neighbours' terrains can draw on it: an outside transition of any
    /// other terrain sees no neighbour of its terrain. Each terrain is
    /// visited once and its transitions latest first, so one that a later
    /// transition has already beaten in its layer is skipped, and each
    /// other one costs a lookup in its table of neighbourhoods.
    fn draw_tile<'a>(
        &'a self,
        terrain: usize,
        neighbours: &[Option<usize>; 8],
        blocked: u8,
        (layer, x, y): (i32, i64, i64),
        drawn: &mut [Option<(usize, &'a str)>],
    ) {
        drawn.fill(None);
        let owners = std::iter::once(terrain).chain(neighbours.iter().enumerate().filter_map(
            |(seen, neighbour)| {
                let neighbour = (*neighbour)?;
                (neighbour != terrain && !neighbours[..seen].contains(&Some(neighbour)))
                    .then_some(neighbour)
            },
        ));
        for owner in owners {
            let Some(range) = self
                .terrains
                .get(owner)
                .map(|owner| owner.transitions.clone())
            else {
                continue;
            };
            // Which neighbours are the owner's terrain, for its outside
            // transitions.
            let same = neighbour_bits(neighbours, |neighbour| neighbour == Some(owner));
            for position in range.rev() {
                let Some(transition) = self.list.get(position) else {
                    continue;
                };
                let output = transition.layer;
                if blocked & (1 << output.min(7)) != 0
                    || drawn
                        .get(output)
                        .is_some_and(|entry| entry.is_some_and(|(at, _)| at > position))
                {
                    continue;
                }
                // Inside transitions of the tile's terrain, and outside ones
                // of its neighbours' that blend with it.
                let shows = if owner == terrain {
                    if transition.draw != Draw::Inside {
                        continue;
                    }
                    neighbour_bits(neighbours, |neighbour| {
                        neighbour.is_none_or(|neighbour| !transition.blends_with(neighbour))
                    })
                } else if transition.draw == Draw::Outside && transition.blends_with(terrain) {
                    same
                } else {
                    continue;
                };
                if !transition.draws(shows) {
                    continue;
                }
                let hash = variant_hash(transition.seed, layer, x, y);
                if let (Some(tile), Some(slot)) =
                    (transition.draw_at(shows, hash), drawn.get_mut(output))
                {
                    *slot = Some((position, tile));
                }
            }
        }
    }

    /// The terrain of each tile of the chunk at `(layer, cx, cy)` and of the
    /// border of one tile around it, row-major over [`WINDOW`]² tiles.
    fn window<S>(&self, source: &S, layer: i32, cx: i64, cy: i64) -> Vec<Option<usize>>
    where
        S: TerrainSource + ?Sized,
    {
        let mut window = vec![None; WINDOW * WINDOW];
        for dy in -1_i64..=1 {
            for dx in -1_i64..=1 {
                let (Some(x), Some(y)) = (cx.checked_add(dx), cy.checked_add(dy)) else {
                    continue;
                };
                let Some(grid) = source
                    .chunk_ground(layer, x, y)
                    .and_then(|grids| grids.get(self.terrain_layer))
                else {
                    continue;
                };
                // Each palette entry's terrain is looked up once, when a
                // tile first uses it.
                let mut terrains: BTreeMap<u16, Option<usize>> = BTreeMap::new();
                // The window columns and rows this neighbour covers, and the
                // chunk's own column and row of the first.
                let span = |offset: i64| -> (usize, usize, usize) {
                    match offset {
                        -1 => (0, 1, SIDE - 1),
                        0 => (1, SIDE + 1, 0),
                        _ => (SIDE + 1, SIDE + 2, 0),
                    }
                };
                let (column_start, column_end, first_column) = span(dx);
                let (row_start, row_end, first_row) = span(dy);
                for (row, local_y) in (row_start..row_end).zip(first_row..) {
                    for (column, local_x) in (column_start..column_end).zip(first_column..) {
                        let Some(&value) = grid.cells.get(local_y * SIDE + local_x) else {
                            continue;
                        };
                        if value == 0 {
                            continue;
                        }
                        let terrain = *terrains.entry(value).or_insert_with(|| {
                            grid.palette
                                .get(usize::from(value) - 1)
                                .and_then(|proto| self.terrain_of(proto))
                        });
                        if let Some(slot) = window.get_mut(row * WINDOW + column) {
                            *slot = terrain;
                        }
                    }
                }
            }
        }
        window
    }
}
