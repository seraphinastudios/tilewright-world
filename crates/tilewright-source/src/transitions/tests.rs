// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

use super::{Draw, Model, Transitions, variant_hash};
use crate::{Grid, Plane, TerrainSource, World, load};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/terrain")
}

fn world() -> World {
    load(&fixture()).unwrap_or_else(|diagnostics| {
        panic!(
            "{}",
            diagnostics
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        )
    })
}

fn transitions(world: &World) -> Arc<Transitions> {
    Arc::clone(world.profile().transitions.as_ref().unwrap())
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

const TRANSITIONS: &str = "profile/transitions.jsonc";

/// The diagnostics of the fixture with its transitions document replaced by
/// `document`, as `(code, path)`.
fn problems_with(document: &str) -> Vec<(&'static str, String)> {
    let directory = tempfile::tempdir().unwrap();
    copy_tree(&fixture(), directory.path());
    fs::write(directory.path().join(TRANSITIONS), document).unwrap();
    load(directory.path())
        .expect_err("the tree must be rejected")
        .into_iter()
        .map(|diagnostic| (diagnostic.code, diagnostic.path))
        .collect()
}

/// A transitions document with the fixture's terrains, whose last terrain,
/// `grass`, has the transitions `rules`.
fn document(rules: &str) -> String {
    format!(
        r#"{{
  "format": "tilewright.transitions/1",
  "layer": "floor",
  "terrains": [
    {{"id": "water", "protos": ["water"]}},
    {{"id": "sand", "protos": ["sand"]}},
    {{"id": "dirt", "protos": ["dirt"]}},
    {{"id": "grass", "protos": ["grass", "grass_flowers"], "transitions": [{rules}]}}
  ]
}}"#
    )
}

fn rejects(document: &str, code: &str) {
    let problems = problems_with(document);
    assert!(
        problems
            .iter()
            .any(|(found, path)| *found == code && path == TRANSITIONS),
        "expected {code} in {TRANSITIONS}, got {problems:?}"
    );
}

/// The resolved prototype at plane tile `(x, y)` of layer 0 and ground
/// layer `ground` of the island.
fn tile(transitions: &Transitions, plane: &Plane, ground: usize, x: i64, y: i64) -> Option<String> {
    let (cx, cy) = (x.div_euclid(32), y.div_euclid(32));
    let index = usize::try_from(y.rem_euclid(32) * 32 + x.rem_euclid(32)).unwrap();
    transitions
        .resolve_chunk(plane, 0, cx, cy)
        .value(ground, index)
        .map(str::to_owned)
}

/// Every prototype the fixture defines, in byte order.
fn fixture_protos() -> Vec<String> {
    let mut protos: Vec<String> = fs::read_dir(fixture().join("defs/proto"))
        .unwrap()
        .map(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_str()
                .unwrap()
                .trim_end_matches(".jsonc")
                .to_owned()
        })
        .collect();
    protos.sort();
    protos
}

/// Whether a tile is open water: one of the water's variants.
fn open_water(tile: Option<&str>) -> bool {
    matches!(tile, Some("water" | "water_ripple"))
}

const FLOOR: usize = 0;
const DECAL: usize = 1;
const EDGE: usize = 2;

#[test]
fn each_model_produces_exactly_its_patterns() {
    for (model, draw, count) in [
        (Model::None, Draw::Inside, 1),
        (Model::Sides, Draw::Inside, 16),
        (Model::Sides, Draw::Outside, 15),
        (Model::Corners, Draw::Inside, 16),
        (Model::Corners, Draw::Outside, 15),
        (Model::Blob, Draw::Inside, 47),
        (Model::Blob, Draw::Outside, 46),
    ] {
        // Every neighbourhood, and the patterns those that draw have.
        let found: BTreeSet<u8> = (0..=255_u8)
            .map(|bits| {
                let shows = std::array::from_fn(|bit| bits & (1 << bit) != 0);
                model.pattern(draw, shows)
            })
            .filter(|pattern| draw == Draw::Inside || *pattern != 0)
            .collect();
        let produced: BTreeSet<u8> = (0..=255_u8)
            .filter(|pattern| model.produces(draw, *pattern))
            .collect();
        assert_eq!(found, produced, "{model:?} {draw:?}");
        assert_eq!(produced.len(), count, "{model:?} {draw:?}");
    }
}

#[test]
fn patterns_follow_the_specification() {
    let shows = |names: &[&str]| -> [bool; 8] {
        let order = ["n", "ne", "e", "se", "s", "sw", "w", "nw"];
        std::array::from_fn(|bit| names.contains(&order[bit]))
    };
    let all = shows(&["n", "ne", "e", "se", "s", "sw", "w", "nw"]);
    assert_eq!(Model::Blob.pattern(Draw::Inside, all), 255);
    assert_eq!(Model::Blob.pattern(Draw::Inside, shows(&[])), 0);
    // A diagonal counts inside only with both its sides.
    assert_eq!(Model::Blob.pattern(Draw::Inside, shows(&["n", "ne"])), 1);
    assert_eq!(
        Model::Blob.pattern(Draw::Inside, shows(&["n", "ne", "e"])),
        1 | 2 | 4
    );
    // A shore to the north: everything but the north side and corners.
    let open_north = shows(&["e", "se", "s", "sw", "w"]);
    assert_eq!(Model::Blob.pattern(Draw::Inside, open_north), 124);
    // Outside, a diagonal counts only where neither side draws.
    assert_eq!(Model::Blob.pattern(Draw::Outside, shows(&["ne"])), 2);
    assert_eq!(Model::Blob.pattern(Draw::Outside, shows(&["n", "ne"])), 1);
    // Corners: outside any of a corner's three tiles shows it, inside all.
    assert_eq!(Model::Corners.pattern(Draw::Outside, shows(&["n"])), 1 | 8);
    assert_eq!(Model::Corners.pattern(Draw::Outside, shows(&["se"])), 2);
    assert_eq!(
        Model::Corners.pattern(Draw::Inside, shows(&["n", "ne", "e"])),
        1
    );
    assert_eq!(Model::Corners.pattern(Draw::Inside, shows(&["n", "e"])), 0);
    assert_eq!(
        Model::Sides.pattern(Draw::Inside, shows(&["n", "ne", "w"])),
        1 | 8
    );
    assert_eq!(Model::None.pattern(Draw::Inside, all), 0);
}

