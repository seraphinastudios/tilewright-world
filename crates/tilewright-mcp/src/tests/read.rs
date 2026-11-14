// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! `inspect`, `search_definitions`, `search_objects` and the `revision`
//! argument: goldens on both fixtures, spans checked against the files on
//! LF and CR LF copies, the drift check, the budgets, and paging to
//! exhaustion with tiny limits and scan budgets.

use super::{
    at, call, copy_tree, fixture, golden, invalid_tree, only_replies, parse, ready, result, send,
    structured, tool_error,
};
use crate::Server;
use crate::budget::MAXIMUM_RESULT_BYTES;
use std::{collections::BTreeSet, fmt::Write as _, path::Path};
use tilewright_jsonc::{Document, Limits, Value};
use tilewright_source::{Chunk, FieldValue, Object};

/// The kinds `inspect` takes.
const KINDS: [&str; 6] = [
    "anchor",
    "definition",
    "object",
    "plane",
    "stairwell",
    "zone",
];

/// Every ID of `kind`, from `search_ids`.
fn ids(server: &mut Server, kind: &str) -> Vec<String> {
    let (_, document) = result(
        server,
        "search_ids",
        &format!(r#"{{"kind":"{kind}","limit":500}}"#),
    );
    let page = structured(&document);
    assert_eq!(at(page, &["next_cursor"]).kind(), "null");
    at(page, &["ids"])
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap().to_owned())
        .collect()
}

