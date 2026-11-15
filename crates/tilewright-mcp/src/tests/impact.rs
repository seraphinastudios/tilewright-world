// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! The differential that `impact` stands on: for every item of every kind
//! it takes, on both fixtures and a generated tree, the removal or rename
//! is carried out on a copy of the tree by rewriting its files (the test
//! does; the server never writes), and the loader's diagnostics must be
//! the predicted breaking references, one to one. A rename that also
//! rewrites exactly the predicted references must load cleanly.

use super::{at, copy_tree, fixture, ready, result, structured};
use crate::Server;
use std::{
    collections::BTreeMap,
    fmt::Write as _,
    path::{Path, PathBuf},
};
use tilewright_jsonc::{Document, Limits, Segment, Value};
use tilewright_source::{Diagnostic, World};

/// A reference `impact` predicts to break.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct Site {
    pub path: String,
    pub start: usize,
    pub end: usize,
    pub role: String,
    pub target: String,
}

/// Every breaking reference of an `impact` call, paged through, and the
/// first page's structured result as canonical JSON.
pub(super) fn predicted(server: &mut Server, arguments: &str) -> (Vec<Site>, String) {
    let inner = &arguments[1..arguments.len() - 1];
    let mut sites = Vec::new();
    let mut cursor: Option<String> = None;
    let mut first = None;
    loop {
        let mut call = format!("{{{inner},\"limit\":7");
        if let Some(cursor) = &cursor {
            let _ = write!(call, ",\"cursor\":\"{cursor}\"");
        }
        call.push('}');
        let (reply, document) = result(server, "impact", &call);
        assert!(reply.contains("\"isError\":false"), "{call}: {reply}");
        let page = structured(&document);
        first.get_or_insert_with(|| document.canonical(page));
        for item in at(page, &["breaking"]).as_array().unwrap() {
            let integer = |key| usize::try_from(at(item, &[key]).as_i64().unwrap()).unwrap();
            sites.push(Site {
                path: at(item, &["path"]).as_str().unwrap().to_owned(),
                start: integer("start"),
                end: integer("end"),
                role: at(item, &["role"]).as_str().unwrap().to_owned(),
                target: at(item, &["to", "id"]).as_str().unwrap().to_owned(),
            });
        }
        let total = at(page, &["breaking_total"]).as_i64().unwrap();
        if let Value::String { value, .. } = at(page, &["next_cursor"]) {
            cursor = Some(value.clone());
        } else {
            assert_eq!(usize::try_from(total).unwrap(), sites.len(), "{arguments}");
            break;
        }
    }
    (sites, first.unwrap())
}

/// One replacement of bytes in a file.
type Edits = BTreeMap<String, Vec<(usize, usize, String)>>;

fn quoted(text: &str) -> String {
    let mut output = String::new();
    crate::json::json_string(text, &mut output);
    output
}

fn read(root: &Path, path: &str) -> String {
    std::fs::read_to_string(root.join(path)).unwrap()
}

fn document(root: &Path, path: &str) -> Document {
    Document::parse(read(root, path), &Limits::default()).unwrap()
}

