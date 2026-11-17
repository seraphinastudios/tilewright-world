// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! `references` and `impact`: goldens on both fixtures; every target and
//! holder of the index paged through and checked against the index; spans
//! checked against the files on LF and CR LF copies; the roles filter and
//! its scan budget; the byte budget; cursors; the cascade, allowance and
//! free names; arguments; and a snapshot without an index.

use super::{
    at, call, copy_tree, fixture, golden, invalid_tree, read::crlf_copy, ready, result, send,
    structured, tool_error,
};
use crate::Server;
use crate::snapshot::{Snapshot, State};
use std::{collections::BTreeSet, fmt::Write as _, path::Path};
use tilewright_jsonc::{Document, Limits, Value};
use tilewright_source::index::{Holder, Reference, ReferenceIndex, Target};
use tilewright_source::{Diagnostic, World};

/// A target as `references` takes it: kind and ID.
fn spell_target(target: Target<'_>) -> (&'static str, String) {
    match target {
        Target::Definition { kind, id } => ("definition", format!("{kind}/{id}")),
        Target::DefinitionType(kind) => ("definition_type", kind.to_owned()),
        Target::Plane(plane) => ("plane", plane.to_owned()),
        Target::Anchor { plane, id } => ("anchor", format!("{plane}/{id}")),
        Target::Zone { plane, id } => ("zone", format!("{plane}/{id}")),
        Target::RenderLayer(layer) => ("render_layer", layer.to_owned()),
        Target::GroundLayer(layer) => ("ground_layer", layer.to_owned()),
        Target::Terrain(terrain) => ("terrain", terrain.to_owned()),
        Target::File(path) => ("file", path.to_owned()),
        _ => panic!("a target this test does not know"),
    }
}

/// A holder as `references` takes it, and as it writes it in `from`:
/// kind, the ID to call with, and the `id` the item gives.
fn spell_holder(
    world: &World,
    reference: &Reference<'_>,
) -> (&'static str, String, Option<String>) {
    let file = reference.file().to_owned();
    match reference.holder() {
        Holder::Manifest => ("manifest", file.clone(), Some(file)),
        Holder::Profile => ("profile", file.clone(), Some(file)),
        Holder::Schema { path } => ("schema", path.to_owned(), Some(path.to_owned())),
        Holder::Transitions => ("transitions", file.clone(), Some(file)),
        Holder::Definition { kind, id } => {
            let id = format!("{kind}/{id}");
            ("definition", id.clone(), Some(id))
        }
        Holder::Plane { plane } => ("plane", plane.to_owned(), Some(plane.to_owned())),
        Holder::Zone { plane, zone } => {
            let id = format!("{plane}/{zone}");
            ("zone", id.clone(), Some(id))
        }
        Holder::Stairwell { plane, stairwell } => {
            let id = format!("{plane}/{stairwell}");
            ("stairwell", id.clone(), Some(id))
        }
        Holder::Chunk { .. } => ("chunk", file.clone(), Some(file)),
        Holder::Object {
            plane,
            chunk,
            ordinal,
        } => {
            let object = world.planes()[plane].chunks[&chunk]
                .depth_first()
                .nth(ordinal)
                .unwrap();
            match &object.uid {
                Some(uid) => ("object", uid.clone(), Some(uid.clone())),
                None => ("object", format!("{file}#{ordinal}"), None),
            }
        }
        _ => panic!("a holder this test does not know"),
    }
}

/// What the test compares of an item: role, holder kind and ID, target
/// kind and ID, path and span.
type Key = (
    String,
    String,
    Option<String>,
    String,
    String,
    String,
    i64,
    i64,
);

fn key_of(item: &Value) -> Key {
    let text = |path: &[&str]| at(item, path).as_str().map(str::to_owned);
    (
        text(&["role"]).unwrap(),
        text(&["from", "kind"]).unwrap(),
        text(&["from", "id"]),
        text(&["to", "kind"]).unwrap(),
        text(&["to", "id"]).unwrap(),
        text(&["path"]).unwrap(),
        at(item, &["start"]).as_i64().unwrap(),
        at(item, &["end"]).as_i64().unwrap(),
    )
}

/// The key the test expects for `reference`, written from the index.
fn expected(world: &World, reference: &Reference<'_>) -> Key {
    let role = format!("{:?}", reference.role());
    // The role's name in snake case, from its Debug name.
    let name: String = role
        .split([' ', '{'])
        .next()
        .unwrap()
        .chars()
        .enumerate()
        .flat_map(|(index, character)| {
            let lower = character.to_ascii_lowercase();
            if character.is_ascii_uppercase() && index > 0 {
                vec!['_', lower]
            } else {
                vec![lower]
            }
        })
        .collect();
    let (holder, _, id) = spell_holder(world, reference);
    let (kind, target) = spell_target(reference.target());
    let span = reference.span();
    (
        name,
        holder.to_owned(),
        id,
        kind.to_owned(),
        target,
        reference.file().to_owned(),
        i64::try_from(span.start).unwrap(),
        i64::try_from(span.end).unwrap(),
    )
}

/// Every item of a `references` call, paged through with `limit`, with
/// each page's `total` and `exists`.
fn exhaust(server: &mut Server, arguments: &str, limit: usize) -> (Vec<Key>, i64, bool) {
    let inner = &arguments[1..arguments.len() - 1];
    let mut items = Vec::new();
    let mut cursor: Option<String> = None;
    let mut seen = None;
    loop {
        let mut call = format!("{{{inner},\"limit\":{limit}");
        if let Some(cursor) = &cursor {
            let _ = write!(call, ",\"cursor\":\"{cursor}\"");
        }
        call.push('}');
        let (reply, document) = result(server, "references", &call);
        assert!(reply.contains("\"isError\":false"), "{call}: {reply}");
        let page = structured(&document);
        let facts = (
            at(page, &["total"]).as_i64().unwrap(),
            at(page, &["exists"]).as_bool().unwrap(),
        );
        assert_eq!(*seen.get_or_insert(facts), facts, "{call}");
        assert_eq!(
            at(page, &["offset"]).as_i64(),
            Some(i64::try_from(items.len()).unwrap())
        );
        let page_items = at(page, &["references"]).as_array().unwrap();
        assert!(page_items.len() <= limit);
        items.extend(page_items.iter().map(key_of));
        if let Value::String { value, .. } = at(page, &["next_cursor"]) {
            cursor = Some(value.clone());
        } else {
            break;
        }
    }
    let (total, exists) = seen.unwrap();
    (items, total, exists)
}

fn index_of(server: &Server) -> (&World, &ReferenceIndex) {
    let State::Valid(valid) = &server.snapshot.state else {
        panic!("the tree loads");
    };
    (&valid.world, valid.index.as_ref().unwrap())
}