#[test]
fn variant_hashes_are_fixed() {
    // These values are in docs/spec/FORMAT.md section 13; a change here
    // changes every world's variants.
    assert_eq!(variant_hash(0, 0, 0, 0), 0);
    assert_eq!(variant_hash(0, 0, 1, 0), 0x7ab4_0e09_0f36_3a7d);
    assert_eq!(variant_hash(7, 0, 3, -2), 0xb15c_867e_036a_0334);
    assert_eq!(variant_hash(42, -3, 100, -200), 0x2dc3_a5dd_f0d2_29e7);
    assert_eq!(
        variant_hash(u32::MAX, -1, i64::MIN, i64::MAX),
        0x82c9_90ff_e041_f8cc
    );
}

#[test]
fn picks_follow_weights() {
    let world = world();
    let transitions = transitions(&world);
    let water = &transitions.transitions()[0];
    let open = &water.tiles()[&255];
    assert_eq!(open.variants().len(), 2);
    let mut counts: BTreeMap<&str, u32> = BTreeMap::new();
    for x in 0..4_000 {
        *counts
            .entry(open.pick(variant_hash(0, 0, x, 0)))
            .or_default() += 1;
    }
    // Weights 3 and 1.
    let water = counts["water"];
    assert!((2_800..=3_200).contains(&water), "{counts:?}");
    assert_eq!(counts["water"] + counts["water_ripple"], 4_000);
    // The same hash always picks the same variant.
    assert_eq!(open.pick(5), open.pick(5));
    assert_eq!(open.pick(0), "water");
    assert_eq!(open.pick(2), "water");
    assert_eq!(open.pick(3), "water_ripple");
    assert_eq!(open.pick(4), "water");
    // Each point of the total picks the variant whose share holds it, as
    // walking the running sum does.
    let weights = [1_u32, 65_535, 2, 1, 7];
    let pattern = super::Pattern::new(
        weights
            .iter()
            .enumerate()
            .map(|(index, weight)| super::Variant {
                proto: format!("v{index}"),
                weight: *weight,
            })
            .collect(),
    );
    let total: u64 = weights.iter().map(|weight| u64::from(*weight)).sum();
    for point in [0, 1, 2, 65_535, 65_536, 65_537, 65_538, 65_539, total - 1] {
        let mut left = point;
        let walked = weights
            .iter()
            .position(|weight| {
                let inside = left < u64::from(*weight);
                left = left.saturating_sub(u64::from(*weight));
                inside
            })
            .unwrap();
        assert_eq!(pattern.pick(point), format!("v{walked}"), "{point}");
        assert_eq!(pattern.pick(point + total), format!("v{walked}"), "{point}");
    }
}

#[test]
fn the_fixture_reads_its_terrains_and_transitions() {
    let world = world();
    let transitions = transitions(&world);
    assert_eq!(transitions.path(), TRANSITIONS);
    assert_eq!(transitions.terrain_layer(), FLOOR);
    assert_eq!(transitions.ground_layers(), 3);
    let ids: Vec<&str> = transitions
        .terrains()
        .iter()
        .map(|terrain| terrain.id.as_str())
        .collect();
    assert_eq!(ids, ["water", "sand", "dirt", "grass"]);
    assert_eq!(transitions.terrain("grass"), Some(3));
    assert_eq!(transitions.terrain_of("grass_flowers"), Some(3));
    assert_eq!(transitions.terrain_of("stone"), None);
    assert_eq!(transitions.paint(3), Some("grass"));
    assert_eq!(transitions.paint(4), None);
    assert_eq!(transitions.transitions().len(), 4);
    assert_eq!(transitions.terrains()[3].transitions, 3..4);
    let sand = &transitions.transitions()[1];
    assert_eq!(
        (sand.terrain(), sand.draw(), sand.model(), sand.layer()),
        (1, Draw::Outside, Model::Sides, EDGE)
    );
    assert!(sand.blends_with(2));
    assert!(!sand.blends_with(0));
    assert!(!sand.blends_with(99));
    // Without "with", inside blends with every other terrain and outside
    // draws onto every lower one.
    let water = &transitions.transitions()[0];
    assert!((1..4).all(|terrain| water.blends_with(terrain)));
    assert!(!water.blends_with(0));
    let grass = &transitions.transitions()[3];
    assert!((0..3).all(|terrain| grass.blends_with(terrain)));
    assert_eq!(transitions.transitions()[2].seed(), 7);
    // Every prototype the document names: members and tiles.
    let named: BTreeSet<&str> = transitions.protos().collect();
    for proto in [
        "water",
        "grass_flowers",
        "dirt",
        "pond",
        "grass_edge_5",
        "water_ripple",
    ] {
        assert!(named.contains(proto), "{proto}");
    }
    assert!(!named.contains("stone"));
    assert!(!named.contains("bush"));
}