fn inspect_arguments(kind: &str, id: &str) -> String {
    let mut arguments = String::from(r#"{"kind":"#);
    crate::json_string(kind, &mut arguments);
    arguments.push_str(r#","id":"#);
    crate::json_string(id, &mut arguments);
    arguments.push('}');
    arguments
}

/// A file name for a golden of `kind` and `id`.
fn golden_name(fixture: &str, kind: &str, id: &str) -> String {
    let id: String = id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '.' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect();
    format!("{fixture}-inspect-{kind}-{id}")
}

#[test]
fn golden_inspect_and_search_results_on_both_fixtures() {
    let basic: &[(&str, &str)] = &[
        ("definition", "proto/chest"),
        ("definition", "loot/chest_common"),
        ("plane", "overworld"),
        ("zone", "overworld/island"),
        ("anchor", "overworld/cave.entrance"),
        ("stairwell", "overworld/cave.chute"),
        ("object", "village.chest"),
        ("object", "o.k3vq7zr2mxa5d6wfh4pnyt2cgu"),
    ];
    let mut server = ready(&fixture("basic"));
    for (kind, id) in basic {
        let (reply, _) = result(&mut server, "inspect", &inspect_arguments(kind, id));
        golden(&golden_name("basic", kind, id), &reply);
    }
    for (name, arguments) in [
        ("search_definitions", r#"{"limit":500}"#),
        (
            "search_definitions-layer",
            r#"{"type":"proto","field":"layer","equals":"item"}"#,
        ),
        ("search_objects", r#"{"limit":500}"#),
        ("search_objects-uncarried", r#"{"carried":false}"#),
    ] {
        let tool = name.split('-').next().unwrap();
        let (reply, _) = result(&mut server, tool, arguments);
        golden(&format!("basic-{name}"), &reply);
    }
    let mut server = ready(&fixture("terrain"));
    let first = ids(&mut server, "definition").remove(0);
    for (kind, id) in [("plane", "island"), ("definition", first.as_str())] {
        let (reply, _) = result(&mut server, "inspect", &inspect_arguments(kind, id));
        golden(&golden_name("terrain", kind, id), &reply);
    }
    for tool in ["search_definitions", "search_objects"] {
        let (reply, _) = result(&mut server, tool, r#"{"limit":5}"#);
        golden(&format!("terrain-{tool}"), &reply);
    }
}

/// The 1-based line and column, in characters, of `offset` in `text`,
/// counted another way than the server counts them.
fn line_and_column(text: &str, offset: usize) -> (i64, i64) {
    let (mut line, mut column) = (1, 1);
    for (index, character) in text.char_indices() {
        if index == offset {
            break;
        }
        if character == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}

/// For every item `inspect` takes in the tree at `root`: its result,
/// checked against the file its path names: the span's text parses to the
/// value given, and its lines and columns are where the span is. Returns
/// `(kind, id, start, start_line, start_column, end_line)` for each.
fn inspect_everything(root: &Path) -> Vec<(String, String, i64, i64, i64, i64)> {
    let mut server = ready(root);
    let mut seen = Vec::new();
    for kind in KINDS {
        for id in ids(&mut server, kind) {
            let (_, document) = result(&mut server, "inspect", &inspect_arguments(kind, &id));
            let item = structured(&document);
            assert_eq!(at(item, &["value_truncated"]).as_bool(), Some(false));
            assert_eq!(at(item, &["truncated_strings"]).as_i64(), Some(0));
            let path = at(item, &["path"]).as_str().unwrap();
            #[allow(clippy::disallowed_methods)]
            let text = std::fs::read_to_string(root.join(path)).unwrap();
            let span = at(item, &["span"]);
            let number = |key: &str| at(span, &[key]).as_i64().unwrap();
            let start = usize::try_from(number("start")).unwrap();
            let end = usize::try_from(number("end")).unwrap();
            let source = Document::parse(&text[start..end], &Limits::default()).unwrap();
            let value = at(item, &["value"]);
            assert_eq!(
                source.canonical(source.root()),
                document.canonical(value),
                "{kind} {id}"
            );
            assert_eq!(
                usize::try_from(at(item, &["canonical_bytes"]).as_i64().unwrap()).unwrap(),
                source.canonical(source.root()).len()
            );
            assert_eq!(
                line_and_column(&text, start),
                (number("start_line"), number("start_column")),
                "{kind} {id}"
            );
            assert_eq!(
                line_and_column(&text, end),
                (number("end_line"), number("end_column")),
                "{kind} {id}"
            );
            seen.push((
                kind.to_owned(),
                id,
                number("start"),
                number("start_line"),
                number("start_column"),
                number("end_line"),
            ));
        }
    }
    seen
}

/// A copy of `from` in `to` whose text files end their lines in CR LF, as
/// a Windows checkout that converts line endings has them.
#[allow(clippy::disallowed_methods)]
fn crlf_copy(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            crlf_copy(&entry.path(), &target);
        } else {
            let text = std::fs::read_to_string(entry.path()).unwrap();
            let mut converted = String::new();
            for line in text.lines() {
                converted.push_str(line);
                converted.push_str("\r\n");
            }
            std::fs::write(target, converted).unwrap();
        }
    }
}

#[test]
fn spans_are_the_items_in_their_files_with_either_line_ending() {
    for name in ["basic", "terrain"] {
        let lf = inspect_everything(&fixture(name));
        let kinds: BTreeSet<&str> = lf.iter().map(|(kind, ..)| kind.as_str()).collect();
        if name == "basic" {
            assert_eq!(kinds.len(), KINDS.len(), "every kind is inspected");
            // The nested inventory entry is the coin's own object.
            assert!(
                lf.iter()
                    .any(|(kind, id, ..)| kind == "object" && id == "o.k3vq7zr2mxa5d6wfh4pnyt2cgu")
            );
        }
        let directory = tempfile::tempdir().unwrap();
        crlf_copy(&fixture(name), directory.path());
        let crlf = inspect_everything(directory.path());
        assert_eq!(crlf.len(), lf.len());
        for (lf, crlf) in lf.iter().zip(&crlf) {
            // The same lines and columns; each line before adds a CR.
            assert_eq!((&lf.0, &lf.1), (&crlf.0, &crlf.1));
            assert_eq!((lf.3, lf.4, lf.5), (crlf.3, crlf.4, crlf.5), "{lf:?}");
            assert_eq!(crlf.2, lf.2 + lf.3 - 1, "{lf:?}");
        }
    }
}

/// The tool error code of an `inspect`.
fn inspect_error(server: &mut Server, kind: &str, id: &str) -> String {
    let (_, document) = result(server, "inspect", &inspect_arguments(kind, id));
    tool_error(&document)
}

/// The span start of an `inspect` that succeeds.
fn inspect_start(server: &mut Server, kind: &str, id: &str) -> i64 {
    let (reply, document) = result(server, "inspect", &inspect_arguments(kind, id));
    assert!(reply.contains("\"isError\":false"), "{kind} {id}: {reply}");
    at(structured(&document), &["span", "start"])
        .as_i64()
        .unwrap()
}

#[test]
#[allow(clippy::disallowed_methods)]
#[allow(clippy::too_many_lines)]
fn inspect_refuses_an_item_whose_file_changed_since_the_snapshot() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("tree");
    copy_tree(&fixture("basic"), &root);
    let mut server = ready(&root);
    let revision = server.revision().unwrap().to_owned();
    let rewrite = |path: &str, from: &str, to: &str| {
        let path = root.join(path);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(from), "{from}");
        std::fs::write(&path, text.replacen(from, to, 1)).unwrap();
    };

    // A value changed: the item is not the snapshot's.
    rewrite("defs/proto/chest.jsonc", "12.5", "13");
    assert_eq!(
        inspect_error(&mut server, "definition", "proto/chest"),
        "revision.changed"
    );
    // Only spacing and comments changed: the item is the snapshot's, at
    // its new place.
    let coin = inspect_start(&mut server, "definition", "proto/coin");
    let path = root.join("defs/proto/coin.jsonc");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("// A note.\n\n{text}")).unwrap();
    assert_eq!(
        inspect_start(&mut server, "definition", "proto/coin"),
        coin + 12
    );
    // A file removed.
    std::fs::remove_file(root.join("defs/recipe/chest.jsonc")).unwrap();
    assert_eq!(
        inspect_error(&mut server, "definition", "recipe/chest"),
        "revision.changed"
    );
    // An entry of a list file changed, beside one that did not.
    rewrite(
        "planes/overworld/zones.jsonc",
        "Green Island",
        "Grey Island",
    );
    assert_eq!(
        inspect_error(&mut server, "zone", "overworld/island"),
        "revision.changed"
    );
    inspect_start(&mut server, "zone", "overworld/village");
    rewrite("planes/overworld/anchors.jsonc", "[4, 4]", "[4, 5]");
    assert_eq!(
        inspect_error(&mut server, "anchor", "overworld/cave.entrance"),
        "revision.changed"
    );
    inspect_start(&mut server, "anchor", "overworld/village.well");
    rewrite(
        "planes/overworld/stairwells.jsonc",
        "\"direction\": \"down\"",
        "\"direction\": \"both\"",
    );
    assert_eq!(
        inspect_error(&mut server, "stairwell", "overworld/cave.chute"),
        "revision.changed"
    );
    inspect_start(&mut server, "stairwell", "overworld/tower.stairs");
    rewrite("planes/vault/plane.jsonc", "\"vault\"", "\"vault\" ");
    inspect_start(&mut server, "plane", "vault");
    // A nested inventory entry changed: it, and each object that carries
    // it, are not the snapshot's; another object of the chunk is.
    let chunk = "planes/overworld/layers/0/0_0.jsonc";
    rewrite(
        chunk,
        "\"proto\": \"coin\", \"fields\": {\"count\": 1}",
        "\"proto\": \"coin\", \"fields\": {\"count\": 2}",
    );
    for uid in [
        "o.k3vq7zr2mxa5d6wfh4pnyt2cgu",
        "village.chest.small",
        "village.chest",
    ] {
        assert_eq!(
            inspect_error(&mut server, "object", uid),
            "revision.changed"
        );
    }
    inspect_start(&mut server, "object", "ruin.north_wall");
    // An explicit default is the same object.
    rewrite(
        chunk,
        "\"uid\": \"ruin.north_wall\", \"at\": [15, 0]",
        "\"uid\": \"ruin.north_wall\", \"z\": 0, \"at\": [15, 0]",
    );
    inspect_start(&mut server, "object", "ruin.north_wall");
    rewrite(chunk, "\"layer\": \"wall\"", "\"layer\": \"item\"");
    assert_eq!(
        inspect_error(&mut server, "object", "ruin.north_wall"),
        "revision.changed"
    );
    // A file replaced by a link is not followed.
    #[cfg(unix)]
    {
        let wall = root.join("defs/proto/wall.jsonc");
        let elsewhere = directory.path().join("wall.jsonc");
        std::fs::rename(&wall, &elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, &wall).unwrap();
        assert_eq!(
            inspect_error(&mut server, "definition", "proto/wall"),
            "revision.changed"
        );
        std::fs::remove_file(&wall).unwrap();
        std::fs::rename(&elsewhere, &wall).unwrap();
    }
    inspect_start(&mut server, "definition", "proto/wall");
    // The root replaced by another tree: the root is opened afresh.
    std::fs::rename(&root, directory.path().join("old")).unwrap();
    copy_tree(&fixture("terrain"), &root);
    assert_eq!(
        inspect_error(&mut server, "definition", "proto/wall"),
        "revision.changed"
    );
    assert_eq!(server.revision(), Some(revision.as_str()));
    // After a refresh, the new tree's items are inspected.
    let (_, document) = result(&mut server, "refresh", "{}");
    assert_eq!(
        at(structured(&document), &["changed"]).as_bool(),
        Some(true)
    );
    inspect_start(&mut server, "plane", "island");
    assert_eq!(
        inspect_error(&mut server, "definition", "proto/wall"),
        "item.not_found"
    );
}

#[test]
fn inspect_checks_its_arguments() {
    let mut server = ready(&fixture("basic"));
    for (kind, id) in [
        ("definition", "proto"),
        ("definition", "/chest"),
        ("definition", "proto/"),
        ("zone", "island"),
        ("anchor", ""),
    ] {
        assert_eq!(inspect_error(&mut server, kind, id), "arguments.invalid");
    }
    for (kind, id) in [
        ("definition", "proto/nothing"),
        ("definition", "proto/chest/x"),
        ("definition", "nothing/chest"),
        ("plane", "nothing"),
        ("plane", ""),
        ("zone", "vault/island"),
        ("zone", "nothing/island"),
        ("anchor", "overworld/nothing"),
        ("stairwell", "vault/tower.stairs"),
        ("object", "nothing"),
        ("object", "../../world.jsonc"),
    ] {
        assert_eq!(inspect_error(&mut server, kind, id), "item.not_found");
    }
    for arguments in [
        r#"{"kind":"render_layer","id":"floor"}"#,
        r#"{"kind":"plane"}"#,
        r#"{"id":"overworld"}"#,
        r#"{"kind":"plane","id":7}"#,
        r#"{"kind":"plane","id":"overworld","path":"x"}"#,
    ] {
        let (_, document) = result(&mut server, "inspect", arguments);
        assert_eq!(tool_error(&document), "arguments.invalid", "{arguments}");
    }
    let long = "a".repeat(513);
    assert_eq!(
        inspect_error(&mut server, "plane", &long),
        "arguments.invalid"
    );
    assert_eq!(
        inspect_error(&mut server, "plane", &long[1..]),
        "item.not_found"
    );
}

#[test]
#[allow(clippy::disallowed_methods)]
fn inspect_stays_within_its_budget() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    copy_tree(&fixture("basic"), root);
    // A prototype whose JSON footprint is far past the budget, with a
    // string past the string cap, control characters, and a key past the
    // string cap near the end.
    let mut footprint = String::from("[");
    for index in 0..40_000 {
        let _ = write!(footprint, "\"\\u0007\\\"{index:08}\",");
    }
    let _ = write!(footprint, "\"{}\",", "y".repeat(5000));
    let _ = write!(footprint, "{{\"{}\": 1}}]", "k".repeat(5000));
    std::fs::write(
        root.join("defs/proto/huge.jsonc"),
        format!(
            r#"{{"format": "tilewright.def/1", "type": "proto", "id": "huge", "fields": {{"face": "huge.png", "layer": "item", "footprint": {footprint}}}}}"#
        ),
    )
    .unwrap();
    // One that fits, but for a string past the cap.
    std::fs::write(
        root.join("defs/proto/long.jsonc"),
        format!(
            r#"{{"format": "tilewright.def/1", "type": "proto", "id": "long", "fields": {{"face": "long.png", "layer": "item", "footprint": "{}é"}}}}"#,
            "z".repeat(4095)
        ),
    )
    .unwrap();
    let mut server = ready(root);
    assert!(server.revision().is_some(), "the tree loads");
    let replies = send(
        &mut server,
        &[&call(
            7,
            "inspect",
            &inspect_arguments("definition", "proto/huge"),
        )],
    );
    let reply = &replies[0];
    only_replies(format!("{reply}\n").as_bytes());
    assert!(reply.len() <= MAXIMUM_RESULT_BYTES, "{}", reply.len());
    assert!(
        reply.len() > MAXIMUM_RESULT_BYTES * 3 / 4,
        "{}",
        reply.len()
    );
    let document = parse(reply);
    let item = structured(&document);
    assert_eq!(at(item, &["value_truncated"]).as_bool(), Some(true));
    assert!(
        at(item, &["canonical_bytes"]).as_i64().unwrap()
            > i64::try_from(MAXIMUM_RESULT_BYTES).unwrap()
    );
    let kept = at(item, &["value", "fields", "footprint"])
        .as_array()
        .unwrap();
    assert!(kept.len() > 1000 && kept.len() < 40_000, "{}", kept.len());
    assert_eq!(kept[0].as_str(), Some("\u{7}\"00000000"));
    // The value stays the document it was cut from: what it kept is the
    // start of the file's value.
    assert_eq!(at(item, &["value", "id"]).as_str(), Some("huge"));

    let (_, document) = result(
        &mut server,
        "inspect",
        &inspect_arguments("definition", "proto/long"),
    );
    let item = structured(&document);
    assert_eq!(at(item, &["value_truncated"]).as_bool(), Some(false));
    assert_eq!(at(item, &["truncated_strings"]).as_i64(), Some(1));
    assert_eq!(
        at(item, &["value", "fields", "footprint"]).as_str(),
        Some("z".repeat(4095).as_str()),
        "cut at a character boundary"
    );
}

/// The items of every page of a search, page by page, with `limit` and
/// `scan_limit`: each item's canonical JSON, the pages, and whether a page
/// stopped at its scan budget.
fn exhaust_search(
    server: &mut Server,
    tool: &str,
    arguments: &str,
    field: &str,
    limit: usize,
    scan_limit: usize,
) -> (Vec<String>, usize, bool) {
    let inner = &arguments[1..arguments.len() - 1];
    let separator = if inner.is_empty() { "" } else { "," };
    let mut items = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    let mut scan_stopped = false;
    let mut offset = 0;
    loop {
        let mut arguments =
            format!("{{{inner}{separator}\"limit\":{limit},\"scan_limit\":{scan_limit}");
        if let Some(cursor) = &cursor {
            let _ = write!(arguments, ",\"cursor\":\"{cursor}\"");
        }
        arguments.push('}');
        let (_, document) = result(server, tool, &arguments);
        let page = structured(&document);
        assert_eq!(at(page, &["offset"]).as_i64(), Some(offset));
        let scanned = at(page, &["scanned"]).as_i64().unwrap();
        assert!(scanned <= i64::try_from(scan_limit).unwrap());
        let values = at(page, &[field]).as_array().unwrap();
        assert!(values.len() <= limit);
        items.extend(values.iter().map(|value| document.canonical(value)));
        let stopped = at(page, &["scan_budget_reached"]).as_bool().unwrap();
        if stopped {
            assert_eq!(scanned, i64::try_from(scan_limit).unwrap());
        }
        scan_stopped |= stopped;
        pages += 1;
        match at(page, &["next_cursor"]) {
            Value::String { value, .. } => {
                cursor = Some(value.clone());
                let next = value.rsplit('.').next().unwrap().parse::<i64>().unwrap();
                assert!(next > offset, "every page moves on");
                offset = next;
            }
            _ => break,
        }
        assert!(pages < 10_000);
    }
    (items, pages, scan_stopped)
}

/// Pages through a search with tiny limits and scan budgets, and checks
/// that every match comes exactly once, in the order of one big page.
fn pages_reach_every_match(
    server: &mut Server,
    tool: &str,
    arguments: &str,
    field: &str,
) -> Vec<String> {
    let (all, pages, _) = exhaust_search(server, tool, arguments, field, 500, 10_000);
    assert_eq!(pages, 1, "{arguments}");
    for (limit, scan_limit) in [(1, 1), (1, 2), (2, 1), (3, 2), (2, 5), (500, 3)] {
        let (collected, _, stopped) =
            exhaust_search(server, tool, arguments, field, limit, scan_limit);
        assert_eq!(collected, all, "{tool} {arguments} {limit} {scan_limit}");
        // More matches than the scan budget, so more candidates too.
        if limit == 500 && all.len() > scan_limit {
            assert!(stopped, "{arguments}");
        }
    }
    all
}

/// Each `(type, id)` an item list names, in order.
fn definition_keys(items: &[String]) -> Vec<String> {
    items
        .iter()
        .map(|item| {
            let document = Document::parse(item.as_str(), &Limits::default()).unwrap();
            format!(
                "{}/{}",
                at(document.root(), &["type"]).as_str().unwrap(),
                at(document.root(), &["id"]).as_str().unwrap()
            )
        })
        .collect()
}

#[test]
#[allow(clippy::too_many_lines)]
fn search_definitions_matches_ids_fields_and_nested_values() {
    let mut server = ready(&fixture("basic"));
    let world = tilewright_source::load(&fixture("basic")).unwrap();
    let all: Vec<String> = world
        .definitions()
        .keys()
        .map(|(kind, id)| format!("{kind}/{id}"))
        .collect();
    let search = |server: &mut Server, arguments: &str| {
        definition_keys(&pages_reach_every_match(
            server,
            "search_definitions",
            arguments,
            "definitions",
        ))
    };
    assert_eq!(search(&mut server, "{}"), all);
    let protos = search(&mut server, r#"{"type":"proto"}"#);
    assert_eq!(protos.len(), 10);
    assert!(search(&mut server, r#"{"type":"prot"}"#).is_empty());
    // A field set, and a field's value.
    let with = |field: &str, test: &dyn Fn(&FieldValue) -> bool| -> Vec<String> {
        world
            .definitions()
            .iter()
            .filter(|(_, definition)| {
                definition
                    .fields
                    .iter()
                    .any(|(name, value)| name == field && test(value))
            })
            .map(|((kind, id), _)| format!("{kind}/{id}"))
            .collect()
    };
    let footprint = with("footprint", &|_| true);
    assert!(!footprint.is_empty());
    assert_eq!(search(&mut server, r#"{"field":"footprint"}"#), footprint);
    let items = with("layer", &|value| {
        *value == FieldValue::String("item".into())
    });
    assert!(items.contains(&"proto/chest".to_owned()) && items.len() < protos.len());
    assert_eq!(
        search(&mut server, r#"{"field":"layer","equals":"item"}"#),
        items
    );
    assert_eq!(
        search(&mut server, r#"{"field":"layer","contains":"it"}"#),
        items
    );
    // Numbers compare by value, an integer with a number.
    for equals in ["12.5", "1.25e1"] {
        assert_eq!(
            search(
                &mut server,
                &format!(r#"{{"field":"weight","equals":{equals}}}"#)
            ),
            ["proto/chest"]
        );
    }
    // Scalars inside list and object values, at any depth.
    for (arguments, expected) in [
        (
            r#"{"field":"entries","equals":"coin"}"#,
            vec!["loot/chest_common"],
        ),
        (
            r#"{"field":"entries","equals":5}"#,
            vec!["loot/chest_common"],
        ),
        (
            r#"{"field":"entries","equals":5.0}"#,
            vec!["loot/chest_common"],
        ),
        (
            r#"{"field":"entries","contains":"hes"}"#,
            vec!["loot/chest_common"],
        ),
        (r#"{"field":"entries","equals":"entries"}"#, vec![]),
        (r#"{"field":"entries","equals":"weight"}"#, vec![]),
        (
            r#"{"field":"ingredients","equals":5}"#,
            vec!["recipe/chest"],
        ),
        (r#"{"field":"ingredients","equals":"5"}"#, vec![]),
        (r#"{"field":"footprint","equals":1}"#, vec!["proto/wall"]),
        (
            r#"{"field":"blocks_view","equals":true}"#,
            vec!["proto/tree", "proto/wall"],
        ),
        (r#"{"field":"blocks_view","equals":"true"}"#, vec![]),
        (r#"{"field":"blocks_view","equals":null}"#, vec![]),
    ] {
        assert_eq!(search(&mut server, arguments), expected, "{arguments}");
    }
    // Without field, equals and contains match the ID.
    assert_eq!(
        search(&mut server, r#"{"equals":"chest"}"#),
        ["proto/chest", "recipe/chest"]
    );
    assert_eq!(
        search(&mut server, r#"{"contains":"chest"}"#),
        ["loot/chest_common", "proto/chest", "recipe/chest"]
    );
    assert_eq!(
        search(&mut server, r#"{"type":"proto","contains":"stairs"}"#),
        ["proto/stairs_down", "proto/stairs_up"]
    );
    // An item names its file, and the value it matched.
    let (_, document) = result(
        &mut server,
        "search_definitions",
        r#"{"field":"entries","equals":"coin"}"#,
    );
    let item = at(structured(&document), &["definitions", "0"]);
    assert_eq!(
        at(item, &["path"]).as_str(),
        Some("defs/loot/chest_common.jsonc")
    );
    assert_eq!(at(item, &["field"]).as_str(), Some("entries"));
    assert_eq!(at(item, &["value_kind"]).as_str(), Some("json"));
    let value = at(item, &["value"]).as_str().unwrap();
    assert!(value.starts_with("[{\"item\":\"coin\""), "{value}");
    let (_, document) = result(
        &mut server,
        "search_definitions",
        r#"{"field":"weight","equals":12.5}"#,
    );
    let item = at(structured(&document), &["definitions", "0"]);
    assert_eq!(at(item, &["value_kind"]).as_str(), Some("number"));
    assert_eq!(at(item, &["value"]).as_f64(), Some(12.5));
}

/// One object of a chunk as the test finds it, written as the search
/// writes it, less the field.
fn object_key(
    plane: &str,
    layer: i32,
    tile: [i64; 2],
    position: &[usize],
    object: &Object,
) -> String {
    format!(
        "{plane} {layer} {tile:?} {position:?} {} {}",
        object.uid.as_deref().unwrap_or("-"),
        object.proto
    )
}

/// Every object of `chunk`, depth first, with where it is.
fn walk(
    plane: &str,
    layer: i32,
    origin: [i64; 2],
    chunk: &Chunk,
) -> Vec<(String, bool, [i64; 2], Object)> {
    fn visit(
        objects: &[Object],
        prefix: &[usize],
        tile: Option<[i64; 2]>,
        origin: [i64; 2],
        output: &mut Vec<(Vec<usize>, [i64; 2], Object)>,
    ) {
        for (index, object) in objects.iter().enumerate() {
            let mut position = prefix.to_vec();
            position.push(index);
            let tile = tile.unwrap_or_else(|| {
                let [x, y] = object.at.unwrap();
                [origin[0] + i64::from(x), origin[1] + i64::from(y)]
            });
            output.push((position.clone(), tile, object.clone()));
            visit(&object.inventory, &position, Some(tile), origin, output);
        }
    }
    let mut found = Vec::new();
    visit(&chunk.objects, &[], None, origin, &mut found);
    found
        .into_iter()
        .map(|(position, tile, object)| {
            (
                object_key(plane, layer, tile, &position, &object),
                position.len() > 1,
                tile,
                object,
            )
        })
        .collect()
}

/// The same keys from search items.
fn object_keys(items: &[String]) -> Vec<String> {
    items
        .iter()
        .map(|item| {
            let document = Document::parse(item.as_str(), &Limits::default()).unwrap();
            let root = document.root();
            let number = |path: &[&str]| at(root, path).as_i64().unwrap();
            let position: Vec<usize> = at(root, &["position"])
                .as_array()
                .unwrap()
                .iter()
                .map(|step| usize::try_from(step.as_i64().unwrap()).unwrap())
                .collect();
            format!(
                "{} {} {:?} {position:?} {} {}",
                at(root, &["plane"]).as_str().unwrap(),
                number(&["layer"]),
                [number(&["tile", "0"]), number(&["tile", "1"])],
                at(root, &["uid"]).as_str().unwrap_or("-"),
                at(root, &["proto"]).as_str().unwrap(),
            )
        })
        .collect()
}

/// An object as the test finds it: its plane, layer, key, whether it is
/// carried, its tile, and itself.
type Found = (String, i32, String, bool, [i64; 2], Object);

#[test]
#[allow(clippy::too_many_lines)]
fn search_objects_filters_and_pages() {
    let mut server = ready(&fixture("basic"));
    let world = tilewright_source::load(&fixture("basic")).unwrap();
    let mut every = Vec::new();
    for (id, plane) in world.planes() {
        for (&(layer, cx, cy), chunk) in &plane.chunks {
            let size = i64::from(plane.chunk_size);
            every.extend(
                walk(id, layer, [cx * size, cy * size], chunk)
                    .into_iter()
                    .map(|(key, carried, tile, object)| {
                        (id.clone(), layer, key, carried, tile, object)
                    }),
            );
        }
    }
    assert_eq!(every.len(), 11);
    let expect = |test: &dyn Fn(&Found) -> bool| {
        every
            .iter()
            .filter(|entry| test(entry))
            .map(|entry| entry.2.clone())
            .collect::<Vec<_>>()
    };
    let search = |server: &mut Server, arguments: &str| {
        object_keys(&pages_reach_every_match(
            server,
            "search_objects",
            arguments,
            "objects",
        ))
    };
    assert_eq!(search(&mut server, "{}"), expect(&|_| true));
    let uncarried = expect(&|entry| !entry.3);
    assert!(uncarried.len() < every.len());
    assert_eq!(search(&mut server, r#"{"carried":false}"#), uncarried);
    assert_eq!(
        search(&mut server, r#"{"plane":"vault"}"#),
        expect(&|entry| entry.0 == "vault")
    );
    assert_eq!(
        search(&mut server, r#"{"plane":"overworld","layer":0}"#),
        expect(&|entry| entry.0 == "overworld" && entry.1 == 0)
    );
    assert!(search(&mut server, r#"{"layer":7}"#).is_empty());
    // A carried object is at its carrier's tile.
    let chest = expect(&|entry| entry.0 == "overworld" && entry.4 == [2, 2]);
    assert_eq!(chest.len(), 4);
    assert_eq!(
        search(&mut server, r#"{"plane":"overworld","bbox":[2,2,2,2]}"#),
        chest
    );
    assert_eq!(
        search(
            &mut server,
            r#"{"plane":"overworld","bbox":[2,2,2,2],"carried":false}"#
        )
        .len(),
        1
    );
    // A chunk at negative coordinates.
    let west = expect(&|entry| entry.4[0] < 0);
    assert_eq!(search(&mut server, r#"{"bbox":[-32,0,-1,31]}"#), west);
    assert_eq!(
        search(
            &mut server,
            r#"{"bbox":[-9007199254740991,-9007199254740991,9007199254740991,9007199254740991]}"#
        ),
        expect(&|_| true)
    );
    assert_eq!(
        search(&mut server, r#"{"proto":"coin"}"#),
        expect(&|entry| entry.5.proto == "coin")
    );
    assert_eq!(
        search(&mut server, r#"{"uid_prefix":"village."}"#),
        expect(&|entry| entry
            .5
            .uid
            .as_deref()
            .is_some_and(|uid| uid.starts_with("village.")))
    );
    assert_eq!(search(&mut server, r#"{"uid_prefix":""}"#).len(), 5);
    // Fields the object sets.
    let set = |name: &str| expect(&|entry| entry.5.fields.iter().any(|(field, _)| field == name));
    assert_eq!(search(&mut server, r#"{"field":"loot"}"#), set("loot"));
    assert_eq!(set("loot").len(), 2);
    assert_eq!(
        search(&mut server, r#"{"field":"count","equals":3}"#).len(),
        1
    );
    assert_eq!(
        search(&mut server, r#"{"field":"tags","contains":"nor"}"#),
        expect(&|entry| entry.5.uid.as_deref() == Some("ruin.north_wall"))
    );
    // Prototype fields are not searched: no object sets face.
    assert!(search(&mut server, r#"{"field":"face"}"#).is_empty());
    // An item says what carries it.
    let (_, document) = result(&mut server, "search_objects", r#"{"uid_prefix":"o."}"#);
    let item = at(structured(&document), &["objects", "0"]);
    assert_eq!(at(item, &["carried"]).as_bool(), Some(true));
    assert_eq!(
        at(item, &["carried_by"]).as_str(),
        Some("village.chest.small")
    );
    assert_eq!(
        at(item, &["path"]).as_str(),
        Some("planes/overworld/layers/0/0_0.jsonc")
    );
    let (_, document) = result(
        &mut server,
        "search_objects",
        r#"{"proto":"coin","field":"count","equals":3}"#,
    );
    let item = at(structured(&document), &["objects", "0"]);
    assert_eq!(at(item, &["carried_by"]).as_str(), Some("village.chest"));
    assert_eq!(at(item, &["value"]).as_i64(), Some(3));
    assert_eq!(at(item, &["value_kind"]).as_str(), Some("integer"));
}

#[test]
fn search_arguments_are_checked() {
    let mut server = ready(&fixture("basic"));
    for (tool, arguments) in [
        ("search_definitions", r#"{"equals":1}"#),
        ("search_definitions", r#"{"equals":"a","contains":"a"}"#),
        ("search_definitions", r#"{"field":"x","equals":[1]}"#),
        ("search_definitions", r#"{"field":"x","equals":{}}"#),
        ("search_definitions", r#"{"type":7}"#),
        ("search_definitions", r#"{"scan_limit":0}"#),
        ("search_definitions", r#"{"scan_limit":10001}"#),
        ("search_definitions", r#"{"plane":"overworld"}"#),
        ("search_objects", r#"{"equals":"x"}"#),
        ("search_objects", r#"{"contains":"x"}"#),
        ("search_objects", r#"{"layer":2147483648}"#),
        ("search_objects", r#"{"layer":"0"}"#),
        ("search_objects", r#"{"bbox":[0,0,1]}"#),
        ("search_objects", r#"{"bbox":[0,0,1,1,1]}"#),
        ("search_objects", r#"{"bbox":[1,0,0,1]}"#),
        ("search_objects", r#"{"bbox":[0,1,1,0]}"#),
        ("search_objects", r#"{"bbox":[0,0,1.5,1]}"#),
        ("search_objects", r#"{"carried":1}"#),
        ("search_objects", r#"{"type":"proto"}"#),
        ("search_objects", r#"{"kind":"object"}"#),
    ] {
        let (_, document) = result(&mut server, tool, arguments);
        assert_eq!(
            tool_error(&document),
            "arguments.invalid",
            "{tool} {arguments}"
        );
    }
    let long = "a".repeat(257);
    for key in ["type", "field", "contains", "equals"] {
        let (_, document) = result(
            &mut server,
            "search_definitions",
            &format!(r#"{{"field":"f","{key}":"{long}"}}"#)
                .replace(r#""field":"f","field""#, r#""field""#),
        );
        assert_eq!(tool_error(&document), "arguments.invalid", "{key}");
    }
    for key in ["plane", "proto", "uid_prefix"] {
        let (_, document) = result(
            &mut server,
            "search_objects",
            &format!(r#"{{"{key}":"{long}"}}"#),
        );
        assert_eq!(tool_error(&document), "arguments.invalid", "{key}");
    }
}

/// Each read tool, with arguments that succeed on `fixtures/basic`.
const READ_CALLS: [(&str, &str); 6] = [
    ("world_summary", ""),
    ("validate", ""),
    ("search_ids", r#""kind":"plane""#),
    ("search_definitions", ""),
    ("search_objects", ""),
    ("inspect", r#""kind":"plane","id":"overworld""#),
];

fn with_revision(arguments: &str, revision: &str) -> String {
    let separator = if arguments.is_empty() { "" } else { "," };
    format!(r#"{{{arguments}{separator}"revision":{revision}}}"#)
}

#[test]
#[allow(clippy::disallowed_methods)]
fn a_revision_argument_fails_once_the_snapshot_moved() {
    let directory = tempfile::tempdir().unwrap();
    copy_tree(&fixture("basic"), directory.path());
    let mut server = ready(directory.path());
    let revision = server.revision().unwrap().to_owned();
    let other = "0".repeat(64);
    let text = |document: &Document| {
        at(document.root(), &["result", "content", "0", "text"])
            .as_str()
            .unwrap()
            .to_owned()
    };
    for (tool, arguments) in READ_CALLS {
        let (reply, document) = result(
            &mut server,
            tool,
            &with_revision(arguments, &format!("\"{revision}\"")),
        );
        assert!(reply.contains("\"isError\":false"), "{tool}: {reply}");
        assert_eq!(
            at(structured(&document), &["revision"]).as_str(),
            Some(revision.as_str())
        );
        let (_, document) = result(
            &mut server,
            tool,
            &with_revision(arguments, &format!("\"{other}\"")),
        );
        assert_eq!(tool_error(&document), "revision.changed", "{tool}");
        assert!(text(&document).ends_with(&format!("the current revision is {revision}")));
        for malformed in [
            "\"abc\"",
            "7",
            "null",
            &format!("\"{}\"", revision.to_uppercase()),
        ] {
            let (_, document) = result(&mut server, tool, &with_revision(arguments, malformed));
            assert_eq!(
                tool_error(&document),
                "arguments.invalid",
                "{tool} {malformed}"
            );
        }
    }
    let (_, document) = result(&mut server, "refresh", r#"{"revision":"x"}"#);
    assert_eq!(tool_error(&document), "arguments.invalid");

    // The tree changes and is loaded again: the old revision fails.
    let path = directory.path().join("defs/proto/coin.jsonc");
    let coin = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("{coin}\n")).unwrap();
    let (_, document) = result(&mut server, "refresh", "{}");
    assert_eq!(
        at(structured(&document), &["changed"]).as_bool(),
        Some(true)
    );
    let current = server.revision().unwrap().to_owned();
    for (tool, arguments) in READ_CALLS {
        let (_, document) = result(
            &mut server,
            tool,
            &with_revision(arguments, &format!("\"{revision}\"")),
        );
        assert_eq!(tool_error(&document), "revision.changed", "{tool}");
        assert!(text(&document).ends_with(&current));
    }

    // A tree that does not load has no revision: any revision fails, even
    // for validate, which answers without one.
    std::fs::write(&path, "{").unwrap();
    let (_, document) = result(&mut server, "refresh", "{}");
    assert_eq!(at(structured(&document), &["valid"]).as_bool(), Some(false));
    for (tool, arguments) in READ_CALLS {
        let (_, document) = result(
            &mut server,
            tool,
            &with_revision(arguments, &format!("\"{current}\"")),
        );
        assert_eq!(tool_error(&document), "revision.changed", "{tool}");
        assert!(
            text(&document)
                .ends_with("the tree does not load now, so there is no current revision")
        );
    }
    let (reply, _) = result(&mut server, "validate", "{}");
    assert!(reply.contains("\"isError\":false"));
}

#[test]
fn the_new_tools_need_a_tree_that_loads() {
    let tree = invalid_tree();
    let mut server = ready(tree.path());
    for (tool, arguments) in [
        ("search_definitions", "{}"),
        ("search_objects", "{}"),
        ("inspect", r#"{"kind":"plane","id":"overworld"}"#),
    ] {
        let (reply, document) = result(&mut server, tool, arguments);
        assert_eq!(tool_error(&document), "tree.invalid");
        golden(&format!("invalid-{tool}"), &reply);
    }
    // Arguments are checked first.
    let (_, document) = result(&mut server, "inspect", r#"{"kind":"plane"}"#);
    assert_eq!(tool_error(&document), "arguments.invalid");
}

#[test]
#[allow(clippy::disallowed_methods)]
fn search_cursors_are_bound_to_revision_tool_and_arguments() {
    let directory = tempfile::tempdir().unwrap();
    copy_tree(&fixture("basic"), directory.path());
    let mut server = ready(directory.path());
    let cursor_of = |server: &mut Server, tool: &str, arguments: &str| {
        let (_, document) = result(server, tool, arguments);
        at(structured(&document), &["next_cursor"])
            .as_str()
            .unwrap()
            .to_owned()
    };
    let definitions = cursor_of(
        &mut server,
        "search_definitions",
        r#"{"type":"proto","limit":2}"#,
    );
    let objects = cursor_of(&mut server, "search_objects", r#"{"scan_limit":2}"#);
    let with = |arguments: &str, cursor: &str| {
        let separator = if arguments.is_empty() { "" } else { "," };
        format!(r#"{{{arguments}{separator}"cursor":"{cursor}"}}"#)
    };
    // The limit and scan budget may change from page to page.
    let (_, document) = result(
        &mut server,
        "search_definitions",
        &with(r#""type":"proto","limit":3,"scan_limit":4"#, &definitions),
    );
    assert_eq!(at(structured(&document), &["offset"]).as_i64(), Some(2));
    let (_, document) = result(
        &mut server,
        "search_objects",
        &with(r#""limit":1"#, &objects),
    );
    assert_eq!(at(structured(&document), &["offset"]).as_i64(), Some(2));
    for (tool, arguments, cursor) in [
        ("search_definitions", r#""type":"loot""#, &definitions),
        ("search_definitions", "", &definitions),
        (
            "search_definitions",
            r#""type":"proto","field":"layer""#,
            &definitions,
        ),
        (
            "search_definitions",
            r#""type":"proto","contains":"c""#,
            &definitions,
        ),
        ("search_objects", r#""carried":false"#, &objects),
        ("search_objects", r#""plane":"overworld""#, &objects),
        ("search_objects", r#""layer":0"#, &objects),
        ("search_objects", r#""bbox":[0,0,9,9]"#, &objects),
        ("search_objects", r#""field":"count","equals":3"#, &objects),
        (
            "search_objects",
            r#""field":"count","equals":3.5"#,
            &objects,
        ),
        ("search_ids", r#""kind":"object""#, &objects),
    ] {
        let (_, document) = result(&mut server, tool, &with(arguments, cursor));
        assert_eq!(
            tool_error(&document),
            "cursor.invalid",
            "{tool} {arguments}"
        );
    }
    // Past the end of the candidates.
    let past = format!("{}.99", objects.rsplit_once('.').unwrap().0);
    let (_, document) = result(&mut server, "search_objects", &with("", &past));
    assert_eq!(tool_error(&document), "cursor.invalid");
    let last = format!("{}.11", objects.rsplit_once('.').unwrap().0);
    let (_, document) = result(&mut server, "search_objects", &with("", &last));
    assert_eq!(tool_error(&document), "cursor.invalid");
    let final_one = format!("{}.10", objects.rsplit_once('.').unwrap().0);
    let (_, document) = result(&mut server, "search_objects", &with("", &final_one));
    assert_eq!(
        at(structured(&document), &["objects"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // A changed tree makes them stale.
    let path = directory.path().join("defs/proto/coin.jsonc");
    let coin = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("{coin}\n")).unwrap();
    let _ = result(&mut server, "refresh", "{}");
    let (_, document) = result(
        &mut server,
        "search_definitions",
        &with(r#""type":"proto""#, &definitions),
    );
    assert_eq!(tool_error(&document), "cursor.stale");
    let (_, document) = result(&mut server, "search_objects", &with("", &objects));
    assert_eq!(tool_error(&document), "cursor.stale");
}

#[test]
#[allow(clippy::disallowed_methods)]
fn a_search_page_stops_at_the_byte_budget_as_sent() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    copy_tree(&fixture("basic"), root);
    // 600 prototypes whose footprint is a long string of quotes and
    // backslashes, which escaping doubles twice over.
    let footprint = "\\\"\\\\".repeat(1000);
    for index in 0..600 {
        std::fs::write(
            root.join(format!("defs/proto/p{index:03}.jsonc")),
            format!(
                r#"{{"format": "tilewright.def/1", "type": "proto", "id": "p{index:03}", "fields": {{"face": "p.png", "layer": "item", "footprint": "{footprint}"}}}}"#
            ),
        )
        .unwrap();
    }
    // 600 objects whose name is 256 quotes.
    let mut objects = String::new();
    for index in 0..600 {
        if index > 0 {
            objects.push(',');
        }
        let _ = write!(
            objects,
            r#"{{"at": [{}, {}], "proto": "coin", "fields": {{"name": "{}"}}}}"#,
            index % 32,
            index / 32 % 32,
            "\\\"".repeat(256)
        );
    }
    std::fs::write(
        root.join("planes/vault/layers/0/0_0.jsonc"),
        format!(
            r#"{{"format": "tilewright.chunk/1", "plane": "vault", "layer": 0, "chunk": [0, 0], "terrain": {{"floor": {{"palette": {{"c": "cave_floor"}}, "fill": "c"}}}}, "objects": [{objects}]}}"#
        ),
    )
    .unwrap();
    let mut server = ready(root);
    assert!(server.revision().is_some(), "the generated tree loads");
    for (tool, arguments, field) in [
        (
            "search_definitions",
            r#"{"field":"footprint","limit":500}"#,
            "definitions",
        ),
        (
            "search_objects",
            r#"{"field":"name","limit":500}"#,
            "objects",
        ),
    ] {
        let replies = send(&mut server, &[&call(7, tool, arguments)]);
        let reply = &replies[0];
        assert!(reply.len() <= MAXIMUM_RESULT_BYTES, "{}", reply.len());
        assert!(
            reply.len() > MAXIMUM_RESULT_BYTES * 3 / 4,
            "{}",
            reply.len()
        );
        let document = parse(reply);
        let page = structured(&document);
        assert_eq!(at(page, &["byte_budget_reached"]).as_bool(), Some(true));
        assert_eq!(at(page, &["scan_budget_reached"]).as_bool(), Some(false));
        let returned = at(page, &[field]).as_array().unwrap().len();
        assert!(returned > 0 && returned < 500, "{returned}");
        let inner = &arguments[1..arguments.find(",\"limit\"").unwrap()];
        let (all, _, _) = exhaust_search(
            &mut server,
            tool,
            &format!("{{{inner}}}"),
            field,
            500,
            10_000,
        );
        let unique: BTreeSet<&String> = all.iter().collect();
        assert_eq!(unique.len(), all.len());
        // The fixture's wall sets a footprint too, and its ruined wall a
        // name.
        assert_eq!(all.len(), 601, "{tool}");
    }
}

/// Every call of the new tools on the tree `server` serves, through every
/// page, for the read-only tests.
pub(super) fn every_new_tool(server: &mut Server) {
    for kind in KINDS {
        for id in ids(server, kind) {
            let (reply, _) = result(server, "inspect", &inspect_arguments(kind, &id));
            assert!(reply.contains("\"isError\":false"), "{reply}");
        }
    }
    for (tool, field) in [
        ("search_definitions", "definitions"),
        ("search_objects", "objects"),
    ] {
        exhaust_search(server, tool, "{}", field, 2, 3);
    }
    exhaust_search(
        server,
        "search_definitions",
        r#"{"field":"layer","contains":"i"}"#,
        "definitions",
        1,
        1,
    );
}

#[test]
fn the_new_tools_leave_the_tree_as_it_was() {
    // The parent's read-only tests call every_new_tool too; this one
    // checks the inspection of a tree that changed under the server.
    let directory = tempfile::tempdir().unwrap();
    copy_tree(&fixture("basic"), directory.path());
    let mut server = ready(directory.path());
    let before = super::listing(directory.path());
    every_new_tool(&mut server);
    assert_eq!(super::listing(directory.path()), before);
}