/// Pages through the references to every target and from every holder
/// of the index, and checks each page against the index.
fn every_reference(root: &Path) -> (usize, usize) {
    let mut server = ready(root);
    let loaded = ready(root);
    let (world, index) = index_of(&loaded);
    assert!(index.len() >= 2);
    let mut targets = BTreeSet::new();
    let mut holders = BTreeSet::new();
    for reference in index.iter() {
        targets.insert(spell_target(reference.target()));
        let (kind, id, _) = spell_holder(world, &reference);
        holders.insert((kind, id));
    }
    let mut checked = 0;
    for (kind, id) in &targets {
        let wanted: Vec<Key> = index
            .iter()
            .filter(|reference| spell_target(reference.target()) == (*kind, id.clone()))
            .map(|reference| expected(world, &reference))
            .collect();
        for limit in [500, 2] {
            let (items, total, exists) = exhaust(
                &mut server,
                &format!(r#"{{"kind":"{kind}","id":"{id}"}}"#),
                limit,
            );
            assert!(exists, "{kind} {id}");
            assert_eq!(items, wanted, "{kind} {id} {limit}");
            assert_eq!(usize::try_from(total).unwrap(), wanted.len());
        }
        checked += wanted.len();
    }
    for (kind, id) in &holders {
        let wanted: Vec<Key> = index
            .iter()
            .filter(|reference| {
                let (k, i, _) = spell_holder(world, reference);
                (k, i) == (*kind, id.clone())
            })
            .map(|reference| expected(world, &reference))
            .collect();
        // In source order: by file, then span.
        let mut sorted = wanted.clone();
        sorted.sort_by(|a, b| (&a.5, a.6, a.7).cmp(&(&b.5, b.6, b.7)));
        let (items, total, exists) = exhaust(
            &mut server,
            &format!(r#"{{"kind":"{kind}","id":"{id}","direction":"from"}}"#),
            3,
        );
        assert!(exists, "{kind} {id}");
        assert_eq!(items, sorted, "from {kind} {id}");
        assert_eq!(usize::try_from(total).unwrap(), wanted.len());
    }
    (targets.len(), checked)
}

#[test]
fn every_reference_is_listed_to_its_target_and_from_its_holder() {
    let directory = tempfile::tempdir().unwrap();
    let rich = directory.path().join("rich");
    super::impact::generated(&rich);
    let mut kinds = 0;
    for root in [fixture("basic"), fixture("terrain"), rich] {
        let (targets, references) = every_reference(&root);
        let server = ready(&root);
        let (_, index) = index_of(&server);
        // Every reference was listed once, by its target.
        assert_eq!(references, index.len());
        kinds += targets;
    }
    assert!(kinds >= 60, "{kinds}");
}

/// The structured result of a call that succeeded.
fn answer(server: &mut Server, tool: &str, arguments: &str) -> String {
    let (reply, document) = result(server, tool, arguments);
    assert!(
        reply.contains("\"isError\":false"),
        "{tool} {arguments}: {reply}"
    );
    document.canonical(structured(&document))
}

#[test]
#[allow(clippy::too_many_lines)]
fn golden_references_and_impact_on_both_fixtures() {
    let basic = [
        (
            "references",
            r#"{"kind":"definition","id":"proto/chest"}"#,
            "definition-proto-chest",
        ),
        (
            "references",
            r#"{"kind":"plane","id":"overworld","limit":3}"#,
            "plane-overworld",
        ),
        (
            "references",
            r#"{"kind":"anchor","id":"overworld/cave.entrance"}"#,
            "anchor",
        ),
        (
            "references",
            r#"{"kind":"zone","id":"overworld/island"}"#,
            "zone",
        ),
        (
            "references",
            r#"{"kind":"render_layer","id":"wall"}"#,
            "render_layer",
        ),
        (
            "references",
            r#"{"kind":"file","id":"profile/schema/proto.jsonc"}"#,
            "file",
        ),
        (
            "references",
            r#"{"kind":"definition_type","id":"loot"}"#,
            "definition_type",
        ),
        (
            "references",
            r#"{"kind":"stairwell","id":"overworld/cave.chute"}"#,
            "stairwell",
        ),
        (
            "references",
            r#"{"kind":"definition","id":"proto/missing"}"#,
            "missing",
        ),
        (
            "references",
            r#"{"kind":"object","id":"village.chest","direction":"from"}"#,
            "from-object",
        ),
        (
            "references",
            r#"{"kind":"object","id":"planes/overworld/layers/0/0_0.jsonc#8","direction":"from"}"#,
            "from-object-ordinal",
        ),
        (
            "references",
            r#"{"kind":"chunk","id":"planes/vault/layers/0/0_0.jsonc","direction":"from"}"#,
            "from-chunk",
        ),
        (
            "references",
            r#"{"kind":"manifest","id":"world.jsonc","direction":"from"}"#,
            "from-manifest",
        ),
        (
            "references",
            r#"{"kind":"profile","id":"profile/profile.jsonc","direction":"from"}"#,
            "from-profile",
        ),
        (
            "references",
            r#"{"kind":"zone","id":"overworld/village","direction":"from"}"#,
            "from-zone",
        ),
        (
            "references",
            r#"{"kind":"stairwell","id":"overworld/cave.chute","direction":"from","roles":["stairwell_step"]}"#,
            "from-stairwell",
        ),
        (
            "impact",
            r#"{"kind":"plane","id":"vault","action":"remove"}"#,
            "impact-plane-remove",
        ),
        (
            "impact",
            r#"{"kind":"plane","id":"overworld","action":"rename","to":"surface","limit":4}"#,
            "impact-plane-rename",
        ),
        (
            "impact",
            r#"{"kind":"definition","id":"proto/chest","action":"remove"}"#,
            "impact-definition",
        ),
        (
            "impact",
            r#"{"kind":"zone","id":"overworld/island","action":"rename","to":"overworld/isle"}"#,
            "impact-zone",
        ),
        (
            "impact",
            r#"{"kind":"object","id":"village.chest","action":"rename","to":"vault.chest"}"#,
            "impact-object",
        ),
        (
            "impact",
            r#"{"kind":"render_layer","id":"wall","action":"remove"}"#,
            "impact-render_layer",
        ),
    ];
    let terrain = [
        ("references", r#"{"kind":"terrain","id":"dirt"}"#, "terrain"),
        (
            "references",
            r#"{"kind":"ground_layer","id":"edge"}"#,
            "ground_layer",
        ),
        (
            "references",
            r#"{"kind":"transitions","id":"profile/transitions.jsonc","direction":"from","limit":5}"#,
            "from-transitions",
        ),
        (
            "impact",
            r#"{"kind":"terrain","id":"dirt","action":"remove"}"#,
            "impact-terrain",
        ),
    ];
    for (name, calls) in [("basic", &basic[..]), ("terrain", &terrain[..])] {
        let mut server = ready(&fixture(name));
        for (tool, arguments, label) in calls {
            let (reply, document) = result(&mut server, tool, arguments);
            assert!(reply.contains("\"isError\":false"), "{arguments}: {reply}");
            let page = structured(&document);
            if *label != "missing" && *label != "stairwell" {
                // Not vacuous.
                let list = if *tool == "impact" {
                    "breaking"
                } else {
                    "references"
                };
                assert!(
                    !at(page, &[list]).as_array().unwrap().is_empty() || *label == "impact-object",
                    "{label}"
                );
            }
            golden(&format!("{name}-{tool}-{label}"), &reply);
        }
    }
}

#[test]
fn spans_name_their_targets_with_either_line_ending() {
    for name in ["basic", "terrain"] {
        let directory = tempfile::tempdir().unwrap();
        let crlf = directory.path().join("crlf");
        crlf_copy(&fixture(name), &crlf);
        for root in [fixture(name), crlf] {
            let server = ready(&root);
            let (_, index) = index_of(&server);
            let mut server = ready(&root);
            let mut targets = BTreeSet::new();
            for reference in index.iter() {
                targets.insert(spell_target(reference.target()));
            }
            let mut checked = 0;
            for (kind, id) in targets {
                let (items, _, _) = exhaust(
                    &mut server,
                    &format!(r#"{{"kind":"{kind}","id":"{id}"}}"#),
                    500,
                );
                for (.., path, start, end) in items {
                    let text = std::fs::read_to_string(root.join(&path)).unwrap();
                    let written =
                        &text[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()];
                    let value = Document::parse(written, &Limits::default()).unwrap();
                    let value = value.root().as_str().unwrap().to_owned();
                    // A path is written relative to the document that
                    // names it; anything else is the target's last part.
                    let last = id.rsplit('/').next().unwrap();
                    assert!(
                        value == last || (kind == "file" && id.ends_with(&value)),
                        "{path} {start}: {written} for {kind} {id}"
                    );
                    checked += 1;
                }
            }
            assert!(checked >= 20, "{checked}");
        }
    }
}

/// A copy of `fixtures/basic` with `chunks` chunks of `objects` trees on
/// layer 7 of the overworld.
#[allow(clippy::disallowed_methods)]
fn many(root: &Path, chunks: usize, objects: usize) {
    copy_tree(&fixture("basic"), root);
    let layer = root.join("planes/overworld/layers/7");
    std::fs::create_dir_all(&layer).unwrap();
    for cx in 0..chunks {
        let trees: Vec<String> = (0..objects)
            .map(|index| {
                format!(
                    r#"{{"at": [{}, {}], "proto": "tree", "fields": {{"loot": "chest_common"}}}}"#,
                    index % 32,
                    (index / 32) % 32
                )
            })
            .collect();
        std::fs::write(
            layer.join(format!("{cx}_0.jsonc")),
            format!(
                "{{\"format\": \"tilewright.chunk/1\", \"plane\": \"overworld\", \"layer\": 7, \"chunk\": [{cx}, 0], \"objects\": [\n{}\n]}}\n",
                trees.join(",\n")
            ),
        )
        .unwrap();
    }
}

#[test]
fn a_roles_filter_examines_at_most_its_scan_limit() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("tree");
    many(&root, 4, 300);
    let mut server = ready(&root);
    // Every tree's proto, and the four basic ones before them.
    let (all, total, _) = exhaust(
        &mut server,
        r#"{"kind":"definition","id":"proto/tree"}"#,
        500,
    );
    assert_eq!(usize::try_from(total).unwrap(), all.len());
    assert!(all.len() > 1200);
    let fields = r#""kind":"definition","id":"loot/chest_common","roles":["field"]"#;
    let (wanted, ..) = exhaust(&mut server, &format!("{{{fields}}}"), 500);
    assert!(wanted.len() > 1200 && wanted.iter().all(|key| key.0 == "field"));
    // A filter that matches only the last few: the pages before are empty
    // and stop at the scan budget, with a cursor to go on.
    let protos = r#""kind":"definition","id":"proto/tree","roles":["stairwell_step","layer"]"#;
    let (_, document) = result(
        &mut server,
        "references",
        &format!("{{{protos},\"scan_limit\":100}}"),
    );
    let page = structured(&document);
    assert_eq!(at(page, &["scanned"]).as_i64(), Some(100));
    assert_eq!(at(page, &["scan_budget_reached"]).as_bool(), Some(true));
    assert!(at(page, &["references"]).as_array().unwrap().is_empty());
    let mut cursor = at(page, &["next_cursor"]).as_str().unwrap().to_owned();
    let mut pages = 1;
    loop {
        let (_, document) = result(
            &mut server,
            "references",
            &format!("{{{protos},\"scan_limit\":100,\"cursor\":\"{cursor}\"}}"),
        );
        let page = structured(&document);
        assert!(at(page, &["scanned"]).as_i64().unwrap() <= 100);
        assert!(at(page, &["references"]).as_array().unwrap().is_empty());
        pages += 1;
        match at(page, &["next_cursor"]).as_str() {
            Some(next) => cursor = next.to_owned(),
            None => break,
        }
    }
    assert_eq!(pages, all.len().div_ceil(100));
    // Without the filter, scan_limit is refused.
    let (_, document) = result(
        &mut server,
        "references",
        r#"{"kind":"definition","id":"proto/tree","scan_limit":5}"#,
    );
    assert_eq!(tool_error(&document), "arguments.invalid");
    // Two roles: the union, in order.
    let (both, ..) = exhaust(
        &mut server,
        r#"{"kind":"definition","id":"loot/chest_common","roles":["field","proto","field"]}"#,
        7,
    );
    assert_eq!(both, wanted);
}

#[test]
fn pages_stop_at_the_byte_budget_as_sent() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("tree");
    many(&root, 2, 600);
    let mut server = ready(&root);
    let arguments = r#"{"kind":"definition","id":"proto/tree","limit":500}"#;
    let (_, document) = result(&mut server, "references", arguments);
    let page = structured(&document);
    assert_eq!(at(page, &["byte_budget_reached"]).as_bool(), Some(true));
    let returned = at(page, &["references"]).as_array().unwrap().len();
    assert!(returned > 100 && returned < 500, "{returned}");
    let (all, total, _) = exhaust(
        &mut server,
        r#"{"kind":"definition","id":"proto/tree"}"#,
        500,
    );
    assert_eq!(usize::try_from(total).unwrap(), all.len());
    let unique: BTreeSet<&Key> = all.iter().collect();
    assert_eq!(unique.len(), all.len());
    // impact pages the same way, and caps the item's own files.
    many(&root.join("../wide"), 130, 1);
    let mut server = ready(&root.join("../wide"));
    let (_, document) = result(
        &mut server,
        "impact",
        r#"{"kind":"plane","id":"overworld","action":"rename","to":"surface","limit":500}"#,
    );
    let page = structured(&document);
    assert_eq!(at(page, &["own_files_truncated"]).as_bool(), Some(true));
    assert_eq!(
        at(page, &["own_files"]).as_array().unwrap().len(),
        crate::references::MAXIMUM_OWN_FILES
    );
    assert!(at(page, &["own_files_total"]).as_i64().unwrap() > 130);
    let (sites, _) = super::impact::predicted(
        &mut server,
        r#"{"kind":"plane","id":"overworld","action":"rename","to":"surface"}"#,
    );
    // Every chunk's plane, the manifest's entry and the portal to it.
    assert!(sites.len() > 130);
    let (_, document) = result(
        &mut server,
        "impact",
        r#"{"kind":"definition","id":"proto/tree","action":"remove","limit":500}"#,
    );
    assert_eq!(
        at(structured(&document), &["byte_budget_reached"]).as_bool(),
        Some(false)
    );
    let mut server = ready(&root);
    let (_, document) = result(
        &mut server,
        "impact",
        r#"{"kind":"definition","id":"proto/tree","action":"remove","limit":500}"#,
    );
    assert_eq!(
        at(structured(&document), &["byte_budget_reached"]).as_bool(),
        Some(true)
    );
}

/// The canonical JSON of the member `key` of the structured result of
/// `impact` with `arguments`.
fn impact_member(server: &mut Server, arguments: &str, key: &str) -> String {
    let (reply, document) = result(server, "impact", arguments);
    assert!(reply.contains("\"isError\":false"), "{arguments}: {reply}");
    document.canonical(at(structured(&document), &[key]))
}

#[test]
#[allow(clippy::too_many_lines)]
fn impact_gives_the_cascade_the_allowance_and_whether_a_name_is_free() {
    let directory = tempfile::tempdir().unwrap();
    let rich = directory.path().join("rich");
    super::impact::generated(&rich);
    let mut server = ready(&rich);
    let remove = r#"{"kind":"plane","id":"vault","action":"remove"}"#;
    let rename = r#"{"kind":"plane","id":"vault","action":"rename","to":"crypt"}"#;
    let objects = ready(&rich);
    let (world, _) = index_of(&objects);
    let vault = &world.planes()["vault"];
    let carried: usize = vault
        .chunks
        .values()
        .map(|chunk| chunk.depth_first().count())
        .sum();
    assert!(carried > vault.chunks.len());
    let cascade = format!(
        r#"{{"zones":3,"anchors":2,"stairwells":0,"chunks":1,"objects":{carried},"incoming":1}}"#
    );
    assert_eq!(impact_member(&mut server, remove, "cascade"), cascade);
    assert_eq!(impact_member(&mut server, rename, "cascade"), cascade);
    // The vault's portal that omits its plane survives a rename; the one
    // that names its plane is a breaking reference.
    assert_eq!(
        impact_member(&mut server, rename, "surviving_implicit"),
        "1"
    );
    assert_eq!(
        impact_member(&mut server, remove, "surviving_implicit"),
        "null"
    );
    assert_eq!(
        impact_member(&mut server, rename, "own_files"),
        r#"["planes/vault/anchors.jsonc","planes/vault/layers/0/0_0.jsonc","planes/vault/plane.jsonc","planes/vault/zones.jsonc"]"#
    );
    assert_eq!(
        impact_member(&mut server, remove, "diff_allowance"),
        r#"{"list":"remove","entries":[{"kind":"plane","id":"vault"}],"entries_total":1,"entries_truncated":false,"entries_offset":0,"entries_next_cursor":null}"#
    );
    assert_eq!(
        impact_member(&mut server, rename, "diff_allowance"),
        r#"{"list":"rename","entries":[{"kind":"plane","from":"vault","to":"crypt"}],"entries_total":1,"entries_truncated":false,"entries_offset":0,"entries_next_cursor":null}"#
    );
    assert_eq!(impact_member(&mut server, rename, "to_free"), "true");
    assert_eq!(impact_member(&mut server, remove, "to_free"), "null");
    let taken = r#"{"kind":"plane","id":"vault","action":"rename","to":"overworld"}"#;
    assert_eq!(impact_member(&mut server, taken, "to_free"), "false");
    // Removal leaves out what goes with the plane; rename breaks the
    // plane's own chunk and the portal that names the plane.
    let (removed, _) = super::impact::predicted(&mut server, remove);
    assert!(
        removed
            .iter()
            .all(|site| !site.path.starts_with("planes/vault/"))
    );
    assert!(removed.iter().any(|site| site.role == "portal_anchor"));
    let (renamed, _) = super::impact::predicted(&mut server, rename);
    assert!(renamed.iter().any(|site| site.role == "chunk_plane"));
    assert!(renamed.iter().all(|site| site.role != "portal_anchor"));
    // Each kind's own files and allowance.
    for (arguments, files, allowance) in [
        (
            r#"{"kind":"definition","id":"proto/chest","action":"remove"}"#,
            r#"["defs/proto/chest.jsonc"]"#,
            r#"{"list":"remove","entries":[{"kind":"definition","id":"proto/chest"}],"entries_total":1,"entries_truncated":false,"entries_offset":0,"entries_next_cursor":null}"#,
        ),
        (
            r#"{"kind":"zone","id":"vault/hall","action":"rename","to":"vault/corridor"}"#,
            r#"["planes/vault/zones.jsonc"]"#,
            r#"{"list":"rename","entries":[{"kind":"zone","from":"vault/hall","to":"vault/corridor"}],"entries_total":1,"entries_truncated":false,"entries_offset":0,"entries_next_cursor":null}"#,
        ),
        (
            r#"{"kind":"anchor","id":"vault/door","action":"remove"}"#,
            r#"["planes/vault/anchors.jsonc"]"#,
            r#"{"list":"remove","entries":[{"kind":"anchor","id":"vault/door"}],"entries_total":1,"entries_truncated":false,"entries_offset":0,"entries_next_cursor":null}"#,
        ),
        (
            r#"{"kind":"stairwell","id":"overworld/cave.chute","action":"remove"}"#,
            r#"["planes/overworld/stairwells.jsonc"]"#,
            r#"{"list":"remove","entries":[{"kind":"stairwell","id":"overworld/cave.chute"}],"entries_total":1,"entries_truncated":false,"entries_offset":0,"entries_next_cursor":null}"#,
        ),
        (
            r#"{"kind":"object","id":"vault.key","action":"rename","to":"vault.key2"}"#,
            r#"["planes/vault/layers/0/0_0.jsonc"]"#,
            r#"{"list":"rename","entries":[{"kind":"object","from":"vault.key","to":"vault.key2"}],"entries_total":1,"entries_truncated":false,"entries_offset":0,"entries_next_cursor":null}"#,
        ),
        (
            r#"{"kind":"render_layer","id":"item","action":"remove"}"#,
            r#"["profile/profile.jsonc"]"#,
            r#"{"list":"remove","entries":[{"kind":"render_layer","id":"item"}],"entries_total":1,"entries_truncated":false,"entries_offset":0,"entries_next_cursor":null}"#,
        ),
    ] {
        assert_eq!(
            impact_member(&mut server, arguments, "own_files"),
            files,
            "{arguments}"
        );
        assert_eq!(
            impact_member(&mut server, arguments, "diff_allowance"),
            allowance
        );
        assert_eq!(impact_member(&mut server, arguments, "cascade"), "null");
        assert_eq!(impact_member(&mut server, arguments, "exists"), "true");
    }
    // An object removed takes what it carries, each uid with its own
    // allowlist entry; a rename changes only its own uid.
    let lamp = r#"{"kind":"object","id":"vault.lamp","action":"remove"}"#;
    assert_eq!(
        impact_member(&mut server, lamp, "carried"),
        r#"[{"uid":"vault.key","proto":"coin","depth":1},{"uid":null,"proto":"coin","depth":1},{"uid":"vault.gem","proto":"coin","depth":2}]"#
    );
    assert_eq!(impact_member(&mut server, lamp, "carried_total"), "3");
    assert_eq!(
        impact_member(&mut server, lamp, "diff_allowance"),
        r#"{"list":"remove","entries":[{"kind":"object","id":"vault.lamp"},{"kind":"object","id":"vault.key"},{"kind":"object","id":"vault.gem"}],"entries_total":3,"entries_truncated":false,"entries_offset":0,"entries_next_cursor":null}"#
    );
    assert_eq!(impact_member(&mut server, lamp, "breaking_total"), "0");
    let renamed = r#"{"kind":"object","id":"vault.lamp","action":"rename","to":"vault.torch"}"#;
    assert_eq!(impact_member(&mut server, renamed, "carried"), "null");
    assert_eq!(impact_member(&mut server, renamed, "carried_total"), "null");
    // What the snapshot does not hold breaks nothing and has no files.
    let missing = r#"{"kind":"definition","id":"proto/nothing","action":"remove"}"#;
    assert_eq!(impact_member(&mut server, missing, "exists"), "false");
    assert_eq!(impact_member(&mut server, missing, "breaking_total"), "0");
    assert_eq!(impact_member(&mut server, missing, "own_files"), "[]");
    assert_eq!(
        impact_member(&mut server, missing, "diff_allowance"),
        "null"
    );
    // A terrain is not a kind tilewright diff compares.
    let mut terrain = ready(&fixture("terrain"));
    assert_eq!(
        impact_member(
            &mut terrain,
            r#"{"kind":"terrain","id":"sand","action":"remove"}"#,
            "diff_allowance"
        ),
        "null"
    );
    assert_eq!(
        impact_member(
            &mut terrain,
            r#"{"kind":"terrain","id":"sand","action":"remove"}"#,
            "own_files"
        ),
        r#"["profile/transitions.jsonc"]"#
    );
    // The fixed text on what is not covered.
    assert_eq!(impact_member(&mut server, missing, "not_covered"), {
        let mut text = String::new();
        crate::json_string(crate::references::NOT_COVERED, &mut text);
        text
    });
    // A render layer that is also a ground layer is refused.
    for action in [r#""action":"remove""#, r#""action":"rename","to":"ground""#] {
        let (_, document) = result(
            &mut server,
            "impact",
            &format!(r#"{{"kind":"render_layer","id":"floor",{action}}}"#),
        );
        assert_eq!(tool_error(&document), "impact.not_covered");
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn arguments_are_checked_before_the_revision_and_the_tree() {
    let mut server = ready(&fixture("basic"));
    let long = "a".repeat(2049);
    for (tool, arguments) in [
        ("references", r#"{"id":"overworld"}"#.to_owned()),
        ("references", r#"{"kind":"plane"}"#.to_owned()),
        ("references", r#"{"kind":7,"id":"overworld"}"#.to_owned()),
        (
            "references",
            r#"{"kind":"planes","id":"overworld"}"#.to_owned(),
        ),
        (
            "references",
            r#"{"kind":"chunk","id":"planes/vault/layers/0/0_0.jsonc"}"#.to_owned(),
        ),
        (
            "references",
            r#"{"kind":"manifest","id":"world.jsonc","direction":"to"}"#.to_owned(),
        ),
        (
            "references",
            r#"{"kind":"terrain","id":"sand","direction":"from"}"#.to_owned(),
        ),
        (
            "references",
            r#"{"kind":"file","id":"world.jsonc","direction":"from"}"#.to_owned(),
        ),
        (
            "references",
            r#"{"kind":"plane","id":"overworld","direction":"up"}"#.to_owned(),
        ),
        ("references", r#"{"kind":"zone","id":"island"}"#.to_owned()),
        (
            "references",
            r#"{"kind":"zone","id":"overworld/Island"}"#.to_owned(),
        ),
        (
            "references",
            r#"{"kind":"definition","id":"proto/"}"#.to_owned(),
        ),
        (
            "references",
            r#"{"kind":"plane","id":"over/world"}"#.to_owned(),
        ),
        (
            "references",
            r#"{"kind":"file","id":"../world.jsonc"}"#.to_owned(),
        ),
        (
            "references",
            r#"{"kind":"manifest","id":"profile/profile.jsonc","direction":"from"}"#.to_owned(),
        ),
        (
            "references",
            r#"{"kind":"object","id":"planes/vault/layers/0/0_0.jsonc#01","direction":"from"}"#
                .to_owned(),
        ),
        (
            "references",
            r#"{"kind":"object","id":"planes/vault/layers/0/0_0.jsonc#","direction":"from"}"#
                .to_owned(),
        ),
        (
            "references",
            r#"{"kind":"object","id":"x#1","direction":"from"}"#.to_owned(),
        ),
        (
            "references",
            r#"{"kind":"plane","id":"overworld","roles":[]}"#.to_owned(),
        ),
        (
            "references",
            r#"{"kind":"plane","id":"overworld","roles":["chunk"]}"#.to_owned(),
        ),
        (
            "references",
            r#"{"kind":"plane","id":"overworld","roles":"proto"}"#.to_owned(),
        ),
        (
            "references",
            format!(
                r#"{{"kind":"plane","id":"overworld","roles":[{}]}}"#,
                vec![r#""proto""#; 25].join(",")
            ),
        ),
        ("references", format!(r#"{{"kind":"plane","id":"{long}"}}"#)),
        (
            "references",
            r#"{"kind":"plane","id":"overworld","limit":0}"#.to_owned(),
        ),
        (
            "references",
            r#"{"kind":"plane","id":"overworld","to":"x"}"#.to_owned(),
        ),
        ("impact", r#"{"kind":"plane","id":"vault"}"#.to_owned()),
        (
            "impact",
            r#"{"kind":"plane","id":"vault","action":"delete"}"#.to_owned(),
        ),
        (
            "impact",
            r#"{"kind":"plane","id":"vault","action":"rename"}"#.to_owned(),
        ),
        (
            "impact",
            r#"{"kind":"plane","id":"vault","action":"remove","to":"x"}"#.to_owned(),
        ),
        (
            "impact",
            r#"{"kind":"plane","id":"vault","action":"rename","to":"vault"}"#.to_owned(),
        ),
        (
            "impact",
            r#"{"kind":"plane","id":"vault","action":"rename","to":"Vault"}"#.to_owned(),
        ),
        (
            "impact",
            r#"{"kind":"zone","id":"overworld/island","action":"rename","to":"vault/island"}"#
                .to_owned(),
        ),
        (
            "impact",
            r#"{"kind":"definition","id":"proto/chest","action":"rename","to":"loot/chest"}"#
                .to_owned(),
        ),
        (
            "impact",
            r#"{"kind":"file","id":"world.jsonc","action":"remove"}"#.to_owned(),
        ),
        (
            "impact",
            r#"{"kind":"definition_type","id":"proto","action":"remove"}"#.to_owned(),
        ),
        (
            "impact",
            r#"{"kind":"ground_layer","id":"floor","action":"remove"}"#.to_owned(),
        ),
        (
            "impact",
            r#"{"kind":"object","id":"planes/vault/layers/0/0_0.jsonc#0","action":"remove"}"#
                .to_owned(),
        ),
        (
            "impact",
            r#"{"kind":"plane","id":"vault","action":"remove","roles":["proto"]}"#.to_owned(),
        ),
        (
            "impact",
            r#"{"kind":"plane","id":"vault","action":"remove","scan_limit":5}"#.to_owned(),
        ),
        (
            "impact",
            format!(r#"{{"kind":"plane","id":"vault","action":"rename","to":"{long}"}}"#),
        ),
    ] {
        // With a revision the snapshot is not at and on a tree that does
        // not load, arguments are still checked first.
        let other = "0".repeat(64);
        let with = format!(
            r#"{},"revision":"{other}"}}"#,
            &arguments[..arguments.len() - 1]
        );
        let (_, document) = result(&mut server, tool, &with);
        assert_eq!(
            tool_error(&document),
            "arguments.invalid",
            "{tool} {arguments}"
        );
    }
    // At the schema's bounds: an ID of 2048 bytes is read, and found
    // malformed, as no ID is so long; one more byte is refused unread.
    let error = |server: &mut Server, id: &str| {
        let (_, document) = result(
            server,
            "references",
            &format!(r#"{{"kind":"object","id":"{id}","direction":"from"}}"#),
        );
        let result = at(document.root(), &["result"]);
        at(result, &["content", "0", "text"])
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert!(error(&mut server, &"a".repeat(2048)).contains("is not written as IDs"));
    assert!(error(&mut server, &"a".repeat(2049)).contains("at most 2048 bytes"));
    // Documents exist when the manifest or profile names them.
    for (kind, id, exists) in [
        ("schema", "profile/schema/loot.jsonc", true),
        ("schema", "profile/schema/none.jsonc", false),
        ("schema", "profile/profile.jsonc", false),
        ("profile", "profile/profile.jsonc", true),
        ("profile", "profile/schema/loot.jsonc", false),
        ("transitions", "profile/transitions.jsonc", false),
    ] {
        let text = answer(
            &mut server,
            "references",
            &format!(r#"{{"kind":"{kind}","id":"{id}","direction":"from"}}"#),
        );
        assert!(
            text.contains(&format!(r#""exists":{exists}"#)),
            "{kind} {id}: {text}"
        );
    }
    // A chunk path, whose layer and coordinates may be negative, is one.
    let text = answer(
        &mut server,
        "references",
        r#"{"kind":"object","id":"planes/overworld/layers/-1/0_0.jsonc#0","direction":"from"}"#,
    );
    assert!(text.contains(r#""exists":true"#), "{text}");
    for id in [
        "planes/overworld/layers/-1/-0_0.jsonc#0",
        "planes/overworld/layers/-1/0_0.jsonc#9",
        "planes/nowhere/layers/0/0_0.jsonc#0",
    ] {
        let text = answer(
            &mut server,
            "references",
            &format!(r#"{{"kind":"object","id":"{id}","direction":"from"}}"#),
        );
        assert!(text.contains(r#""exists":false"#), "{id}: {text}");
    }
    let roles = vec![r#""proto""#; 24].join(",");
    let text = answer(
        &mut server,
        "references",
        &format!(r#"{{"kind":"definition","id":"proto/chest","roles":[{roles}]}}"#),
    );
    assert!(text.contains(r#""role":"proto""#));
}

#[test]
fn the_reference_tools_check_the_revision_and_need_a_tree_that_loads() {
    let tree = invalid_tree();
    let mut server = ready(tree.path());
    for (tool, arguments) in [
        ("references", r#"{"kind":"plane","id":"overworld"}"#),
        (
            "impact",
            r#"{"kind":"plane","id":"overworld","action":"remove"}"#,
        ),
    ] {
        let (reply, document) = result(&mut server, tool, arguments);
        assert_eq!(tool_error(&document), "tree.invalid");
        golden(&format!("invalid-{tool}"), &reply);
    }
    let mut server = ready(&fixture("basic"));
    let revision = server.revision().unwrap().to_owned();
    for (tool, arguments) in [
        ("references", r#""kind":"plane","id":"overworld""#),
        (
            "impact",
            r#""kind":"plane","id":"overworld","action":"remove""#,
        ),
    ] {
        let (reply, _) = result(
            &mut server,
            tool,
            &format!(r#"{{{arguments},"revision":"{revision}"}}"#),
        );
        assert!(reply.contains("\"isError\":false"));
        let (_, document) = result(
            &mut server,
            tool,
            &format!(r#"{{{arguments},"revision":"{}"}}"#, "0".repeat(64)),
        );
        assert_eq!(tool_error(&document), "revision.changed");
    }
}

#[test]
#[allow(clippy::disallowed_methods)]
fn reference_cursors_are_bound_to_revision_tool_and_arguments() {
    let directory = tempfile::tempdir().unwrap();
    copy_tree(&fixture("basic"), directory.path());
    let mut server = ready(directory.path());
    let first = |server: &mut Server, tool: &str, arguments: &str| {
        let (_, document) = result(server, tool, arguments);
        at(structured(&document), &["next_cursor"])
            .as_str()
            .unwrap()
            .to_owned()
    };
    let references = r#""kind":"plane","id":"overworld""#;
    let cursor = first(
        &mut server,
        "references",
        &format!("{{{references},\"limit\":2}}"),
    );
    let impact = r#""kind":"plane","id":"overworld","action":"rename","to":"surface""#;
    let impact_cursor = first(&mut server, "impact", &format!("{{{impact},\"limit\":2}}"));
    for (tool, arguments, cursor) in [
        ("references", r#""kind":"plane","id":"vault""#, &cursor),
        (
            "references",
            r#""kind":"plane","id":"overworld","direction":"from""#,
            &cursor,
        ),
        (
            "references",
            r#""kind":"plane","id":"overworld","roles":["chunk_plane"]"#,
            &cursor,
        ),
        ("impact", impact, &cursor),
        ("references", references, &impact_cursor),
        (
            "impact",
            r#""kind":"plane","id":"overworld","action":"rename","to":"top""#,
            &impact_cursor,
        ),
        (
            "impact",
            r#""kind":"plane","id":"overworld","action":"remove""#,
            &impact_cursor,
        ),
    ] {
        let (_, document) = result(
            &mut server,
            tool,
            &format!(r#"{{{arguments},"cursor":"{cursor}"}}"#),
        );
        assert_eq!(
            tool_error(&document),
            "cursor.invalid",
            "{tool} {arguments}"
        );
    }
    // Past the end, or at zero.
    let parts: Vec<&str> = cursor.rsplitn(2, '.').collect();
    for offset in ["0", "999"] {
        let (_, document) = result(
            &mut server,
            "references",
            &format!(r#"{{{references},"cursor":"{}.{offset}"}}"#, parts[1]),
        );
        assert_eq!(tool_error(&document), "cursor.invalid");
    }
    // Good until the tree changes.
    for (tool, arguments, cursor) in [
        ("references", references, &cursor),
        ("impact", impact, &impact_cursor),
    ] {
        let (_, document) = result(
            &mut server,
            tool,
            &format!(r#"{{{arguments},"cursor":"{cursor}"}}"#),
        );
        assert_eq!(at(structured(&document), &["offset"]).as_i64(), Some(2));
    }
    let path = directory.path().join("defs/proto/coin.jsonc");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("{text}\n")).unwrap();
    let _ = result(&mut server, "refresh", "{}");
    for (tool, arguments, cursor) in [
        ("references", references, &cursor),
        ("impact", impact, &impact_cursor),
    ] {
        let (_, document) = result(
            &mut server,
            tool,
            &format!(r#"{{{arguments},"cursor":"{cursor}"}}"#),
        );
        assert_eq!(tool_error(&document), "cursor.stale");
    }
}

#[test]
fn a_tree_too_large_to_index_is_served_without_an_index() {
    let budget = Diagnostic {
        path: ".".to_owned(),
        start: 0,
        end: 0,
        line: 0,
        column: 0,
        code: "source.budget",
        message: "the tree has more references, strings or holders than an index can number"
            .to_owned(),
    };
    let mut server = ready(&fixture("basic"));
    let revision = server.revision().unwrap().to_owned();
    server.snapshot = Snapshot::load_with(&fixture("basic"), |_| Err(vec![budget.clone()]));
    // The plain load serves the same revision, without an index.
    assert_eq!(server.revision(), Some(revision.as_str()));
    for (tool, arguments) in [
        ("references", r#"{"kind":"plane","id":"overworld"}"#),
        (
            "impact",
            r#"{"kind":"plane","id":"overworld","action":"remove"}"#,
        ),
    ] {
        let (_, document) = result(&mut server, tool, arguments);
        assert_eq!(tool_error(&document), "index.unavailable");
    }
    let summary = answer(&mut server, "world_summary", "{}");
    assert!(summary.contains(&revision));
    // Any other failure of the indexed load is the tree's own: not loaded
    // again.
    let other = Diagnostic {
        code: "ref.unresolved",
        ..budget.clone()
    };
    server.snapshot = Snapshot::load_with(&fixture("basic"), |_| Err(vec![other.clone()]));
    assert_eq!(server.revision(), None);
    assert_eq!(server.snapshot.diagnostics(), [other]);
    // A budget diagnostic that the plain load gives too is the tree's.
    let tree = invalid_tree();
    server.snapshot = Snapshot::load_with(tree.path(), |_| Err(vec![budget.clone()]));
    assert_eq!(server.revision(), None);
    assert_eq!(server.snapshot.diagnostics().len(), 7);
    // refresh loads with the index again.
    let mut server = ready(&fixture("basic"));
    server.snapshot = Snapshot::load_with(&fixture("basic"), |_| Err(vec![budget]));
    let _ = result(&mut server, "refresh", "{}");
    let text = answer(
        &mut server,
        "references",
        r#"{"kind":"plane","id":"overworld"}"#,
    );
    assert!(text.contains(r#""exists":true"#));
}

/// Calls `references` and `impact` on every item of every kind, for the
/// tests that require every tool to leave the tree as it was.
pub(super) fn every_reference_tool(server: &mut Server) {
    for kind in [
        "definition",
        "plane",
        "zone",
        "anchor",
        "stairwell",
        "render_layer",
    ] {
        let (_, document) = result(
            server,
            "search_ids",
            &format!(r#"{{"kind":"{kind}","limit":500}}"#),
        );
        let ids: Vec<String> = at(structured(&document), &["ids"])
            .as_array()
            .unwrap()
            .iter()
            .map(|id| id.as_str().unwrap().to_owned())
            .collect();
        for id in ids {
            let arguments = format!(r#"{{"kind":"{kind}","id":"{id}","limit":2}}"#);
            let (reply, _) = result(server, "references", &arguments);
            assert!(reply.contains("\"isError\":false"), "{reply}");
            if kind != "render_layer" {
                let arguments = format!(r#"{{"kind":"{kind}","id":"{id}","action":"remove"}}"#);
                let (reply, _) = result(server, "impact", &arguments);
                assert!(reply.contains("\"isError\":false"), "{reply}");
            }
        }
    }
    for arguments in [
        r#"{"kind":"manifest","id":"world.jsonc","direction":"from"}"#,
        r#"{"kind":"profile","id":"profile/profile.jsonc","direction":"from","roles":["profile_schema"]}"#,
    ] {
        let (reply, _) = result(server, "references", arguments);
        assert!(reply.contains("\"isError\":false"), "{reply}");
    }
    let replies = send(
        server,
        &[&call(
            1,
            "references",
            r#"{"kind":"file","id":"profile/profile.jsonc"}"#,
        )],
    );
    assert!(replies[0].contains("\"isError\":false"));
}

#[test]
#[allow(clippy::disallowed_methods, clippy::too_many_lines)]
fn an_object_carrying_many_is_listed_within_its_caps() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("tree");
    copy_tree(&fixture("basic"), &root);
    let carried = crate::references::MAXIMUM_CARRIED + 20;
    let items: Vec<String> = (0..carried)
        .map(|index| format!(r#"{{"uid": "bag.item{index}", "proto": "coin"}}"#))
        .collect();
    std::fs::write(
        root.join("planes/overworld/layers/0/1_0.jsonc"),
        format!(
            r#"{{"format": "tilewright.chunk/1", "plane": "overworld", "layer": 0, "chunk": [1, 0], "objects": [{{"uid": "bag", "at": [0, 0], "proto": "chest", "inventory": [{}]}}]}}"#,
            items.join(",")
        ),
    )
    .unwrap();
    let mut server = ready(&root);
    let arguments = r#"{"kind":"object","id":"bag","action":"remove"}"#;
    let total = carried.to_string();
    assert_eq!(
        impact_member(&mut server, arguments, "carried_total"),
        total
    );
    assert_eq!(
        impact_member(&mut server, arguments, "carried_truncated"),
        "true"
    );
    let (_, document) = result(&mut server, "impact", arguments);
    let page = structured(&document);
    assert_eq!(
        at(page, &["carried"]).as_array().unwrap().len(),
        crate::references::MAXIMUM_CARRIED
    );
    let allowance = at(page, &["diff_allowance"]);
    assert_eq!(
        at(allowance, &["entries"]).as_array().unwrap().len(),
        crate::references::MAXIMUM_CARRIED
    );
    assert_eq!(
        at(allowance, &["entries_total"]).as_i64(),
        Some(i64::try_from(carried + 1).unwrap())
    );
    assert_eq!(at(allowance, &["entries_truncated"]).as_bool(), Some(true));
    // The rest come with the cursors: each list paged to its end gives
    // every item once, in order.
    let (carried_items, carried_pages) = paged(
        &mut server,
        arguments,
        "carried_cursor",
        &["carried_next_cursor"],
        &["carried"],
    );
    assert_eq!(carried_pages, 2);
    assert_eq!(carried_items.len(), carried);
    let uids: Vec<String> = carried_items
        .iter()
        .map(|item| {
            let document = Document::parse(item.as_str(), &Limits::default()).unwrap();
            at(document.root(), &["uid"]).as_str().unwrap().to_owned()
        })
        .collect();
    let expected: Vec<String> = (0..carried)
        .map(|index| format!("bag.item{index}"))
        .collect();
    assert_eq!(uids, expected);
    let (entries, entries_pages) = paged(
        &mut server,
        arguments,
        "entries_cursor",
        &["diff_allowance", "entries_next_cursor"],
        &["diff_allowance", "entries"],
    );
    assert_eq!(entries_pages, 2);
    assert_eq!(entries.len(), carried + 1);
    let mut wanted = vec![r#"{"kind":"object","id":"bag"}"#.to_owned()];
    wanted.extend(
        expected
            .iter()
            .map(|uid| format!(r#"{{"kind":"object","id":"{uid}"}}"#)),
    );
    assert_eq!(entries, wanted);
    // tilewright diff accepts the removal with exactly those entries, and
    // not with the first page alone or with any one left out.
    let removed = directory.path().join("removed");
    copy_tree(&root, &removed);
    std::fs::write(
        removed.join("planes/overworld/layers/0/1_0.jsonc"),
        r#"{"format": "tilewright.chunk/1", "plane": "overworld", "layer": 0, "chunk": [1, 0], "objects": []}"#,
    )
    .unwrap();
    assert!(!super::impact::breaking_with(
        &root, &removed, "remove", &entries
    ));
    assert!(super::impact::breaking_with(
        &root,
        &removed,
        "remove",
        &entries[..crate::references::MAXIMUM_CARRIED]
    ));
    for left_out in [0, 1, crate::references::MAXIMUM_CARRIED, carried] {
        let mut fewer = entries.clone();
        fewer.remove(left_out);
        assert!(
            super::impact::breaking_with(&root, &removed, "remove", &fewer),
            "{left_out}"
        );
    }
}

#[test]
#[allow(clippy::disallowed_methods)]
fn rules_that_are_not_references_are_not_impact() {
    // FORMAT.md section 13: an outside transition without `with` draws onto
    // the terrains before it, of which there must be one. Removing the
    // first terrain breaks that rule, which is not a reference: impact
    // predicts nothing, says so in not_covered, and the tree no longer
    // loads.
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("tree");
    copy_tree(&fixture("terrain"), &root);
    let first = r#"{"id": "sandy", "protos": ["sand"]}"#;
    let transitions = |first: &str| {
        format!(
            r#"{{"format": "tilewright.transitions/1", "layer": "floor", "terrains": [{first}
  {{"id": "grassy", "protos": ["grass"], "transitions": [{{"draw": "outside", "model": "sides", "layer": "edge", "tiles": {{"1": "sand_n"}}}}]}}
]}}"#
        )
    };
    std::fs::write(
        root.join("profile/transitions.jsonc"),
        transitions(&format!("{first},")),
    )
    .unwrap();
    tilewright_source::load(&root).expect("the tree loads");
    let mut server = ready(&root);
    let arguments = r#"{"kind":"terrain","id":"sandy","action":"remove"}"#;
    assert_eq!(impact_member(&mut server, arguments, "breaking_total"), "0");
    let not_covered = impact_member(&mut server, arguments, "not_covered");
    assert!(not_covered.contains("Rules of the format that are not references are not checked"));
    assert!(not_covered.contains("preview_transaction"));
    std::fs::write(root.join("profile/transitions.jsonc"), transitions("")).unwrap();
    let diagnostics = tilewright_source::load(&root).unwrap_err();
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "ref.unresolved"),
        "{diagnostics:?}"
    );
    assert_eq!(diagnostics[0].code, "shape.missing_key", "{diagnostics:?}");
}

#[test]
fn a_load_past_its_own_budget_is_not_loaded_again() {
    // One budget diagnostic that is not the index's refusal at `.`, as a
    // load past its own budgets gives at the file it reached: the tree's
    // own failure, served as it is, without a second load.
    let own = Diagnostic {
        path: "planes/overworld/layers/0/0_0.jsonc".to_owned(),
        start: 0,
        end: 0,
        line: 0,
        column: 0,
        code: "source.budget",
        message: "the load read more than its budget".to_owned(),
    };
    let snapshot = Snapshot::load_with(&fixture("basic"), |_| Err(vec![own.clone()]));
    assert_eq!(snapshot.revision(), None);
    assert_eq!(snapshot.diagnostics(), [own]);
}

/// The input schema of `tool`, as `tools/list` gives it.
fn input_schema(tool: &str) -> Document {
    let schemas =
        Document::parse(format!("{{{}}}", crate::tools::list()), &Limits::default()).unwrap();
    let schema = at(schemas.root(), &["tools"])
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry.get("name").and_then(Value::as_str) == Some(tool))
        .map(|entry| schemas.canonical(at(entry, &["inputSchema"])))
        .unwrap();
    Document::parse(schema, &Limits::default()).unwrap()
}

#[test]
fn the_input_schemas_state_the_rules_the_server_checks() {
    let mut server = ready(&fixture("basic"));
    let accepted = |server: &mut Server, tool: &str, arguments: &str| {
        let (reply, _) = result(server, tool, arguments);
        reply.contains("\"isError\":false")
    };
    // scan_limit only with roles, and no default a client would send.
    let schema = input_schema("references");
    assert_eq!(
        schema.canonical(at(schema.root(), &["dependentRequired"])),
        r#"{"scan_limit":["roles"]}"#
    );
    assert!(
        at(schema.root(), &["properties", "scan_limit"])
            .get("default")
            .is_none()
    );
    let base = r#""kind":"plane","id":"overworld""#;
    assert!(!accepted(
        &mut server,
        "references",
        &format!(r#"{{{base},"scan_limit":5}}"#)
    ));
    assert!(accepted(
        &mut server,
        "references",
        &format!(r#"{{{base},"scan_limit":5,"roles":["proto"]}}"#)
    ));
    // to exactly when action is rename.
    let schema = input_schema("impact");
    assert_eq!(
        schema.canonical(at(schema.root(), &["if"])),
        r#"{"properties":{"action":{"const":"rename"}},"required":["action"]}"#
    );
    assert_eq!(
        schema.canonical(at(schema.root(), &["then"])),
        r#"{"required":["to"]}"#
    );
    assert_eq!(
        schema.canonical(at(schema.root(), &["else"])),
        r#"{"not":{"required":["to"]}}"#
    );
    let base = r#""kind":"plane","id":"vault""#;
    for (rest, ok) in [
        (r#""action":"rename","to":"crypt""#, true),
        (r#""action":"rename""#, false),
        (r#""action":"remove""#, true),
        (r#""action":"remove","to":"crypt""#, false),
    ] {
        assert_eq!(
            accepted(&mut server, "impact", &format!("{{{base},{rest}}}")),
            ok,
            "{rest}"
        );
    }
    // Every default a schema of these tools states is accepted as sent.
    for (tool, base) in [
        ("references", r#""kind":"plane","id":"overworld""#),
        ("impact", r#""kind":"plane","id":"vault","action":"remove""#),
    ] {
        let schema = input_schema(tool);
        for member in at(schema.root(), &["properties"]).as_object().unwrap() {
            if let Some(default) = member.value.get("default") {
                let arguments = format!(
                    r#"{{{base},"{}":{}}}"#,
                    member.key,
                    schema.canonical(default)
                );
                assert!(
                    accepted(&mut server, tool, &arguments),
                    "{tool} {arguments}"
                );
            }
        }
    }
}

/// A file past the format's 64 MiB, read after another file, is refused by
/// the library in its read budget's words, but its limit is the format's
/// per-file limit, which `tilewright tx apply` refuses too: so
/// `server_budget_reached` is false for it (docs/MCP.md, "Budgets").
///
/// The file is sparse: the library refuses it from its length and never
/// reads it, and the loader does not read it, so it costs no disk and no
/// time. On Unix only, where extending a file with `set_len` leaves it
/// sparse (Linux file systems and APFS); NTFS would allocate it.
#[cfg(unix)]
#[test]
#[allow(clippy::disallowed_methods)]
fn a_file_past_the_format_limit_read_second_is_not_the_read_budget() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("tree");
    copy_tree(&fixture("basic"), &root);
    std::fs::write(root.join("profile/small.jsonc"), "{\"a\": 1}\n").unwrap();
    std::fs::File::create(root.join("profile/huge.jsonc"))
        .unwrap()
        .set_len(70 << 20)
        .unwrap();
    let mut server = ready(&root);
    let test =
        |path: &str| format!(r#"{{"op": "test", "path": "{path}", "pointer": ["a"], "value": 1}}"#);
    let reached = |server: &mut Server, operations: &str| {
        let arguments = format!(
            r#"{{"transaction":{{"format": "tilewright.transaction/1", "operations": [{operations}]}}}}"#
        );
        let (_, document) = result(server, "preview_transaction", &arguments);
        let page = structured(&document);
        assert_eq!(at(page, &["would_apply"]).as_bool(), Some(false));
        assert_eq!(
            at(page, &["refusals", "0", "limit", "kind"]).as_str(),
            Some("file_size")
        );
        assert_eq!(
            at(page, &["refusals", "0", "limit", "server_budget"]).as_bool(),
            Some(false)
        );
        at(page, &["server_budget_reached"]).as_bool().unwrap()
    };
    // Alone, the library says the file is too large: not the server's.
    assert!(!reached(&mut server, &test("profile/huge.jsonc")));
    // After another file, it says the touched files hold more than the
    // read budget, and is still not the server's.
    let both = format!(
        "{},{}",
        test("profile/small.jsonc"),
        test("profile/huge.jsonc")
    );
    assert!(!reached(&mut server, &both));
}

/// Every item of the list at `items` of `impact`'s result for `arguments`,
/// each as its JSON text, following the cursor at `next` with the argument
/// `cursor` until it is null; and the number of pages. Each page starts
/// at the offset its cursor named, and the other lists stay at their
/// first page.
fn paged(
    server: &mut Server,
    arguments: &str,
    cursor: &str,
    next: &[&str],
    items: &[&str],
) -> (Vec<String>, usize) {
    let mut found = Vec::new();
    let mut pages = 0;
    let mut call = arguments.to_owned();
    loop {
        let (reply, document) = result(server, "impact", &call);
        assert!(reply.contains("\"isError\":false"), "{call}: {reply}");
        let page = structured(&document);
        // The breaking references are always their first page here.
        assert_eq!(at(page, &["offset"]).as_i64(), Some(0));
        let offset_key = if cursor == "carried_cursor" {
            vec!["carried_offset"]
        } else {
            vec!["diff_allowance", "entries_offset"]
        };
        assert_eq!(
            at(page, &offset_key).as_i64(),
            Some(i64::try_from(found.len()).unwrap())
        );
        let list = at(page, items).as_array().unwrap();
        assert!(!list.is_empty());
        found.extend(list.iter().map(|item| document.canonical(item)));
        pages += 1;
        match at(page, next).as_str() {
            Some(token) => {
                call = format!(
                    r#"{},"{cursor}":"{token}"}}"#,
                    &arguments[..arguments.len() - 1]
                );
            }
            None => return (found, pages),
        }
    }
}

#[test]
#[allow(clippy::disallowed_methods)]
fn impacts_list_cursors_are_their_own() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("tree");
    copy_tree(&fixture("basic"), &root);
    let items: Vec<String> = (0..crate::references::MAXIMUM_CARRIED + 5)
        .map(|index| format!(r#"{{"uid": "bag.item{index}", "proto": "coin"}}"#))
        .collect();
    std::fs::write(
        root.join("planes/overworld/layers/0/1_0.jsonc"),
        format!(
            r#"{{"format": "tilewright.chunk/1", "plane": "overworld", "layer": 0, "chunk": [1, 0], "objects": [{{"uid": "bag", "at": [0, 0], "proto": "chest", "inventory": [{}]}}]}}"#,
            items.join(",")
        ),
    )
    .unwrap();
    let mut server = ready(&root);
    let arguments = r#"{"kind":"object","id":"bag","action":"remove"}"#;
    let (_, document) = result(&mut server, "impact", arguments);
    let page = structured(&document);
    let carried = at(page, &["carried_next_cursor"])
        .as_str()
        .unwrap()
        .to_owned();
    let entries = at(page, &["diff_allowance", "entries_next_cursor"])
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(carried, entries);
    let with = |name: &str, token: &str, arguments: &str| {
        format!(
            r#"{},"{name}":"{token}"}}"#,
            &arguments[..arguments.len() - 1]
        )
    };
    let refused = |server: &mut Server, call: &str| {
        let (_, document) = result(server, "impact", call);
        assert_eq!(tool_error(&document), "cursor.invalid", "{call}");
    };
    // A cursor of one list is not a cursor of another.
    refused(&mut server, &with("entries_cursor", &carried, arguments));
    refused(&mut server, &with("carried_cursor", &entries, arguments));
    refused(&mut server, &with("cursor", &carried, arguments));
    // Nor of other arguments: the object renamed, or another object.
    let rename = r#"{"kind":"object","id":"bag","action":"rename","to":"sack"}"#;
    refused(&mut server, &with("entries_cursor", &entries, rename));
    refused(&mut server, &with("carried_cursor", &carried, rename));
    // Both cursors in one call page both lists.
    let both = with(
        "entries_cursor",
        &entries,
        &with("carried_cursor", &carried, arguments),
    );
    let (_, document) = result(&mut server, "impact", &both);
    let page = structured(&document);
    assert_eq!(at(page, &["carried"]).as_array().unwrap().len(), 5);
    assert_eq!(
        at(page, &["diff_allowance", "entries"])
            .as_array()
            .unwrap()
            .len(),
        6
    );
    assert_eq!(at(page, &["carried_truncated"]).as_bool(), Some(false));
    assert!(at(page, &["carried_next_cursor"]).as_str().is_none());
    // A string that is not a cursor.
    refused(&mut server, &with("carried_cursor", "nope", arguments));
}