/// Renders the resolved tiles of every chunk of the fixture, with a legend
/// of one character per prototype.
fn render(world: &World) -> String {
    let transitions = transitions(world);
    let mut chunks = Vec::new();
    let mut protos = BTreeSet::new();
    for (id, plane) in world.planes() {
        for &(layer, cx, cy) in plane.chunks.keys() {
            let resolved = transitions.resolve_chunk(plane, layer, cx, cy);
            protos.extend(resolved.values());
            chunks.push((id.as_str(), layer, cx, cy, resolved));
        }
    }
    let symbols: BTreeMap<&str, char> = protos
        .iter()
        .copied()
        .zip(('a'..='z').chain('A'..='Z').chain('0'..='9'))
        .collect();
    assert_eq!(symbols.len(), protos.len(), "too many prototypes to render");
    let mut text = String::from(
        "# The fixture's tiles as Transitions::resolve_chunk resolves them, one\n\
         # character per tile, `.` for an empty one. Generated; regenerate with\n\
         # TILEWRIGHT_UPDATE_TERRAIN_FIXTURES=1 cargo test --locked -p tilewright-source resolved_fixture_is_current\n",
    );
    for (proto, symbol) in &symbols {
        let _ = writeln!(text, "{symbol} {proto}");
    }
    let names = &world.profile().ground_layers;
    for (plane, layer, cx, cy, resolved) in chunks {
        for (ground, name) in names.iter().enumerate() {
            let _ = write!(text, "\n{plane} layer {layer} chunk {cx} {cy} {name}\n");
            for y in 0..32 {
                let row: String = (0..32)
                    .map(|x| {
                        resolved
                            .value(ground, y * 32 + x)
                            .map_or('.', |proto| symbols[proto])
                    })
                    .collect();
                text.push_str(&row);
                text.push('\n');
            }
        }
    }
    text
}

#[test]
fn resolved_fixture_is_current() {
    let path = fixture().join("resolved.txt");
    let rendered = render(&world());
    if std::env::var_os("TILEWRIGHT_UPDATE_TERRAIN_FIXTURES").is_some() {
        fs::write(&path, &rendered).unwrap();
    }
    let recorded = fs::read_to_string(&path).unwrap_or_default();
    assert!(
        recorded == rendered,
        "fixtures/terrain/resolved.txt differs from what the resolver gives; \
         regenerate it as its header says and review the change"
    );
}

#[test]
fn resolution_is_deterministic() {
    let first = render(&world());
    let second = render(&world());
    assert_eq!(first, second);
}

#[test]
fn transitions_draw_over_the_painted_terrain() {
    let world = world();
    let transitions = transitions(&world);
    let island = &world.planes()["island"];
    // A lone water tile has pattern 0.
    assert_eq!(
        tile(&transitions, island, EDGE, 3, 3).as_deref(),
        Some("pond")
    );
    // The pond's sides, and a corner tile whose pattern has no tile.
    assert_eq!(
        tile(&transitions, island, EDGE, 10, 8).as_deref(),
        Some("shore_n")
    );
    assert_eq!(
        tile(&transitions, island, EDGE, 10, 15).as_deref(),
        Some("shore_s")
    );
    assert_eq!(
        tile(&transitions, island, EDGE, 6, 12).as_deref(),
        Some("shore_w")
    );
    assert_eq!(
        tile(&transitions, island, EDGE, 14, 10).as_deref(),
        Some("shore_e")
    );
    assert_eq!(tile(&transitions, island, EDGE, 6, 8), None);
    // The terrain layer keeps the painted terrain everywhere.
    let world_floor = |x: i64, y: i64| {
        island.chunks[&(0, x.div_euclid(32), y.div_euclid(32))].ground[FLOOR]
            .value(usize::try_from(y.rem_euclid(32) * 32 + x.rem_euclid(32)).unwrap())
            .map(str::to_owned)
    };
    for &(layer, cx, cy) in island.chunks.keys() {
        let resolved = transitions.resolve_chunk(island, layer, cx, cy);
        for index in 0..1024 {
            assert_eq!(
                resolved.value(FLOOR, index),
                island.chunks[&(layer, cx, cy)].ground[FLOOR].value(index)
            );
        }
    }
    assert_eq!(world_floor(3, 3).as_deref(), Some("water"));
    // Stone is no terrain: the water beside it continues, and nothing draws
    // on it.
    assert_eq!(tile(&transitions, island, EDGE, 14, 11), None);
    assert_eq!(
        tile(&transitions, island, FLOOR, 15, 11).as_deref(),
        Some("stone")
    );
    assert_eq!(tile(&transitions, island, EDGE, 15, 11), None);
    // Dirt picks its variant decals; unpainted tiles stay empty.
    let dirt: BTreeSet<String> = (0..32)
        .filter_map(|x| tile(&transitions, island, DECAL, x, 0))
        .collect();
    assert_eq!(
        dirt.into_iter().collect::<Vec<_>>(),
        ["dirt", "dirt_pebbles"]
    );
    for ground in [FLOOR, DECAL, EDGE] {
        assert_eq!(tile(&transitions, island, ground, 21, 15), None);
    }
}

#[test]
fn transitions_match_across_chunk_edges() {
    let world = world();
    let transitions = transitions(&world);
    let island = &world.planes()["island"];
    // Grass that starts at x = 32, in chunk (1, 0), draws its edge onto the
    // dirt at x = 31, in chunk (0, 0).
    assert_eq!(
        tile(&transitions, island, EDGE, 31, 15).as_deref(),
        Some("grass_edge_3")
    );
    assert_eq!(
        tile(&transitions, island, EDGE, 31, 14).as_deref(),
        Some("grass_edge_3")
    );
    assert_eq!(
        tile(&transitions, island, EDGE, 31, 13).as_deref(),
        Some("grass_edge_2")
    );
    // A pond across the same edge has no shore along it.
    assert!(open_water(
        tile(&transitions, island, EDGE, 31, 21).as_deref()
    ));
    assert!(open_water(
        tile(&transitions, island, EDGE, 32, 21).as_deref()
    ));
    assert_eq!(
        tile(&transitions, island, EDGE, 30, 20).as_deref(),
        Some("shore_n")
    );
    assert_eq!(
        tile(&transitions, island, EDGE, 33, 20).as_deref(),
        Some("shore_n")
    );
    // Sand at y = 32, in chunk (0, 1), edges the dirt at y = 31.
    assert_eq!(
        tile(&transitions, island, EDGE, 21, 31).as_deref(),
        Some("sand_s")
    );
    assert_eq!(
        tile(&transitions, island, EDGE, 21, 33).as_deref(),
        Some("sand_n")
    );
    // Without the neighbouring chunk, there is nothing to draw.
    let mut alone = island.clone();
    alone.chunks.remove(&(0, 1, 0));
    assert_eq!(tile(&transitions, &alone, EDGE, 31, 15), None);
    // Inside, a tile without terrain continues the water.
    assert!(open_water(
        tile(&transitions, &alone, EDGE, 31, 21).as_deref()
    ));
    // A chunk the source does not have resolves to empty tiles.
    let missing = transitions.resolve_chunk(island, 0, 5, 5);
    assert_eq!(missing.ground_layers(), 3);
    assert_eq!(missing.values().count(), 0);
    assert_eq!(missing.at(FLOOR, [0, 0]), None);
    assert_eq!(
        transitions.resolve_chunk(island, 1, 0, 0).values().count(),
        0
    );
}

