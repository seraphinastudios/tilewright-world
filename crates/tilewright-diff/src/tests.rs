// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

use crate::{Allowance, Allowlist, Error, Input, Kind, Reason, Report, Side, Snapshot, diff, load};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/basic")
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A writable copy of the basic fixture.
struct Copy {
    directory: tempfile::TempDir,
}

impl Copy {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        copy_tree(&fixture(), &directory.path().join("world"));
        Self { directory }
    }

    fn root(&self) -> PathBuf {
        self.directory.path().join("world")
    }

    /// Replaces every `from` in a file, which must contain it.
    fn edit(self, path: &str, from: &str, to: &str) -> Self {
        let path = self.root().join(path);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(from), "{} lacks {from}", path.display());
        std::fs::write(&path, text.replace(from, to)).unwrap();
        self
    }

    fn write(self, path: &str, text: &str) -> Self {
        let path = self.root().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
        self
    }

    fn remove(self, path: &str) -> Self {
        let path = self.root().join(path);
        if path.is_dir() {
            std::fs::remove_dir_all(path).unwrap();
        } else {
            std::fs::remove_file(path).unwrap();
        }
        self
    }

    fn rename(self, from: &str, to: &str) -> Self {
        std::fs::rename(self.root().join(from), self.root().join(to)).unwrap();
        self
    }

    fn source(&self) -> Snapshot {
        load(&self.root()).unwrap_or_else(|error| panic!("{error:?}"))
    }

    /// The snapshot of the pack compiled from the copy.
    fn pack(&self) -> Snapshot {
        let world = tilewright_source::load(&self.root()).unwrap();
        let written =
            tilewright_pack::write_pack(&world, &self.directory.path().join("packs")).unwrap();
        let snapshot = load(&written.path).unwrap();
        assert_eq!(snapshot.side().input, Input::Pack);
        snapshot
    }
}

/// The report's lines without the summary.
fn lines(report: &Report) -> Vec<String> {
    let text = report.to_text();
    let (body, summary) = text.trim_end().rsplit_once('\n').unwrap_or(("", &text));
    assert!(summary.starts_with("summary: "), "{text}");
    body.lines().map(str::to_owned).collect()
}

fn allow(entries: &str) -> Allowlist {
    Allowlist::parse(&format!(
        "{{\"format\": \"tilewright.diff-allow/1\", {entries}}}"
    ))
    .unwrap()
}

/// The fixture with one more ID of `kind`, and that ID.
fn with_extra(kind: Kind) -> (Copy, &'static str) {
    let copy = Copy::new();
    match kind {
        Kind::Anchor => (
            copy.edit(
                "planes/overworld/anchors.jsonc",
                "\"anchors\": [",
                "\"anchors\": [\n    {\"id\": \"village.gate\", \"layer\": 0, \"at\": [8, 8]},",
            ),
            "overworld/village.gate",
        ),
        Kind::Definition => (
            copy.write(
                "defs/proto/rock.jsonc",
                r#"{"format": "tilewright.def/1", "type": "proto", "id": "rock", "fields": {"face": "rock.png", "layer": "item"}}"#,
            ),
            "proto/rock",
        ),
        Kind::Plane => (
            copy.edit(
                "world.jsonc",
                "\"planes\": [\"overworld\", \"vault\"]",
                "\"planes\": [\"overworld\", \"vault\", \"annex\"]",
            )
            .write(
                "planes/annex/plane.jsonc",
                r#"{"format": "tilewright.plane/1", "id": "annex", "kind": "instanced", "chunk_size": 32}"#,
            ),
            "annex",
        ),
        Kind::RenderLayer => (
            copy.edit(
                "profile/profile.jsonc",
                "\"living\", \"effect\"]",
                "\"living\", \"effect\", \"sky\"]",
            ),
            "sky",
        ),
        Kind::Stairwell => (
            copy.edit(
                "planes/overworld/stairwells.jsonc",
                "\"stairwells\": [",
                "\"stairwells\": [\n    {\"id\": \"well.shaft\", \"steps\": [{\"layer\": 0, \"at\": [5, 5], \"proto\": \"stairs_down\"}, {\"layer\": -1, \"at\": [5, 5], \"proto\": \"stairs_up\"}]},",
            ),
            "overworld/well.shaft",
        ),
        Kind::Zone => (
            copy.edit(
                "planes/overworld/zones.jsonc",
                "\"zones\": [",
                "\"zones\": [\n    {\"id\": \"meadow\", \"parent\": \"island\"},",
            ),
            "overworld/meadow",
        ),
    }
}