/// The span of the value at `pointer` of the file at `path`.
fn span(root: &Path, path: &str, pointer: &[Segment<'_>]) -> (usize, usize) {
    let document = document(root, path);
    let span = document.root().pointer(pointer).unwrap().span();
    (span.start, span.end)
}

/// The bytes that remove item `index` of the array at `pointer`, with a
/// comma beside it.
fn removal(root: &Path, path: &str, pointer: &[Segment<'_>], index: usize) -> (usize, usize) {
    let document = document(root, path);
    let items = document
        .root()
        .pointer(pointer)
        .unwrap()
        .as_array()
        .unwrap();
    let this = items[index].span();
    match (
        items.get(index + 1),
        index.checked_sub(1).map(|before| &items[before]),
    ) {
        (Some(next), _) => (this.start, next.span().start),
        (None, Some(before)) => (before.span().end, this.end),
        (None, None) => (this.start, this.end),
    }
}

/// The index of the entry of the array `key` of `path` whose `id` is `id`.
fn entry(root: &Path, path: &str, key: &str, id: &str) -> usize {
    document(root, path)
        .root()
        .get(key)
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .position(|item| item.get("id").and_then(Value::as_str) == Some(id))
        .unwrap()
}

#[allow(clippy::disallowed_methods)]
fn apply_edits(root: &Path, edits: &Edits) {
    for (path, list) in edits {
        let mut text = read(root, path);
        let mut list = list.clone();
        list.sort();
        for (start, end, replacement) in list.into_iter().rev() {
            text.replace_range(start..end, &replacement);
        }
        std::fs::write(root.join(path), text).unwrap();
    }
}

/// An item and the change made to it.
#[derive(Clone, Debug)]
pub(super) struct Case {
    pub kind: &'static str,
    pub id: String,
    /// The new ID of a rename, or `None` for a removal.
    pub to: Option<String>,
}

impl Case {
    pub fn arguments(&self) -> String {
        match &self.to {
            None => format!(
                r#"{{"kind":"{}","id":"{}","action":"remove"}}"#,
                self.kind, self.id
            ),
            Some(to) => format!(
                r#"{{"kind":"{}","id":"{}","action":"rename","to":"{to}"}}"#,
                self.kind, self.id
            ),
        }
    }
}

/// The local part of an ID written `owner/name`, or the ID.
fn local(id: &str) -> &str {
    id.split_once('/').map_or(id, |(_, name)| name)
}

/// Carries `case` out on the tree at `root`: the item's own files are
/// deleted, moved or edited; with `sites`, each is rewritten to the new
/// ID too. Only a rename has sites to rewrite.
#[allow(clippy::disallowed_methods, clippy::too_many_lines)]
fn apply(root: &Path, world: &World, case: &Case, sites: &[Site]) -> Edits {
    let mut edits = Edits::new();
    let mut edit = |path: &str, (start, end): (usize, usize), text: String| {
        edits
            .entry(path.to_owned())
            .or_default()
            .push((start, end, text));
    };
    let new = case.to.as_deref().map(local);
    for site in sites {
        edit(&site.path, (site.start, site.end), quoted(new.unwrap()));
    }
    let mut moved: Option<(PathBuf, PathBuf)> = None;
    let mut deleted: Option<PathBuf> = None;
    match case.kind {
        "definition" => {
            let path = format!("defs/{}.jsonc", case.id);
            match new {
                None => deleted = Some(root.join(&path)),
                Some(new) => {
                    edit(&path, span(root, &path, &[Segment::Key("id")]), quoted(new));
                    let kind = case.id.split_once('/').unwrap().0;
                    moved = Some((
                        root.join(&path),
                        root.join(format!("defs/{kind}/{new}.jsonc")),
                    ));
                }
            }
        }
        "plane" => {
            let directory = root.join("planes").join(&case.id);
            match new {
                None => deleted = Some(directory),
                Some(new) => {
                    let path = format!("planes/{}/plane.jsonc", case.id);
                    edit(&path, span(root, &path, &[Segment::Key("id")]), quoted(new));
                    moved = Some((directory, root.join("planes").join(new)));
                }
            }
        }
        "zone" | "anchor" | "stairwell" => {
            let (plane, id) = case.id.split_once('/').unwrap();
            let key = match case.kind {
                "zone" => "zones",
                "anchor" => "anchors",
                _ => "stairwells",
            };
            let path = format!("planes/{plane}/{key}.jsonc");
            let index = entry(root, &path, key, id);
            match new {
                None => edit(
                    &path,
                    removal(root, &path, &[Segment::Key(key)], index),
                    String::new(),
                ),
                Some(new) => edit(
                    &path,
                    span(
                        root,
                        &path,
                        &[Segment::Key(key), Segment::Index(index), Segment::Key("id")],
                    ),
                    quoted(new),
                ),
            }
        }
        "object" => {
            let location = world.object_location(&case.id).unwrap();
            let path = world.planes()[&location.plane].chunks[&location.chunk]
                .path
                .clone();
            let mut pointer = vec![Segment::Key("objects")];
            for (depth, step) in location.path.iter().enumerate() {
                if depth > 0 {
                    pointer.push(Segment::Key("inventory"));
                }
                pointer.push(Segment::Index(*step));
            }
            match new {
                None => {
                    let index = *location.path.last().unwrap();
                    let list = &pointer[..pointer.len() - 1];
                    edit(&path, removal(root, &path, list, index), String::new());
                }
                Some(new) => {
                    pointer.push(Segment::Key("uid"));
                    edit(&path, span(root, &path, &pointer), quoted(new));
                }
            }
        }
        "render_layer" => {
            let path = "profile/profile.jsonc";
            let index = world
                .profile()
                .render_layers
                .iter()
                .position(|layer| *layer == case.id)
                .unwrap();
            let pointer = [Segment::Key("render_layers")];
            match new {
                None => edit(path, removal(root, path, &pointer, index), String::new()),
                Some(new) => edit(
                    path,
                    span(root, path, &[pointer[0], Segment::Index(index)]),
                    quoted(new),
                ),
            }
        }
        "terrain" => {
            let transitions = world.profile().transitions.as_ref().unwrap();
            let path = transitions.path().to_owned();
            let index = transitions.terrain(&case.id).unwrap();
            match new {
                None => edit(
                    &path,
                    removal(root, &path, &[Segment::Key("terrains")], index),
                    String::new(),
                ),
                Some(new) => edit(
                    &path,
                    span(
                        root,
                        &path,
                        &[
                            Segment::Key("terrains"),
                            Segment::Index(index),
                            Segment::Key("id"),
                        ],
                    ),
                    quoted(new),
                ),
            }
        }
        other => panic!("no case for {other}"),
    }
    apply_edits(root, &edits);
    if let Some(path) = deleted {
        if path.is_dir() {
            std::fs::remove_dir_all(path).unwrap();
        } else {
            std::fs::remove_file(path).unwrap();
        }
    }
    if let Some((from, to)) = moved {
        std::fs::rename(from, to).unwrap();
    }
    edits
}

/// Where `site` is once `edits` are made to its file.
fn moved_by(edits: &Edits, site: &Site) -> Site {
    let shift = |position: usize| {
        let mut moved = position;
        for (start, end, replacement) in edits.get(&site.path).into_iter().flatten() {
            if *end <= position {
                moved = moved + replacement.len() - (end - start);
            }
        }
        moved
    };
    Site {
        start: shift(site.start),
        end: shift(site.end),
        ..site.clone()
    }
}

/// Whether `diagnostic` is the loader's report of `site` not resolving:
/// at its span, at a span that holds it (a palette entry is reported at
/// its grid), or on its file without a position (a default zone); a
/// plane the manifest lists without its directory is reported as that
/// plane's missing `plane.jsonc`.
fn reports(diagnostic: &Diagnostic, site: &Site, rename: &dyn Fn(&str) -> String) -> bool {
    if diagnostic.code == "source.missing_file" {
        return site.role == "manifest_plane"
            && diagnostic.path == format!("planes/{}/plane.jsonc", site.target);
    }
    diagnostic.code == "ref.unresolved"
        && diagnostic.path == rename(&site.path)
        && ((diagnostic.start, diagnostic.end) == (site.start, site.end)
            || (diagnostic.start <= site.start && site.end <= diagnostic.end)
            || (diagnostic.start, diagnostic.end, diagnostic.line) == (0, 0, 0))
}

/// Panics unless `diagnostics` are `sites`, one to one.
fn one_to_one(
    case: &Case,
    sites: &[Site],
    diagnostics: &[Diagnostic],
    rename: &dyn Fn(&str) -> String,
) {
    let mut unmatched: Vec<&Site> = sites.iter().collect();
    for diagnostic in diagnostics {
        // The exact span first, so a grid that holds two entries takes
        // each once.
        let found = unmatched
            .iter()
            .position(|site| {
                diagnostic.path == rename(&site.path)
                    && (diagnostic.start, diagnostic.end) == (site.start, site.end)
            })
            .or_else(|| {
                unmatched
                    .iter()
                    .position(|site| reports(diagnostic, site, rename))
            });
        match found {
            Some(position) => {
                unmatched.remove(position);
            }
            None => panic!(
                "{case:?}: the loader reports {diagnostic:?}, which impact did not predict; predicted {sites:#?}"
            ),
        }
    }
    assert!(
        unmatched.is_empty(),
        "{case:?}: impact predicted {unmatched:#?}, which the loader does not report; it reports {diagnostics:#?}"
    );
}

/// Every case on the tree at `root`: each item of each kind `impact`
/// takes, removed and renamed. A render layer that is also a ground
/// layer is left out, as `impact` refuses it.
fn cases(world: &World) -> Vec<Case> {
    let mut items: Vec<(&'static str, String)> = Vec::new();
    for (kind, id) in world.definitions().keys() {
        items.push(("definition", format!("{kind}/{id}")));
    }
    for (id, plane) in world.planes() {
        items.push(("plane", id.clone()));
        for zone in plane.zones.keys() {
            items.push(("zone", format!("{id}/{zone}")));
        }
        for anchor in plane.anchors.keys() {
            items.push(("anchor", format!("{id}/{anchor}")));
        }
        for stairwell in plane.stairwells.keys() {
            items.push(("stairwell", format!("{id}/{stairwell}")));
        }
    }
    for uid in world.object_uids().keys() {
        items.push(("object", uid.clone()));
    }
    let profile = world.profile();
    for layer in &profile.render_layers {
        if !profile.ground_layers.contains(layer) {
            items.push(("render_layer", layer.clone()));
        }
    }
    if let Some(transitions) = &profile.transitions {
        for terrain in transitions.terrains() {
            items.push(("terrain", terrain.id.clone()));
        }
    }
    let mut cases = Vec::new();
    for (kind, id) in items {
        let to = match id.split_once('/') {
            Some((owner, name)) => format!("{owner}/{name}-renamed"),
            None => format!("{id}-renamed"),
        };
        cases.push(Case {
            kind,
            id: id.clone(),
            to: None,
        });
        cases.push(Case {
            kind,
            id,
            to: Some(to),
        });
    }
    cases
}

/// A copy of `fixtures/basic` at `root` with more kinds of reference: a
/// second plane with anchors, zones (one the default, one a parent), a
/// zone grid, portals to its own anchors with and without their plane and
/// to the other plane's, an object on a render layer, carried objects with
/// uids, and a portal from the overworld to it.
#[allow(clippy::disallowed_methods)]
pub(super) fn generated(root: &Path) {
    copy_tree(&fixture("basic"), root);
    let write = |path: &str, text: &str| {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    write(
        "planes/vault/plane.jsonc",
        r#"{"format": "tilewright.plane/1", "id": "vault", "kind": "instanced", "chunk_size": 32, "default_zone": "keep"}"#,
    );
    write(
        "planes/vault/anchors.jsonc",
        r#"{"format": "tilewright.anchors/1", "anchors": [{"id": "door", "layer": 0, "at": [2, 2]}, {"id": "hatch", "layer": 0, "at": [3, 3]}]}"#,
    );
    write(
        "planes/vault/zones.jsonc",
        r#"{"format": "tilewright.zones/1", "zones": [{"id": "keep"}, {"id": "hall", "parent": "keep"}, {"id": "cell", "parent": "hall"}]}"#,
    );
    let mut rows = vec![format!("\"k{}\"", "h".repeat(31))];
    rows.extend(std::iter::repeat_n(format!("\"{}\"", "h".repeat(32)), 31));
    write(
        "planes/vault/layers/0/0_0.jsonc",
        &r#"{
  "format": "tilewright.chunk/1",
  "plane": "vault",
  "layer": 0,
  "chunk": [0, 0],
  "terrain": {"floor": {"palette": {"c": "cave_floor"}, "fill": "c"}, "zone": {"palette": {"h": "hall", "k": "cell"}, "rows": [ROWS]}},
  "objects": [
    {"uid": "vault.chest", "at": [1, 1], "proto": "chest", "fields": {"loot": "chest_common"}},
    {"at": [2, 2], "proto": "stairs_up", "portal": {"anchor": "door"}},
    {"at": [3, 2], "proto": "stairs_up", "portal": {"plane": "vault", "anchor": "hatch"}},
    {"at": [4, 2], "proto": "stairs_up", "portal": {"plane": "overworld", "anchor": "village.well"}},
    {"uid": "vault.lamp", "at": [5, 5], "proto": "coin", "layer": "item", "inventory": [{"uid": "vault.key", "proto": "coin"}, {"proto": "coin", "inventory": [{"uid": "vault.gem", "proto": "coin"}]}]}
  ]
}
"#
        .replace("ROWS", &rows.join(", ")),
    );
    write(
        "planes/overworld/layers/0/1_0.jsonc",
        r#"{
  "format": "tilewright.chunk/1",
  "plane": "overworld",
  "layer": 0,
  "chunk": [1, 0],
  "objects": [
    {"at": [0, 0], "proto": "stairs_down", "portal": {"plane": "vault", "anchor": "door"}},
    {"at": [1, 0], "proto": "stairs_down", "portal": {"anchor": "village.well"}},
    {"at": [2, 0], "proto": "tree", "layer": "living"}
  ]
}
"#,
    );
}

/// The differential on the tree at `root`; returns how many cases and
/// predicted references it checked, and the roles it met.
#[allow(clippy::disallowed_methods)]
fn differential(root: &Path) -> (usize, usize, BTreeMap<String, usize>) {
    let world = tilewright_source::load(root).expect("the tree loads");
    let mut server = ready(root);
    let mut checked = (0, 0, BTreeMap::new());
    for case in cases(&world) {
        let (sites, _) = predicted(&mut server, &case.arguments());
        for site in &sites {
            *checked.2.entry(site.role.clone()).or_default() += 1;
        }
        checked.0 += 1;
        checked.1 += sites.len();
        // The change alone: what breaks is what was predicted.
        let directory = tempfile::tempdir().unwrap();
        let copy = directory.path().join("tree");
        copy_tree(root, &copy);
        let edits = apply(&copy, &world, &case, &[]);
        let shifted: Vec<Site> = sites.iter().map(|site| moved_by(&edits, site)).collect();
        let plane_rename = case.kind == "plane" && case.to.is_some();
        if !plane_rename {
            // A renamed plane's directory is not the one the manifest
            // lists, so the loader does not reach its chunks; only the
            // full rename below is checked for it.
            let diagnostics = tilewright_source::load(&copy).err().unwrap_or_default();
            one_to_one(&case, &shifted, &diagnostics, &|path| path.to_owned());
        }
        // A rename with every predicted reference rewritten loads cleanly.
        if case.to.is_some() {
            let directory = tempfile::tempdir().unwrap();
            let copy = directory.path().join("tree");
            copy_tree(root, &copy);
            apply(&copy, &world, &case, &sites);
            if let Err(diagnostics) = tilewright_source::load(&copy) {
                panic!(
                    "{case:?}: rewritten as predicted, the tree does not load: {diagnostics:#?}"
                );
            }
        }
    }
    checked
}

#[test]
fn removing_or_renaming_breaks_exactly_the_predicted_references() {
    let directory = tempfile::tempdir().unwrap();
    let rich = directory.path().join("rich");
    generated(&rich);
    let mut roles: BTreeMap<String, usize> = BTreeMap::new();
    let mut total = (0, 0);
    for root in [fixture("basic"), fixture("terrain"), rich] {
        let (cases, sites, met) = differential(&root);
        total.0 += cases;
        total.1 += sites;
        for (role, count) in met {
            *roles.entry(role).or_default() += count;
        }
    }
    // Not vacuous: many cases, many references, and every role an item of
    // these kinds can be named by.
    assert!(total.0 >= 150 && total.1 >= 300, "{total:?}");
    for role in [
        "manifest_plane",
        "field",
        "zone_parent",
        "default_zone",
        "ground_palette",
        "zone_palette",
        "chunk_plane",
        "proto",
        "layer",
        "portal_plane",
        "portal_anchor",
        "stairwell_step",
        "terrain_proto",
        "transition_blend",
        "transition_tile",
    ] {
        assert!(
            roles.get(role).is_some_and(|count| *count >= 1),
            "{role}: {roles:?}"
        );
    }
    eprintln!(
        "impact differential: {} cases, {} predicted references, roles {roles:?}",
        total.0, total.1
    );
}