#[test]
fn later_transitions_and_authored_tiles_win() {
    let world = world();
    let transitions = transitions(&world);
    let island = &world.planes()["island"];
    // Sand to the west and grass to the east: grass comes later and wins.
    assert_eq!(
        tile(&transitions, island, EDGE, 13, 30).as_deref(),
        Some("grass_edge_3")
    );
    // Only sand, to the south.
    assert_eq!(
        tile(&transitions, island, EDGE, 12, 28).as_deref(),
        Some("sand_s")
    );
    // A bush placed by hand beside the grass stays.
    assert_eq!(
        tile(&transitions, island, EDGE, 25, 6).as_deref(),
        Some("bush")
    );
    assert_eq!(
        tile(&transitions, island, EDGE, 25, 5).as_deref(),
        Some("grass_edge_3")
    );
    // Hand-placed flowers are grass: no edge is drawn around them inside
    // the grass, and they keep their prototype.
    assert_eq!(
        tile(&transitions, island, FLOOR, 28, 6).as_deref(),
        Some("grass_flowers")
    );
    assert_eq!(tile(&transitions, island, EDGE, 27, 6), None);
}

/// A source over plain grids, as an editor might keep them.
struct Grids(BTreeMap<(i32, i64, i64), Vec<Grid>>);

impl TerrainSource for Grids {
    fn chunk_ground(&self, layer: i32, cx: i64, cy: i64) -> Option<&[Grid]> {
        self.0.get(&(layer, cx, cy)).map(Vec::as_slice)
    }
}

#[test]
fn any_terrain_source_resolves_alike() {
    let world = world();
    let transitions = transitions(&world);
    let island = &world.planes()["island"];
    let mut grids = Grids(
        island
            .chunks
            .iter()
            .map(|(key, chunk)| (*key, chunk.ground.clone()))
            .collect(),
    );
    for &(layer, cx, cy) in island.chunks.keys() {
        assert_eq!(
            transitions.resolve_chunk(&grids, layer, cx, cy),
            transitions.resolve_chunk(island, layer, cx, cy)
        );
    }
    // An edit to the source shows in the next resolve: paint the lone water
    // tile over with grass.
    let floor = &mut grids.0.get_mut(&(0, 0, 0)).unwrap()[0];
    let grass = u16::try_from(
        floor
            .palette
            .iter()
            .position(|value| value == "grass")
            .unwrap()
            + 1,
    )
    .unwrap();
    floor.cells[3 * 32 + 3] = grass;
    let resolved = transitions.resolve_chunk(&grids, 0, 0, 0);
    assert_eq!(resolved.at(FLOOR, [3, 3]), Some("grass"));
    assert_eq!(resolved.at(EDGE, [3, 2]), Some("grass_edge_6"));
    // A malformed grid, as a public model may hold, resolves without
    // panicking.
    grids.0.insert(
        (0, 2, 0),
        vec![Grid {
            palette: vec!["water".to_owned()],
            cells: vec![2; 7],
        }],
    );
    let resolved = transitions.resolve_chunk(&grids, 0, 2, 0);
    assert_eq!(resolved.values().count(), 0);
    let grids = transitions.resolve_chunk(island, 0, 0, 0).grids();
    assert_eq!(grids.len(), 3);
    assert_eq!(grids[EDGE].value(6 * 32 + 25), Some("bush"));
    assert_eq!(grids[EDGE].value(3 * 32 + 3), Some("pond"));
    assert_eq!(grids[FLOOR].value(3 * 32 + 3), Some("water"));
}

/// An outside transition of `grass` with one side pattern.
const SIDES: &str =
    r#"{"draw": "outside", "model": "sides", "layer": "edge", "tiles": {"1": "sand_n"}}"#;

#[test]
fn malformed_documents_are_rejected() {
    let sides = SIDES;
    rejects(
        &document("").replace("transitions/1", "transitions/2"),
        "shape.format",
    );
    rejects(
        &document("").replace("\"layer\": \"floor\",", ""),
        "shape.missing_key",
    );
    rejects(
        &document(&sides.replace("\"draw\"", "\"drew\"")),
        "shape.unknown_key",
    );
    rejects(
        &document(&sides.replace("\"sides\"", "\"hex\"")),
        "shape.range",
    );
    rejects(
        &document(&sides.replace("\"outside\"", "\"over\"")),
        "shape.range",
    );
    rejects(&document(&sides.replace("\"sides\"", "3")), "shape.type");
    // No transition draws into the terrain layer.
    rejects(
        &document(&sides.replace("\"edge\"", "\"floor\"")),
        "shape.range",
    );
    rejects(
        &document(
            &sides
                .replace("\"sides\"", "\"none\"")
                .replace("\"1\"", "\"0\""),
        ),
        "shape.range",
    );
    rejects(
        &document(&sides.replace(", \"tiles\": {\"1\": \"sand_n\"}", "")),
        "shape.missing_key",
    );
    rejects(
        &document(&sides.replace("{\"1\": \"sand_n\"}", "{}")),
        "shape.range",
    );
    rejects(
        &document(&sides.replace("{\"1\": \"sand_n\"}", "[]")),
        "shape.type",
    );
    rejects(
        &document(&sides.replace("\"layer\": \"edge\"", "\"layer\": \"edge\", \"seed\": -1")),
        "shape.range",
    );
}