/// The fixture with one ID of `kind` renamed, every reference to it
/// updated, and a display name changed where the kind has one; and the
/// report line for it.
fn with_renamed(kind: Kind) -> (Copy, &'static str) {
    let copy = Copy::new();
    match kind {
        Kind::Anchor => (
            copy.edit(
                "planes/overworld/anchors.jsonc",
                "\"village.well\"",
                "\"village.fountain\"",
            )
            .edit(
                "planes/overworld/layers/-1/0_0.jsonc",
                "\"anchor\": \"village.well\"",
                "\"anchor\": \"village.fountain\"",
            ),
            "renamed anchor overworld/village.well -> overworld/village.fountain (same content)",
        ),
        Kind::Definition => (
            copy.rename("defs/proto/tree.jsonc", "defs/proto/oak.jsonc")
                .edit(
                    "defs/proto/oak.jsonc",
                    "\"id\": \"tree\"",
                    "\"id\": \"oak\"",
                )
                .edit(
                    "planes/overworld/layers/0/0_0.jsonc",
                    "\"proto\": \"tree\"",
                    "\"proto\": \"oak\"",
                ),
            "renamed definition proto/tree -> proto/oak (same content)",
        ),
        Kind::Plane => (
            copy.edit("world.jsonc", "\"vault\"]", "\"strongroom\"]")
                .rename("planes/vault", "planes/strongroom")
                .edit(
                    "planes/strongroom/plane.jsonc",
                    "\"id\": \"vault\",",
                    "\"id\": \"strongroom\",\n  \"title\": \"Strongroom\",",
                )
                .edit(
                    "planes/strongroom/layers/0/0_0.jsonc",
                    "\"plane\": \"vault\"",
                    "\"plane\": \"strongroom\"",
                )
                .edit(
                    "planes/overworld/layers/0/0_0.jsonc",
                    "\"plane\": \"vault\"",
                    "\"plane\": \"strongroom\"",
                ),
            "renamed plane vault -> strongroom (same content)",
        ),
        Kind::RenderLayer => (
            copy.edit(
                "profile/profile.jsonc",
                "\"living\", \"effect\"]",
                "\"living\", \"effects\"]",
            )
            .edit(
                "profile/schema/proto.jsonc",
                "\"living\", \"effect\"]",
                "\"living\", \"effects\"]",
            ),
            "renamed render_layer effect -> effects (same content)",
        ),
        Kind::Stairwell => (
            copy.edit(
                "planes/overworld/stairwells.jsonc",
                "\"id\": \"cave.chute\"",
                "\"id\": \"cave.slide\"",
            ),
            "renamed stairwell overworld/cave.chute -> overworld/cave.slide (same content)",
        ),
        Kind::Zone => (
            copy.edit(
                "planes/overworld/zones.jsonc",
                "\"id\": \"village\"",
                "\"id\": \"hamlet\"",
            )
            .edit(
                "planes/overworld/zones.jsonc",
                "Little Village",
                "Little Hamlet",
            )
            .edit(
                "planes/overworld/layers/0/0_0.jsonc",
                "\"v\": \"village\"",
                "\"v\": \"hamlet\"",
            ),
            "renamed zone overworld/village -> overworld/hamlet (same content)",
        ),
    }
}

#[test]
fn kinds_sort_and_parse_by_name() {
    let mut names: Vec<&str> = Kind::ALL.iter().map(|kind| kind.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        Kind::ALL
            .iter()
            .map(|kind| kind.as_str())
            .collect::<Vec<_>>()
    );
    for kind in Kind::ALL {
        assert_eq!(Kind::parse(kind.as_str()), Some(kind));
    }
    assert_eq!(Kind::parse("proto"), None);
    assert!(Kind::Zone.is_id("overworld/village") && !Kind::Zone.is_id("village"));
    assert!(Kind::Definition.is_id("proto/tree") && !Kind::Definition.is_id("proto/"));
    assert!(Kind::Plane.is_id("vault") && !Kind::Plane.is_id("a/b"));
    assert!(!Kind::RenderLayer.is_id("Wall") && !Kind::Anchor.is_id("a/b/c"));
}