#[test]
fn malformed_patterns_and_variants_are_rejected() {
    let sides = SIDES;
    // Patterns: canonical decimals the model produces.
    for key in ["01", "16", "256", "-1", "x", "", "1.0"] {
        rejects(
            &document(&sides.replace("\"1\":", &format!("\"{key}\":"))),
            "shape.range",
        );
    }
    rejects(&document(&sides.replace("\"1\":", "\"0\":")), "shape.range");
    let blob = r#"{"draw": "inside", "model": "blob", "layer": "edge", "tiles": {"2": "grass"}}"#;
    rejects(&document(blob), "shape.range");
    rejects(
        &document(&blob.replace("inside", "outside").replace("\"2\"", "\"3\"")),
        "shape.range",
    );
    // Variants.
    for tile in [
        "[]",
        "{\"proto\": \"grass\", \"weight\": 0}",
        "{\"proto\": \"grass\", \"weight\": 65536}",
        "{\"proto\": \"grass\", \"weight\": 1.5}",
        "{\"proto\": \"grass\", \"kind\": 1}",
        "{\"weight\": 2}",
        "7",
        "\"Grass\"",
    ] {
        let problems = problems_with(&document(&sides.replace("\"sand_n\"", tile)));
        assert!(
            problems.iter().any(|(_, path)| path == TRANSITIONS),
            "{tile}: {problems:?}"
        );
    }
    let many = format!("[{}]", vec!["\"grass\""; 257].join(", "));
    rejects(
        &document(&sides.replace("\"sand_n\"", &many)),
        "shape.range",
    );
    let most = format!("[{}]", vec!["\"grass\""; 256].join(", "));
    assert!(load_with_document(&document(
        &sides.replace("\"sand_n\"", &most)
    )));
}

#[test]
fn references_resolve_and_are_unique() {
    let sides = SIDES;
    rejects(
        &document(&sides.replace("\"sand_n\"", "\"lava\"")),
        "ref.unresolved",
    );
    rejects(
        &document(&sides.replace("\"edge\"", "\"item\"")),
        "ref.unresolved",
    );
    rejects(
        &document("").replace("\"layer\": \"floor\"", "\"layer\": \"item\""),
        "ref.unresolved",
    );
    rejects(
        &document("").replace("[\"water\"]", "[\"lava\"]"),
        "ref.unresolved",
    );
    rejects(
        &document(&sides.replace(
            "\"layer\": \"edge\"",
            "\"layer\": \"edge\", \"with\": [\"lava\"]",
        )),
        "ref.unresolved",
    );
    rejects(
        &document(&sides.replace(
            "\"layer\": \"edge\"",
            "\"layer\": \"edge\", \"with\": [\"grass\"]",
        )),
        "shape.range",
    );
    rejects(
        &document(&sides.replace("\"layer\": \"edge\"", "\"layer\": \"edge\", \"with\": []")),
        "shape.range",
    );
    rejects(
        &document(&sides.replace(
            "\"layer\": \"edge\"",
            "\"layer\": \"edge\", \"with\": [\"dirt\", \"dirt\"]",
        )),
        "ref.duplicate",
    );
    rejects(
        &document("").replace("\"sand\", \"protos\"", "\"water\", \"protos\""),
        "ref.duplicate",
    );
    rejects(
        &document("").replace("[\"sand\"]", "[\"water\"]"),
        "ref.duplicate",
    );
    rejects(
        &document("").replace("[\"sand\"]", "[\"sand\", \"sand\"]"),
        "ref.duplicate",
    );
    rejects(&document("").replace("[\"sand\"]", "[]"), "shape.range");
    // An outside transition of the first terrain names what it draws onto.
    let first = r#"{"format": "tilewright.transitions/1", "layer": "floor", "terrains": [
        {"id": "water", "protos": ["water"], "transitions": [
            {"draw": "outside", "model": "sides", "layer": "edge", "tiles": {"1": "shore_n"}}]},
        {"id": "sand", "protos": ["sand"]}]}"#;
    rejects(first, "shape.missing_key");
    assert!(load_with_document(&first.replace(
        "\"layer\": \"edge\",",
        "\"layer\": \"edge\", \"with\": [\"sand\"],"
    )));
    // A terrain listed later may be named.
    assert!(load_with_document(&document("").replace(
        r#"{"id": "water", "protos": ["water"]}"#,
        r#"{"id": "water", "protos": ["water"], "transitions": [{"draw": "inside",
            "model": "sides", "layer": "edge", "with": ["grass"], "tiles": {"15": "water"}}]}"#
    )));
}

#[test]
fn counts_are_capped() {
    let sides = SIDES;
    let terrains: Vec<String> = (0..1_025)
        .map(|index| format!("{{\"id\": \"t{index}\", \"protos\": [\"water\"]}}"))
        .collect();
    rejects(
        &format!(
            "{{\"format\": \"tilewright.transitions/1\", \"layer\": \"floor\", \"terrains\": [{}]}}",
            terrains.join(", ")
        ),
        "shape.range",
    );
    rejects(
        "{\"format\": \"tilewright.transitions/1\", \"layer\": \"floor\", \"terrains\": []}",
        "shape.range",
    );
    rejects(&document(&[sides; 9].join(", ")), "shape.range");
    assert!(load_with_document(&document(&[sides; 8].join(", "))));
    // One transition, then 32 terrains of 8, pass the document's 256.
    let crowded: Vec<String> = fixture_protos()
        .into_iter()
        .filter(|proto| proto != "dirt")
        .enumerate()
        .map(|(index, proto)| {
            format!(
                "{{\"id\": \"t{index}\", \"protos\": [\"{proto}\"], \"transitions\": [{}]}}",
                vec![
                    sides.replace(
                        "\"layer\": \"edge\"",
                        "\"layer\": \"edge\", \"with\": [\"x\"]"
                    );
                    8
                ]
                .join(", ")
            )
        })
        .collect();
    let crowded = format!(
        "{{\"format\": \"tilewright.transitions/1\", \"layer\": \"floor\", \"terrains\": [{{\"id\": \"x\", \"protos\": [\"dirt\"], \"transitions\": [{{\"draw\": \"inside\", \"model\": \"none\", \"layer\": \"decal\", \"tiles\": {{\"0\": \"dirt\"}}}}]}}, {}]}}",
        crowded.join(", ")
    );
    let problems = problems_with(&crowded);
    assert!(
        problems
            .iter()
            .any(|(code, path)| *code == "shape.range" && path == TRANSITIONS),
        "{problems:?}"
    );
}

/// Whether the fixture loads with its transitions document replaced by
/// `document`.
fn load_with_document(document: &str) -> bool {
    let directory = tempfile::tempdir().unwrap();
    copy_tree(&fixture(), directory.path());
    fs::write(directory.path().join(TRANSITIONS), document).unwrap();
    match load(directory.path()) {
        Ok(_) => true,
        Err(diagnostics) => {
            eprintln!("{diagnostics:?}");
            false
        }
    }
}

#[test]
fn the_profile_names_a_safe_transitions_path() {
    let check = |from: &str, to: &str, code: &str| {
        let directory = tempfile::tempdir().unwrap();
        copy_tree(&fixture(), directory.path());
        let path = directory.path().join("profile/profile.jsonc");
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains(from));
        fs::write(&path, text.replacen(from, to, 1)).unwrap();
        let problems: Vec<(&str, String)> = load(directory.path())
            .expect_err("the tree must be rejected")
            .into_iter()
            .map(|diagnostic| (diagnostic.code, diagnostic.path))
            .collect();
        assert!(
            problems.iter().any(|(found, _)| *found == code),
            "{to}: expected {code}, got {problems:?}"
        );
    };
    let named = "\"transitions\": \"transitions.jsonc\"";
    check(
        named,
        "\"transitions\": \"../transitions.jsonc\"",
        "source.path",
    );
    check(
        named,
        "\"transitions\": \"transitions.json\"",
        "source.path",
    );
    check(named, "\"transitions\": 3", "shape.type");
    check(
        named,
        "\"transitions\": \"rules.jsonc\"",
        "source.missing_file",
    );
    check(
        named,
        "\"transitions\": \"schema/proto.jsonc\"",
        "shape.unknown_key",
    );
    // Without transitions, the tree loads and its painted terrain is all.
    let directory = tempfile::tempdir().unwrap();
    copy_tree(&fixture(), directory.path());
    let path = directory.path().join("profile/profile.jsonc");
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, text.replacen(&format!(",\n  {named}"), "", 1)).unwrap();
    let world = load(directory.path()).unwrap();
    assert!(world.profile().transitions.is_none());
}

#[test]
fn every_terrain_is_checked_at_once() {
    // A terrain whose ID is invalid still has its prototypes and
    // transitions checked.
    let problems = problems_with(&document("").replace(
        r#"{"id": "sand", "protos": ["sand"]}"#,
        r#"{"id": "Sand", "protos": [], "transitions": [{"draw": "aside"}]}"#,
    ));
    for code in ["shape.identifier", "shape.range", "shape.missing_key"] {
        assert!(
            problems
                .iter()
                .any(|(found, path)| *found == code && path == TRANSITIONS),
            "{code}: {problems:?}"
        );
    }
    // A repeated prototype names the terrain that has it, even one whose
    // ID is invalid.
    let diagnostics = |document: &str| -> Vec<String> {
        let directory = tempfile::tempdir().unwrap();
        copy_tree(&fixture(), directory.path());
        fs::write(directory.path().join(TRANSITIONS), document).unwrap();
        load(directory.path())
            .unwrap_err()
            .into_iter()
            .map(|diagnostic| diagnostic.message)
            .collect()
    };
    let messages = diagnostics(&document("").replace("[\"sand\"]", "[\"water\"]"));
    assert!(
        messages
            .iter()
            .any(|message| message == "prototype \"water\" already paints terrain \"water\""),
        "{messages:?}"
    );
    let messages = diagnostics(
        &document("")
            .replace("\"id\": \"water\"", "\"id\": \"Water\"")
            .replace("[\"sand\"]", "[\"water\"]"),
    );
    assert!(
        messages
            .iter()
            .any(|message| message == "prototype \"water\" already paints terrain number 1"),
        "{messages:?}"
    );
}

#[test]
fn transitions_count_as_declared() {
    // Malformed transitions count toward the document's cap too, so the
    // cap bounds the work however many fail: 33 terrains of 8 each are
    // more than 256, and the cap is reported once.
    let terrains: Vec<String> = fixture_protos()
        .into_iter()
        .enumerate()
        .map(|(index, proto)| {
            format!(
                "{{\"id\": \"t{index}\", \"protos\": [\"{proto}\"], \"transitions\": [{}]}}",
                ["{}"; 8].join(", ")
            )
        })
        .collect();
    assert_eq!(terrains.len(), 33);
    let document = format!(
        "{{\"format\": \"tilewright.transitions/1\", \"layer\": \"floor\", \"terrains\": [{}]}}",
        terrains.join(", ")
    );
    let directory = tempfile::tempdir().unwrap();
    copy_tree(&fixture(), directory.path());
    fs::write(directory.path().join(TRANSITIONS), document).unwrap();
    let diagnostics = load(directory.path()).unwrap_err();
    let capped = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.message.contains("at most 256 transitions"))
        .count();
    assert_eq!(capped, 1, "{diagnostics:?}");
    // The rules past the cap are not read: only the first 256 report
    // their missing keys.
    let missing = diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.message == "missing key \"draw\"")
        .count();
    assert_eq!(missing, 256);
}