#[test]
fn identical_worlds_have_an_empty_diff() {
    let copy = Copy::new();
    let (source, pack) = (copy.source(), copy.pack());
    // A source tree and its pack hold the same IDs with the same content.
    assert_eq!(source.ids, pack.ids);
    assert_eq!(source.side().source_digest, pack.side().source_digest);
    for (old, new) in [(&source, &source), (&source, &pack), (&pack, &source)] {
        let report = diff(old, new, &Allowlist::default());
        assert!(lines(&report).is_empty(), "{}", report.to_text());
        assert!(!report.breaking());
        assert_eq!(
            report.to_text(),
            "summary: 0 removed, 0 renamed, 0 added, 0 not allowed\n"
        );
    }
    let json = diff(&source, &pack, &Allowlist::default()).to_json();
    assert!(json.starts_with("{\"format\":\"tilewright.diff/1\",\"old\":{\"input\":\"source\",\"profile\":\"example\",\"profile_version\":\"1.0.0\",\"source_digest\":\""), "{json}");
    assert!(json.contains("\"new\":{\"input\":\"pack\","), "{json}");
    assert!(
        json.ends_with(
            "\"breaking\":false,\"removed\":[],\"renamed\":[],\"added\":[],\"unused\":[]}\n"
        ),
        "{json}"
    );
}

#[test]
fn each_kind_reports_removed_added_and_renamed_ids() {
    let base = Copy::new();
    let (old, old_pack) = (base.source(), base.pack());
    let none = Allowlist::default();
    for kind in Kind::ALL {
        let (extra, id) = with_extra(kind);
        let (extra, extra_pack) = (extra.source(), extra.pack());
        let report = diff(&old, &extra, &none);
        assert_eq!(lines(&report), [format!("added {kind} {id}")]);
        assert!(!report.breaking());
        let report = diff(&extra, &old, &none);
        assert_eq!(lines(&report), [format!("removed {kind} {id}")]);
        assert!(report.breaking());
        // Packs and sources give the same report.
        assert_eq!(diff(&extra_pack, &old, &none).to_text(), report.to_text());
        assert_eq!(diff(&extra, &old_pack, &none).to_text(), report.to_text());

        let (renamed, line) = with_renamed(kind);
        let (renamed, renamed_pack) = (renamed.source(), renamed.pack());
        let report = diff(&old, &renamed, &none);
        assert_eq!(lines(&report), [line], "{kind}");
        assert!(report.breaking());
        assert_eq!(report.not_allowed(), 1);
        assert_eq!(diff(&old, &renamed_pack, &none).to_text(), report.to_text());
        assert_eq!(diff(&old_pack, &renamed, &none).to_text(), report.to_text());
    }
}