#[test]
fn overlong_lists_count_toward_the_document_cap() {
    // 16 lists of 17 each break their terrain's cap, and together the
    // document's.
    let terrains: Vec<String> = (0..16)
        .map(|index| {
            format!(
                "{{\"id\": \"t{index}\", \"protos\": [\"grass_edge_{}\"], \"transitions\": [{}]}}",
                index + 1,
                vec!["{}"; 17].join(", ")
            )
        })
        .collect();
    let document = format!(
        "{{\"format\": \"tilewright.transitions/1\", \"layer\": \"floor\", \"terrains\": [{}]}}",
        terrains.join(", ")
    );
    let directory = tempfile::tempdir().unwrap();
    copy_tree(&fixture(), directory.path());
    fs::write(directory.path().join(TRANSITIONS), document).unwrap();
    let diagnostics = load(directory.path()).unwrap_err();
    let count = |text: &str| {
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.message.contains(text))
            .count()
    };
    assert_eq!(count("at most 256 transitions"), 1, "{diagnostics:?}");
    assert_eq!(count("transitions must list 0 to 8 entries"), 16);
    // No rule of an overlong list is read.
    assert_eq!(count("missing key \"draw\""), 0);
}

#[test]
fn every_member_of_a_list_is_checked() {
    let messages = |document: &str| -> Vec<(&'static str, String)> {
        let directory = tempfile::tempdir().unwrap();
        copy_tree(&fixture(), directory.path());
        fs::write(directory.path().join(TRANSITIONS), document).unwrap();
        load(directory.path())
            .unwrap_err()
            .into_iter()
            .map(|diagnostic| (diagnostic.code, diagnostic.message))
            .collect()
    };
    // A bad prototype ID does not hide a later prototype that another
    // terrain has.
    let found =
        messages(&document("").replace("[\"sand\"]", "[\"Bad\", \"sand\", \"sand\", \"water\"]"));
    for (code, message) in [
        ("shape.identifier", "\"Bad\" is not a valid identifier"),
        ("ref.duplicate", "\"sand\" is listed twice"),
        (
            "ref.duplicate",
            "prototype \"water\" already paints terrain \"water\"",
        ),
    ] {
        assert!(
            found
                .iter()
                .any(|(found, text)| *found == code && text == message),
            "{message}: {found:?}"
        );
    }
    // A pattern that is not one of the model's still has its tile checked.
    let found = messages(&document(&SIDES.replace(
        r#""1": "sand_n""#,
        r#""16": {"proto": "lava", "weight": 0}"#,
    )));
    for (code, message) in [
        (
            "shape.range",
            "\"16\" is not a pattern of the sides model drawn outside",
        ),
        ("shape.range", "0 is outside 1..=65535"),
        ("ref.unresolved", "no proto definition \"lava\""),
    ] {
        assert!(
            found
                .iter()
                .any(|(found, text)| *found == code && text == message),
            "{message}: {found:?}"
        );
    }
    // Nor does a bad terrain ID in `with` hide a later unknown one.
    let found = messages(&document(&SIDES.replace(
        "\"layer\": \"edge\"",
        "\"layer\": \"edge\", \"with\": [\"Bad\", \"dirt\", \"dirt\", \"lava\"]",
    )));
    for (code, message) in [
        ("shape.identifier", "\"Bad\" is not a valid identifier"),
        ("ref.duplicate", "\"dirt\" is listed twice"),
        ("ref.unresolved", "no terrain \"lava\""),
    ] {
        assert!(
            found
                .iter()
                .any(|(found, text)| *found == code && text == message),
            "{message}: {found:?}"
        );
    }
}

/// Resolves a chunk as section 13 reads, without the resolver's window or
/// candidate pruning: every transition, in order, for every tile, with
/// each neighbour looked up on its own.
fn naive(
    transitions: &Transitions,
    source: &Grids,
    layer: i32,
    cx: i64,
    cy: i64,
) -> Vec<Vec<Option<String>>> {
    let value = |ground: usize, x: i64, y: i64| -> Option<String> {
        let grids = source.chunk_ground(layer, x.div_euclid(32), y.div_euclid(32))?;
        let index = usize::try_from(y.rem_euclid(32) * 32 + x.rem_euclid(32)).unwrap();
        grids.get(ground)?.value(index).map(str::to_owned)
    };
    let terrain_at = |x: i64, y: i64| {
        value(transitions.terrain_layer(), x, y).and_then(|proto| transitions.terrain_of(&proto))
    };
    let mut layers = vec![vec![None; 1024]; transitions.ground_layers()];
    for local_y in 0..32 {
        for local_x in 0..32 {
            let (x, y) = (cx * 32 + local_x, cy * 32 + local_y);
            let index = usize::try_from(local_y * 32 + local_x).unwrap();
            for (ground, tiles) in layers.iter_mut().enumerate() {
                tiles[index] = value(ground, x, y);
            }
            let Some(terrain) = terrain_at(x, y) else {
                continue;
            };
            let neighbours = [
                (0, -1),
                (1, -1),
                (1, 0),
                (1, 1),
                (0, 1),
                (-1, 1),
                (-1, 0),
                (-1, -1),
            ]
            .map(|(dx, dy)| terrain_at(x + dx, y + dy));
            for transition in transitions.transitions() {
                let applies = match transition.draw() {
                    Draw::Inside => transition.terrain() == terrain,
                    Draw::Outside => transition.blends_with(terrain),
                };
                if !applies {
                    continue;
                }
                let shows = neighbours.map(|neighbour| match transition.draw() {
                    Draw::Inside => neighbour.is_none_or(|other| !transition.blends_with(other)),
                    Draw::Outside => neighbour == Some(transition.terrain()),
                });
                let pattern = transition.model().pattern(transition.draw(), shows);
                if transition.draw() == Draw::Outside && pattern == 0 {
                    continue;
                }
                let Some(tiles) = transition.tiles().get(&pattern) else {
                    continue;
                };
                let output = transition.layer();
                // A prototype the source places wins; the loader keeps
                // every transition out of the terrain layer.
                if output == transitions.terrain_layer() || value(output, x, y).is_some() {
                    continue;
                }
                let hash = variant_hash(transition.seed(), layer, x, y);
                layers[output][index] = Some(tiles.pick(hash).to_owned());
            }
        }
    }
    layers
}

/// A transition of [`rich_document`]: its draw, model, layer and `with`.
type RichRule = (Draw, Model, &'static str, &'static str);

/// A transitions document over the fixture's prototypes that uses every
/// model and draw, `with` lists naming earlier and later terrains, and
/// output into both ground layers.
fn rich_document() -> String {
    let terrains: [(&str, &str, &[RichRule]); 6] = [
        (
            "water",
            "\"water\"",
            &[
                (Draw::Inside, Model::Blob, "decal", ""),
                (Draw::Outside, Model::Corners, "edge", "\"rock\""),
            ],
        ),
        (
            "sand",
            "\"sand\"",
            &[
                (Draw::Outside, Model::Sides, "edge", ""),
                (Draw::Inside, Model::None, "decal", ""),
            ],
        ),
        (
            "dirt",
            "\"dirt\", \"dirt_pebbles\"",
            &[
                (Draw::Outside, Model::Blob, "edge", "\"water\", \"sand\""),
                (Draw::Inside, Model::Corners, "edge", ""),
            ],
        ),
        (
            "grass",
            "\"grass\"",
            &[
                (Draw::Outside, Model::Corners, "edge", ""),
                (Draw::Inside, Model::Sides, "decal", "\"dirt\""),
            ],
        ),
        (
            "flowers",
            "\"grass_flowers\"",
            &[(Draw::Outside, Model::Sides, "decal", "\"grass\"")],
        ),
        (
            "rock",
            "\"stone\"",
            &[
                (Draw::Inside, Model::None, "edge", ""),
                (Draw::Outside, Model::Blob, "edge", ""),
            ],
        ),
    ];
    let mut seed = 0;
    let terrains: Vec<String> = terrains
        .iter()
        .map(|(id, protos, rules)| {
            let rules: Vec<String> = rules
                .iter()
                .map(|(draw, model, layer, with)| {
                    seed += 1;
                    rich_rule(*draw, *model, layer, with, seed)
                })
                .collect();
            format!(
                "{{\"id\": \"{id}\", \"protos\": [{protos}], \"transitions\": [{}]}}",
                rules.join(", ")
            )
        })
        .collect();
    format!(
        "{{\"format\": \"tilewright.transitions/1\", \"layer\": \"floor\", \"terrains\": [{}]}}",
        terrains.join(", ")
    )
}

/// A transition of [`rich_document`] with a tile for every pattern its
/// model produces, each of two weighted variants.
fn rich_rule(draw: Draw, model: Model, layer: &str, with: &str, seed: u32) -> String {
    let tiles: Vec<String> = (0..=255_u8)
        .filter(|pattern| model.produces(draw, *pattern))
        .map(|pattern| {
            let edge = u32::from(pattern) % 15 + 1;
            format!(
                "\"{pattern}\": [\"grass_edge_{edge}\", {{\"proto\": \"shore_n\", \"weight\": {}}}]",
                u32::from(pattern) % 3 + 1
            )
        })
        .collect();
    let with = if with.is_empty() {
        String::new()
    } else {
        format!(", \"with\": [{with}]")
    };
    format!(
        "{{\"draw\": \"{}\", \"model\": \"{}\", \"layer\": \"{layer}\"{with}, \"seed\": {seed}, \"tiles\": {{{}}}}}",
        draw.as_str(),
        model.as_str(),
        tiles.join(", ")
    )
}

/// A deterministic pseudo-random painting of a 3 × 3 block of chunks, in
/// runs so that terrains meet in every arrangement, with a few bushes
/// placed by hand; without the chunk at (1, 1) when `missing`.
fn painted(state: &mut u64, missing: bool) -> Grids {
    let floor_values = [
        "water",
        "sand",
        "dirt",
        "dirt_pebbles",
        "grass",
        "grass_flowers",
        "stone",
        "bush",
    ];
    let mut next = |bound: usize| {
        *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        usize::try_from(variant_hash(0, 0, state.cast_signed(), 0) % bound as u64).unwrap()
    };
    let mut grids = Grids(BTreeMap::new());
    for cy in -1..=1 {
        for cx in -1..=1 {
            let mut floor = vec![0_u16; 1024];
            let mut edge = vec![0_u16; 1024];
            for index in 0..1024 {
                // Mostly runs of the tile before, so patterns vary.
                floor[index] = if index > 0 && next(4) != 0 {
                    floor[index - 1]
                } else {
                    u16::try_from(next(floor_values.len() + 1)).unwrap()
                };
                if next(40) == 0 {
                    edge[index] = 1;
                }
            }
            if missing && (cx, cy) == (1, 1) {
                continue;
            }
            grids.0.insert(
                (0, cx, cy),
                vec![
                    Grid {
                        palette: floor_values
                            .iter()
                            .map(|value| (*value).to_owned())
                            .collect(),
                        cells: floor,
                    },
                    Grid {
                        palette: Vec::new(),
                        cells: vec![0; 1024],
                    },
                    Grid {
                        palette: vec!["bush".to_owned()],
                        cells: edge,
                    },
                ],
            );
        }
    }
    grids
}

#[test]
fn the_resolver_matches_the_definition() {
    let directory = tempfile::tempdir().unwrap();
    copy_tree(&fixture(), directory.path());
    fs::write(directory.path().join(TRANSITIONS), rich_document()).unwrap();
    let world = load(directory.path()).unwrap_or_else(|diagnostics| panic!("{diagnostics:?}"));
    let transitions = transitions(&world);
    let mut state = 0x5eed_u64;
    for round in 0..4 {
        let grids = painted(&mut state, round == 3);
        for cy in -1..=1 {
            for cx in -1..=1 {
                let resolved = transitions.resolve_chunk(&grids, 0, cx, cy);
                let expected = naive(&transitions, &grids, 0, cx, cy);
                for (ground, tiles) in expected.iter().enumerate() {
                    for (index, tile) in tiles.iter().enumerate() {
                        assert_eq!(
                            resolved.value(ground, index),
                            tile.as_deref(),
                            "round {round} chunk {cx} {cy} ground {ground} tile {index}"
                        );
                    }
                }
            }
        }
    }
}