#[test]
fn a_rename_that_changes_content_is_a_removal_and_an_addition() {
    let old = Copy::new().source();
    let (renamed, _) = with_renamed(Kind::Definition);
    let new = renamed
        .edit("defs/proto/oak.jsonc", "tree.png", "oak.png")
        .source();
    assert_eq!(
        lines(&diff(&old, &new, &Allowlist::default())),
        [
            "removed definition proto/tree",
            "added definition proto/oak"
        ]
    );
    // An entry that renames it says what happened.
    let report = diff(
        &old,
        &new,
        &allow(r#""rename": [{"kind": "definition", "from": "proto/tree", "to": "proto/oak"}]"#),
    );
    assert_eq!(
        lines(&report),
        ["renamed definition proto/tree -> proto/oak (declared, allowed)"]
    );
    assert!(!report.breaking());
}

/// The vault's anchors document, with one anchor.
const VAULT_ANCHORS: &str =
    r#"{"format": "tilewright.anchors/1", "anchors": [{"id": "door", "layer": 0, "at": [1, 2]}]}"#;

/// The fixture without the vault plane.
fn without_vault() -> Copy {
    Copy::new()
        .edit("world.jsonc", ", \"vault\"]", "]")
        .remove("planes/vault")
        .edit(
            "planes/overworld/layers/0/0_0.jsonc",
            "\"portal\": {\"plane\": \"vault\", ",
            "\"portal\": {",
        )
}

#[test]
fn ids_of_a_renamed_plane_pair_with_those_of_its_new_id() {
    let old = Copy::new()
        .write("planes/vault/anchors.jsonc", VAULT_ANCHORS)
        .source();
    let (renamed, _) = with_renamed(Kind::Plane);
    let renamed = renamed.write("planes/strongroom/anchors.jsonc", VAULT_ANCHORS);
    let report = diff(&old, &renamed.source(), &Allowlist::default());
    assert_eq!(
        lines(&report),
        [
            "renamed anchor vault/door -> strongroom/door (same content)",
            "renamed plane vault -> strongroom (same content)",
        ]
    );
    assert_eq!(report.not_allowed(), 2);

    // A declared rename of the plane accepts its anchors, zones and
    // stairwells that keep their IDs in the new plane.
    let report = diff(
        &old,
        &renamed.pack(),
        &allow(r#""rename": [{"kind": "plane", "from": "vault", "to": "strongroom"}]"#),
    );
    assert_eq!(
        lines(&report),
        [
            "renamed anchor vault/door -> strongroom/door (same content, allowed)",
            "renamed plane vault -> strongroom (declared, allowed)",
        ]
    );
    assert!(!report.breaking());

    // Removing a plane removes its anchors; an entry that removes the plane
    // accepts both.
    let gone = without_vault().source();
    let report = diff(&old, &gone, &Allowlist::default());
    assert_eq!(
        lines(&report),
        ["removed anchor vault/door", "removed plane vault"]
    );
    let report = diff(
        &old,
        &gone,
        &allow(r#""remove": [{"kind": "plane", "id": "vault"}]"#),
    );
    assert_eq!(
        lines(&report),
        [
            "removed anchor vault/door (allowed)",
            "removed plane vault (allowed)"
        ]
    );
    assert!(!report.breaking());

    // Renaming the plane into one without the anchor does not accept the
    // anchor's removal.
    let report = diff(
        &old,
        &gone,
        &allow(r#""rename": [{"kind": "plane", "from": "vault", "to": "overworld"}]"#),
    );
    assert_eq!(
        lines(&report),
        [
            "removed anchor vault/door",
            "renamed plane vault -> overworld (declared, allowed)"
        ]
    );
    assert!(report.breaking());
}

#[test]
fn allowlists_accept_declared_removals_and_renames() {
    let old = Copy::new().source();
    let (renamed, _) = with_renamed(Kind::Definition);
    let renamed = renamed.source();
    let (extra, _) = with_extra(Kind::Definition);
    let extra = extra.source();

    let report = diff(
        &old,
        &renamed,
        &allow(r#""rename": [{"kind": "definition", "from": "proto/tree", "to": "proto/oak"}]"#),
    );
    assert_eq!(
        lines(&report),
        ["renamed definition proto/tree -> proto/oak (declared, allowed)"]
    );
    assert!(!report.breaking());

    // A removal entry accepts the old ID however it went away.
    let removal = allow(r#""remove": [{"kind": "definition", "id": "proto/tree"}]"#);
    let report = diff(&old, &renamed, &removal);
    assert_eq!(
        lines(&report),
        ["renamed definition proto/tree -> proto/oak (same content, allowed)"]
    );
    assert!(!report.breaking());
    let report = diff(
        &extra,
        &old,
        &allow(r#""remove": [{"kind": "definition", "id": "proto/rock"}]"#),
    );
    assert_eq!(lines(&report), ["removed definition proto/rock (allowed)"]);
    assert!(!report.breaking());

    // Entries that do not apply are reported, and a rename to an ID the new
    // world lacks accepts nothing.
    let report = diff(
        &old,
        &renamed,
        &allow(
            r#""remove": [{"kind": "definition", "id": "proto/coin"}],
               "rename": [{"kind": "definition", "from": "proto/tree", "to": "proto/pine"},
                          {"kind": "zone", "from": "overworld/nowhere", "to": "overworld/village"}]"#,
        ),
    );
    assert_eq!(
        lines(&report),
        [
            "renamed definition proto/tree -> proto/oak (same content)",
            "unused allowance: remove definition proto/coin (not removed)",
            "unused allowance: rename definition proto/tree -> proto/pine (target missing)",
            "unused allowance: rename zone overworld/nowhere -> overworld/village (not removed)",
        ]
    );
    assert!(report.breaking());
    assert_eq!(
        report.unused[1].allowance,
        Allowance::Rename {
            kind: Kind::Definition,
            from: "proto/tree".into(),
            to: "proto/pine".into()
        }
    );
    assert_eq!(report.unused[1].reason, Reason::TargetMissing);
}

/// A snapshot with IDs whose content is the given text.
fn snapshot(entries: &[(Kind, &str, &str)]) -> Snapshot {
    let mut ids: BTreeMap<Kind, BTreeMap<String, [u8; 32]>> = BTreeMap::new();
    for (kind, id, content) in entries {
        ids.entry(*kind)
            .or_default()
            .insert((*id).to_owned(), Sha256::digest(content).into());
    }
    Snapshot {
        side: Side {
            input: Input::Source,
            profile: "example".into(),
            profile_version: "1".into(),
            source_digest: "00".into(),
        },
        ids,
    }
}

#[test]
fn renames_pair_only_unambiguous_ids_of_the_same_scope() {
    use Kind::{Definition, Plane, RenderLayer, Zone};
    let none = Allowlist::default();
    // Two removed and two added IDs with the same content: no pairs.
    let report = diff(
        &snapshot(&[(Definition, "proto/a", "x"), (Definition, "proto/b", "x")]),
        &snapshot(&[(Definition, "proto/c", "x"), (Definition, "proto/d", "x")]),
        &none,
    );
    assert_eq!(
        lines(&report),
        [
            "removed definition proto/a",
            "removed definition proto/b",
            "added definition proto/c",
            "added definition proto/d"
        ]
    );
    // One removed and two added: no pair either.
    let report = diff(
        &snapshot(&[(Definition, "proto/a", "x")]),
        &snapshot(&[(Definition, "proto/c", "x"), (Definition, "proto/d", "x")]),
        &none,
    );
    assert!(report.renamed.is_empty());
    // Another definition type, or another plane that was not renamed, is
    // another scope.
    let report = diff(
        &snapshot(&[
            (Definition, "proto/a", "x"),
            (Plane, "p", "1"),
            (Plane, "q", "2"),
            (Zone, "p/z", "y"),
        ]),
        &snapshot(&[
            (Definition, "loot/a", "x"),
            (Plane, "p", "1"),
            (Plane, "q", "2"),
            (Zone, "q/z", "y"),
        ]),
        &none,
    );
    assert!(report.renamed.is_empty(), "{}", report.to_text());
    // Unrelated IDs with other content stay apart, in sorted order.
    let report = diff(
        &snapshot(&[
            (RenderLayer, "wall", "3"),
            (RenderLayer, "floor", "0"),
            (Definition, "proto/b", "y"),
        ]),
        &snapshot(&[
            (RenderLayer, "walls", "3"),
            (RenderLayer, "ground", "1"),
            (Definition, "proto/a", "x"),
        ]),
        &none,
    );
    assert_eq!(
        lines(&report),
        [
            "removed definition proto/b",
            "removed render_layer floor",
            "renamed render_layer wall -> walls (same content)",
            "added definition proto/a",
            "added render_layer ground"
        ]
    );
}

#[test]
fn declared_renames_take_precedence_and_may_merge_ids() {
    use Kind::Definition;
    let old = snapshot(&[(Definition, "proto/a", "x"), (Definition, "proto/b", "y")]);
    let new = snapshot(&[(Definition, "proto/b", "y"), (Definition, "proto/c", "x")]);
    // The content rule would pair a with c; the entry pairs it with b, which
    // the old world had too.
    let report = diff(
        &old,
        &new,
        &allow(r#""rename": [{"kind": "definition", "from": "proto/a", "to": "proto/b"}]"#),
    );
    assert_eq!(
        lines(&report),
        [
            "renamed definition proto/a -> proto/b (declared, allowed)",
            "added definition proto/c"
        ]
    );
    assert!(!report.breaking());
}

#[test]
fn reports_are_deterministic_json() {
    use Kind::{Anchor, Definition, Plane, Zone};
    let mut old = snapshot(&[
        (Zone, "p/z", "z"),
        (Plane, "p", "p"),
        (Definition, "proto/b", "b"),
        (Definition, "proto/a", "a"),
        (Anchor, "p/x", "x"),
    ]);
    old.side.profile_version = "1\u{1b}[2J\u{7f}\u{85}\"\\".into();
    let new = snapshot(&[
        (Plane, "p", "p"),
        (Definition, "proto/c", "a"),
        (Anchor, "p/y", "y"),
    ]);
    let allowlist =
        allow(r#""remove": [{"kind": "zone", "id": "p/z"}, {"kind": "plane", "id": "q"}]"#);
    let report = diff(&old, &new, &allowlist);
    let json = report.to_json();
    assert_eq!(
        json,
        concat!(
            "{\"format\":\"tilewright.diff/1\",",
            "\"old\":{\"input\":\"source\",\"profile\":\"example\",\"profile_version\":\"1\\u001b[2J\\u007f\\u0085\\\"\\\\\",\"source_digest\":\"00\"},",
            "\"new\":{\"input\":\"source\",\"profile\":\"example\",\"profile_version\":\"1\",\"source_digest\":\"00\"},",
            "\"breaking\":true,",
            "\"removed\":[{\"kind\":\"anchor\",\"id\":\"p/x\",\"allowed\":false},",
            "{\"kind\":\"definition\",\"id\":\"proto/b\",\"allowed\":false},",
            "{\"kind\":\"zone\",\"id\":\"p/z\",\"allowed\":true}],",
            "\"renamed\":[{\"kind\":\"definition\",\"from\":\"proto/a\",\"to\":\"proto/c\",\"basis\":\"content\",\"allowed\":false}],",
            "\"added\":[{\"kind\":\"anchor\",\"id\":\"p/y\"}],",
            "\"unused\":[{\"allowance\":\"remove\",\"kind\":\"plane\",\"id\":\"q\",\"reason\":\"not_removed\"}]}\n"
        )
    );
    assert_eq!(diff(&old, &new, &allowlist).to_json(), json);
    let parsed =
        tilewright_jsonc::Document::parse(json.trim_end(), &tilewright_jsonc::Limits::default())
            .expect("the report is JSON");
    assert_eq!(
        parsed
            .root()
            .get("old")
            .and_then(|old| old.get("profile_version"))
            .and_then(|version| version.as_str()),
        Some("1\u{1b}[2J\u{7f}\u{85}\"\\")
    );
    assert_eq!(
        report.to_text(),
        "removed anchor p/x\nremoved definition proto/b\nremoved zone p/z (allowed)\nrenamed definition proto/a -> proto/c (same content)\nadded anchor p/y\nunused allowance: remove plane q (not removed)\nsummary: 3 removed, 1 renamed, 1 added, 3 not allowed\n"
    );

    // Loading the same world twice gives the same snapshot.
    assert_eq!(Copy::new().source(), Copy::new().source());
}

#[test]
fn allowlists_are_strict() {
    const HEAD: &str = r#"{"format": "tilewright.diff-allow/1", "#;
    for (text, code, line, column) in [
        ("{}".to_owned(), "shape.missing_key", 1, 1),
        ("[]".to_owned(), "shape.type", 1, 1),
        (
            r#"{"format": "tilewright.diff/1"}"#.to_owned(),
            "shape.format",
            1,
            12,
        ),
        (format!("{HEAD}\"extra\": 1}}"), "shape.unknown_key", 1, 39),
        (format!("{HEAD}\"remove\": {{}}}}"), "shape.type", 1, 49),
        (format!("{HEAD}\"remove\": [1]}}"), "shape.type", 1, 50),
        (
            format!(r#"{HEAD}"remove": [{{"kind": "proto", "id": "a"}}]}}"#),
            "shape.range",
            1,
            59,
        ),
        (
            format!(r#"{HEAD}"remove": [{{"kind": "zone", "id": "village"}}]}}"#),
            "shape.identifier",
            1,
            73,
        ),
        (
            format!(r#"{HEAD}"remove": [{{"kind": "plane", "id": "a/b"}}]}}"#),
            "shape.identifier",
            1,
            74,
        ),
        (
            format!(r#"{HEAD}"remove": [{{"kind": "plane"}}]}}"#),
            "shape.missing_key",
            1,
            50,
        ),
        (
            format!(r#"{HEAD}"remove": [{{"kind": "plane", "id": 1}}]}}"#),
            "shape.type",
            1,
            74,
        ),
        (
            format!(r#"{HEAD}"remove": [{{"kind": "plane", "id": "a", "why": ""}}]}}"#),
            "shape.unknown_key",
            1,
            79,
        ),
        (
            format!(r#"{HEAD}"rename": [{{"kind": "plane", "from": "a", "to": "a"}}]}}"#),
            "shape.range",
            1,
            50,
        ),
        (
            format!(
                r#"{HEAD}"remove": [{{"kind": "plane", "id": "a"}}], "rename": [{{"kind": "plane", "from": "a", "to": "b"}}]}}"#
            ),
            "ref.duplicate",
            1,
            118,
        ),
        (
            format!(r#"{HEAD}"remove": [],}}"#),
            "jsonc.trailing_comma",
            1,
            51,
        ),
        (
            "{\n  \"format\": \"tilewright.diff-allow/1\",\n  \"remove\": [{\"kind\": \"zone\", \"id\": \"x\"}]\n}"
                .to_owned(),
            "shape.identifier",
            3,
            37,
        ),
    ] {
        let error = Allowlist::parse(&text).expect_err(&text);
        assert_eq!(
            (error.code, error.line, error.column),
            (code, line, column),
            "{text}: {error}"
        );
    }

    // Comments are allowed, and an allowlist may be empty.
    let allowlist = Allowlist::parse(
        "// Retired in the winter update.\n{\"format\": \"tilewright.diff-allow/1\", \"remove\": [\n  {\"kind\": \"render_layer\", \"id\": \"effect\"} // unused\n]}",
    )
    .unwrap();
    assert_eq!(
        allowlist.allowances().collect::<Vec<_>>(),
        [Allowance::Remove {
            kind: Kind::RenderLayer,
            id: "effect".into()
        }]
    );
    assert!(
        Allowlist::parse(r#"{"format": "tilewright.diff-allow/1"}"#)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn allowlist_files_are_bounded_utf8() {
    let directory = tempfile::tempdir().unwrap();
    let missing = Allowlist::read(&directory.path().join("missing.jsonc")).unwrap_err();
    assert_eq!((missing.code, missing.line), ("source.io", 0));
    let latin1 = directory.path().join("latin1.jsonc");
    std::fs::write(&latin1, b"{\"format\": \"\xe9\"}").unwrap();
    assert_eq!(
        Allowlist::read(&latin1).unwrap_err().code,
        "source.encoding"
    );
    let large = directory.path().join("large.jsonc");
    std::fs::File::create(&large)
        .unwrap()
        .set_len(64 * 1024 * 1024 + 1)
        .unwrap();
    assert_eq!(
        Allowlist::read(&large).unwrap_err().code,
        "jsonc.input_too_large"
    );
    let file = directory.path().join("allow.jsonc");
    std::fs::write(
        &file,
        r#"{"format": "tilewright.diff-allow/1", "remove": [{"kind": "plane", "id": "vault"}]}"#,
    )
    .unwrap();
    assert_eq!(Allowlist::read(&file).unwrap().allowances().count(), 1);
}

#[test]
fn malformed_sides_do_not_load() {
    let directory = tempfile::tempdir().unwrap();
    match load(&directory.path().join("missing")) {
        Err(Error::Source(diagnostics)) => {
            assert_eq!(
                diagnostics[0].code, "source.missing_file",
                "{diagnostics:?}"
            );
        }
        other => panic!("{other:?}"),
    }
    // A file is read as a pack.
    let garbage = directory.path().join("garbage.twpack");
    std::fs::write(&garbage, b"not a pack").unwrap();
    assert!(matches!(load(&garbage), Err(Error::Pack(_))));
    let copy = Copy::new();
    let world = tilewright_source::load(&copy.root()).unwrap();
    let written = tilewright_pack::write_pack(&world, directory.path()).unwrap();
    let bytes = std::fs::read(&written.path).unwrap();
    let truncated = directory.path().join("truncated.twpack");
    std::fs::write(&truncated, &bytes[..bytes.len() - 4096]).unwrap();
    assert!(matches!(load(&truncated), Err(Error::Pack(_))));
    // A pack whose contents do not match its name fails verification.
    let mut flipped = bytes.clone();
    let last = flipped.len() - 1;
    flipped[last] ^= 1;
    std::fs::write(&written.path, flipped).unwrap();
    assert!(matches!(load(&written.path), Err(Error::Pack(_))));

    // A reference that no longer resolves makes the new world invalid.
    let dangling = Copy::new().remove("defs/proto/coin.jsonc");
    match load(&dangling.root()) {
        Err(Error::Source(diagnostics)) => {
            assert!(
                diagnostics
                    .iter()
                    .all(|diagnostic| diagnostic.code == "ref.unresolved"),
                "{diagnostics:?}"
            );
            assert!(!diagnostics.is_empty());
        }
        other => panic!("{other:?}"),
    }
}
