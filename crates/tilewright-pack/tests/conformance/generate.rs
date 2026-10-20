// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Generates the shared reader conformance fixtures: the pack compiled from
//! `fixtures/basic`, deliberately broken copies of it, and the manifest
//! that says how each must fail. Generation is deterministic: the fixture
//! source, the compiler and the bundled SQLite and zstd fix every byte.

use crate::json::J;
use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};
use std::{fmt::Write as _, path::Path};
use tilewright_pack::{
    ChunkData, ErrorCode, FieldRecord, MAXIMUM_RECORD_BYTES, Pack, PortalRecord, encode_chunk,
    write_pack,
};

/// The manifest's `format`.
pub const MANIFEST_FORMAT: &str = "tilewright.pack-conformance/1";

/// One single read and the code it fails with, or `None` when it succeeds.
pub struct Read {
    pub op: J,
    pub error: Option<ErrorCode>,
}

/// A broken pack and how readers must treat it.
pub struct Case {
    pub file: String,
    pub rule: String,
    pub open: Option<ErrorCode>,
    pub open_max_pack_bytes: Option<u64>,
    pub reads: Vec<Read>,
    pub verify: Option<ErrorCode>,
    pub verify_max_pack_bytes: Option<u64>,
    pub verify_max_decoded_bytes: Option<u64>,
}

impl Case {
    fn new(file: impl Into<String>, rule: impl Into<String>) -> Self {
        Self {
            file: file.into(),
            rule: rule.into(),
            open: None,
            open_max_pack_bytes: None,
            reads: Vec::new(),
            verify: None,
            verify_max_pack_bytes: None,
            verify_max_decoded_bytes: None,
        }
    }

    fn open(mut self, code: ErrorCode) -> Self {
        self.open = Some(code);
        self
    }

    fn read(mut self, op: J, error: Option<ErrorCode>) -> Self {
        self.reads.push(Read { op, error });
        self
    }

    fn verify(mut self, code: ErrorCode) -> Self {
        self.verify = Some(code);
        self
    }

    fn to_json(&self) -> J {
        let mut members = vec![
            ("file", J::from(self.file.as_str())),
            ("rule", J::from(self.rule.as_str())),
        ];
        if let Some(limit) = self.open_max_pack_bytes {
            members.push((
                "open_limits",
                J::object([("max_pack_bytes", J::from(limit))]),
            ));
        }
        if let Some(code) = self.open {
            members.push(("open", J::from(code.as_str())));
        }
        if !self.reads.is_empty() {
            let reads = self.reads.iter().map(|read| {
                let J::Object(mut op) = read.op.clone() else {
                    unreachable!("a read is an object")
                };
                op.push((
                    "error".into(),
                    read.error.map_or(J::Null, |code| J::from(code.as_str())),
                ));
                J::Object(op)
            });
            members.push(("reads", J::Array(reads.collect())));
        }
        let mut limits = Vec::new();
        if let Some(limit) = self.verify_max_pack_bytes {
            limits.push(("max_pack_bytes", J::from(limit)));
        }
        if let Some(limit) = self.verify_max_decoded_bytes {
            limits.push(("max_decoded_bytes", J::from(limit)));
        }
        if !limits.is_empty() {
            members.push(("verify_limits", J::object(limits)));
        }
        if let Some(code) = self.verify {
            members.push(("verify", J::from(code.as_str())));
        }
        J::object(members)
    }
}

fn chunk_op(plane: &str, layer: i32, cx: i64, cy: i64) -> J {
    J::object([
        ("op", J::from("chunk")),
        ("plane", J::from(plane)),
        ("layer", J::from(layer)),
        ("cx", J::from(cx)),
        ("cy", J::from(cy)),
    ])
}

fn string_op(id: u32) -> J {
    J::object([("op", J::from("string")), ("id", J::from(id))])
}

fn stairwell_op(plane: &str, id: &str) -> J {
    J::object([
        ("op", J::from("stairwell")),
        ("plane", J::from(plane)),
        ("id", J::from(id)),
    ])
}

fn step_op(plane: &str, layer: i32, x: i64, y: i64) -> J {
    J::object([
        ("op", J::from("stairwell_step")),
        ("plane", J::from(plane)),
        ("layer", J::from(layer)),
        ("x", J::from(x)),
        ("y", J::from(y)),
    ])
}

fn keyed_op(op: &str, first: (&'static str, &str), second: Option<(&'static str, &str)>) -> J {
    let mut members = vec![("op", J::from(op)), (first.0, J::from(first.1))];
    if let Some((key, value)) = second {
        members.push((key, J::from(value)));
    }
    J::object(members)
}

fn profile_op() -> J {
    J::object([("op", J::from("profile"))])
}

fn layer_height_op() -> J {
    J::object([("op", J::from("layer_height"))])
}

fn object_location_op(uid: &str) -> J {
    J::object([("op", J::from("object_location")), ("uid", J::from(uid))])
}

fn object_op(uid: &str) -> J {
    J::object([("op", J::from("object")), ("uid", J::from(uid))])
}

/// Runs SQL on a pack copy, then rebuilds it without free pages.
fn sql(path: &Path, sql: &str) {
    let connection = Connection::open(path).unwrap();
    connection.execute_batch(sql).unwrap();
    connection.execute_batch("VACUUM").unwrap();
}

/// Runs SQL on a pack copy and keeps its pages as they are.
fn sql_in_place(path: &Path, sql: &str) {
    Connection::open(path).unwrap().execute_batch(sql).unwrap();
}

/// Replaces (or adds) one chunk row's data.
fn set_chunk(path: &Path, plane: &str, layer: i32, cx: i64, cy: i64, data: &[u8]) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute(
            "INSERT OR REPLACE INTO chunks (plane, layer, cx, cy, encoding, data) VALUES (?1, ?2, ?3, ?4, 'twc1+zstd', ?5)",
            params![plane, layer, cx, cy, data],
        )
        .unwrap();
    connection.execute_batch("VACUUM").unwrap();
}

fn bytes(path: &Path, change: impl FnOnce(&mut Vec<u8>)) {
    let mut data = std::fs::read(path).unwrap();
    change(&mut data);
    std::fs::write(path, data).unwrap();
}

/// The pack under construction: the valid pack's path and lookups into it.
struct Base<'a> {
    valid: &'a Path,
    directory: &'a Path,
    cases: Vec<Case>,
}

impl Base<'_> {
    /// The ID of an interned string.
    fn id(&self, value: &str) -> u32 {
        Connection::open(self.valid)
            .unwrap()
            .query_row("SELECT id FROM strings WHERE value = ?1", [value], |row| {
                row.get(0)
            })
            .unwrap()
    }

    fn strings(&self) -> u32 {
        Connection::open(self.valid)
            .unwrap()
            .query_row("SELECT count(*) FROM strings", [], |row| row.get(0))
            .unwrap()
    }

    fn chunk(&self, plane: &str, layer: i32, cx: i64, cy: i64) -> ChunkData {
        Pack::open(self.valid)
            .unwrap()
            .chunk(plane, layer, cx, cy)
            .unwrap()
            .unwrap()
    }

    /// Adds a case whose file is a copy of the valid pack changed by
    /// `change`.
    fn add(&mut self, case: Case, change: impl FnOnce(&Path)) {
        let path = self.directory.join(&case.file);
        std::fs::copy(self.valid, &path).unwrap();
        change(&path);
        self.cases.push(case);
    }
}

/// Writes the fixtures into `directory` and returns the manifest text.
pub fn generate(directory: &Path, source: &Path) -> String {
    let world = tilewright_source::load(source).expect("the fixture is valid");
    let written = write_pack(&world, directory).unwrap();
    let valid_name = written
        .path
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let length = written.bytes;
    let mut base = Base {
        valid: &written.path,
        directory,
        cases: Vec::new(),
    };
    open_cases(&mut base, length);
    row_cases(&mut base);
    document_cases(&mut base);
    chunk_cases(&mut base);
    reference_cases(&mut base);
    uid_cases(&mut base);
    let valid = valid_entry(&written.path, &valid_name);
    let codes = ErrorCode::ALL
        .iter()
        .filter(|code| **code != ErrorCode::Invalid)
        .map(|code| J::from(code.as_str()))
        .collect();
    let manifest = J::object([
        ("format", J::from(MANIFEST_FORMAT)),
        ("codes", J::Array(codes)),
        ("valid", J::Array(vec![valid])),
        (
            "cases",
            J::Array(base.cases.iter().map(Case::to_json).collect()),
        ),
    ]);
    manifest.pretty()
}

#[allow(clippy::too_many_lines)]
fn open_cases(base: &mut Base<'_>, length: u64) {
    base.add(
        Case::new(
            "open-not-a-database.twpack",
            "a file of one page that is not an SQLite database",
        )
        .open(ErrorCode::Sqlite),
        |path| std::fs::write(path, b"not a pack\n".repeat(373)).unwrap(),
    );
    base.add(
        Case::new(
            "open-empty.twpack",
            "an empty file, which SQLite opens as a database without tables",
        )
        .open(ErrorCode::Schema),
        |path| std::fs::write(path, b"").unwrap(),
    );
    for (name, rule, change) in [
        (
            "open-schema-view",
            "the schema has a view beyond the format's tables",
            "CREATE VIEW extra AS SELECT 1",
        ),
        (
            "open-schema-index",
            "the schema has an index beyond SQLite's automatic ones",
            "CREATE INDEX extra ON definitions (document)",
        ),
        (
            "open-schema-column",
            "a table has a column the format does not",
            "ALTER TABLE definitions ADD COLUMN extra TEXT",
        ),
        (
            "open-schema-many-tables",
            "the schema has more tables than the format",
            &(0..20).fold(String::new(), |mut sql, index| {
                let _ = write!(sql, "CREATE TABLE extra{index} (x);");
                sql
            }),
        ),
        (
            "open-schema-long",
            "a table's SQL is longer than any the format has",
            &format!("CREATE TABLE extra ({} INTEGER)", "x".repeat(2000)),
        ),
        (
            "open-application-id",
            "the application ID is not TWP1",
            "PRAGMA application_id = 1",
        ),
        (
            "open-user-version",
            "the user version is 1, from before object uids, not 2",
            "PRAGMA user_version = 1",
        ),
        (
            "open-page-size",
            "the pages are 8192 bytes",
            "PRAGMA page_size = 8192",
        ),
    ] {
        let code = if name.starts_with("open-schema") {
            ErrorCode::Schema
        } else {
            ErrorCode::Identity
        };
        let change = change.to_owned();
        base.add(
            Case::new(format!("{name}.twpack"), rule).open(code),
            |path| sql(path, &change),
        );
    }
    base.add(
        Case::new(
            "open-free-pages.twpack",
            "pages freed by a dropped table are kept as free pages",
        )
        .open(ErrorCode::FreePages),
        |path| {
            sql_in_place(
                path,
                "CREATE TABLE extra (x); INSERT INTO extra VALUES (zeroblob(20000)); DROP TABLE extra",
            );
        },
    );
    base.add(
        Case::new(
            "verify-hidden-free-pages.twpack",
            "free pages that the header no longer lists, which the integrity check finds unused",
        )
        .verify(ErrorCode::Integrity),
        |path| {
            sql_in_place(
                path,
                "CREATE TABLE extra (x); INSERT INTO extra VALUES (zeroblob(20000)); DROP TABLE extra",
            );
            // The first freelist trunk page and the free page count.
            bytes(path, |data| data[32..40].fill(0));
        },
    );
    for (name, rule, change) in [
        (
            "missing-key",
            "meta lacks the compiler key",
            "DELETE FROM meta WHERE key = 'compiler'",
        ),
        (
            "extra-key",
            "meta has a key beyond the format's",
            "INSERT INTO meta VALUES ('zzz', 'x')",
        ),
        (
            "format",
            "meta names another pack format",
            "UPDATE meta SET value = 'tilewright.pack/2' WHERE key = 'format'",
        ),
        (
            "encoding",
            "meta names another chunk encoding",
            "UPDATE meta SET value = 'twc2+zstd' WHERE key = 'chunk_encoding'",
        ),
        (
            "digest",
            "the source digest is not lowercase hex",
            "UPDATE meta SET value = upper(value) WHERE key = 'source_digest'",
        ),
        (
            "blob",
            "a meta value is a blob",
            "UPDATE meta SET value = CAST(value AS BLOB) WHERE key = 'profile_id'",
        ),
        (
            "long",
            "a meta value other than profile is longer than 1 KiB",
            "UPDATE meta SET value = value || replace(hex(zeroblob(1024)), '00', 'x') WHERE key = 'compiler'",
        ),
        (
            "utf8",
            "a meta value is text that is not UTF-8",
            "UPDATE meta SET value = CAST(x'ff' AS TEXT) WHERE key = 'compiler'",
        ),
    ] {
        base.add(
            Case::new(format!("open-meta-{name}.twpack"), rule).open(ErrorCode::Meta),
            |path| sql(path, change),
        );
    }
    base.add(
        Case::new(
            "open-trailing-byte.twpack",
            "one byte follows the last page",
        )
        .open(ErrorCode::FileLength),
        |path| bytes(path, |data| data.push(0)),
    );
    base.add(
        Case::new(
            "open-trailing-page.twpack",
            "a page of zeros, as a sparse hole reads, follows the last page",
        )
        .open(ErrorCode::FileLength),
        |path| bytes(path, |data| data.extend_from_slice(&[0; 4096])),
    );
    base.add(
        Case::new(
            "open-truncated.twpack",
            "the file ends one page before the database does, inside a table opening reads",
        )
        .open(ErrorCode::Sqlite),
        |path| bytes(path, |data| data.truncate(data.len() - 4096)),
    );
    let mut too_large = Case::new(
        "open-too-large.twpack",
        "the file is one byte longer than the pack size limit",
    )
    .open(ErrorCode::TooLarge);
    too_large.open_max_pack_bytes = Some(length - 1);
    base.add(too_large, |_| {});
    let mut verify_too_large = Case::new(
        "verify-too-large.twpack",
        "verification's pack size limit is one byte less than the file",
    )
    .verify(ErrorCode::TooLarge);
    verify_too_large.verify_max_pack_bytes = Some(length - 1);
    base.add(verify_too_large, |_| {});
    let mut budget = Case::new(
        "verify-decode-budget.twpack",
        "the chunks decode to more than the verification budget",
    )
    .verify(ErrorCode::DecodeBudget);
    budget.verify_max_decoded_bytes = Some(1);
    base.add(budget, |_| {});
    base.add(
        Case::new(
            format!("{}.twpack", "0".repeat(64)),
            "the file is named by a digest that is not its own",
        )
        .verify(ErrorCode::Digest),
        |_| {},
    );
    base.add(
        Case::new(
            "verify-integrity.twpack",
            "a key in the strings value index differs from its row",
        )
        .read(string_op(base.id("cave_floor")), None)
        .verify(ErrorCode::Integrity),
        corrupt_string_index,
    );
}

/// Changes a byte of a key in the unique index of `strings.value`, so the
/// index and its table disagree but every page still parses.
fn corrupt_string_index(path: &Path) {
    let page: i64 = Connection::open(path)
        .unwrap()
        .query_row(
            "SELECT rootpage FROM sqlite_schema WHERE name = 'sqlite_autoindex_strings_1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    bytes(path, |data| {
        let start = usize::try_from((page - 1) * 4096).unwrap();
        // The index has one leaf page for the fixture's few strings.
        let contents = &mut data[start..start + 4096];
        let offset = contents
            .windows(10)
            .position(|window| window == b"cave_floor")
            .unwrap();
        contents[offset + 9] = b'x';
    });
}

#[allow(clippy::too_many_lines)]
fn row_cases(base: &mut Base<'_>) {
    let first = 1;
    let last = base.strings();
    for (name, rule, id, change, read, code) in [
        (
            "string-blob",
            "a string is a blob",
            first,
            format!("UPDATE strings SET value = CAST(value AS BLOB) WHERE id = {first}"),
            Some(ErrorCode::String),
            ErrorCode::String,
        ),
        (
            "string-long",
            "a string is longer than 1 MiB",
            last,
            format!(
                "UPDATE strings SET value = value || replace(hex(zeroblob(1048576)), '00', 'x') WHERE id = {last}"
            ),
            Some(ErrorCode::String),
            ErrorCode::String,
        ),
        (
            "string-utf8",
            "a string is text that is not UTF-8",
            last,
            format!("UPDATE strings SET value = CAST(value || x'ff' AS TEXT) WHERE id = {last}"),
            Some(ErrorCode::String),
            ErrorCode::String,
        ),
        (
            "string-nul",
            "a string holds NUL, which only verification checks",
            last,
            format!("UPDATE strings SET value = value || char(0) WHERE id = {last}"),
            None,
            ErrorCode::String,
        ),
    ] {
        base.add(
            Case::new(format!("{name}.twpack"), rule)
                .read(string_op(id), read)
                .verify(code),
            |path| sql(path, &change),
        );
    }
    let order = format!("UPDATE strings SET value = 'a' || value WHERE id = {last}");
    for (name, rule, change) in [
        (
            "string-ids-order",
            "the last string sorts before the one before it",
            order.as_str(),
        ),
        (
            "string-ids-gap",
            "string IDs skip a number",
            &format!("UPDATE strings SET id = {} WHERE id = {last}", last + 1),
        ),
    ] {
        base.add(
            Case::new(format!("{name}.twpack"), rule).verify(ErrorCode::StringIds),
            |path| sql(path, change),
        );
    }
    for (name, rule, change, op, read, code) in [
        (
            "plane-kind",
            "a plane's kind is not one the format has",
            "UPDATE planes SET kind = 'shared-ish' WHERE id = 'vault'",
            keyed_op("plane", ("id", "vault"), None),
            Some(ErrorCode::Plane),
            ErrorCode::Plane,
        ),
        (
            "plane-id",
            "a plane's ID is not an identifier",
            "UPDATE planes SET id = 'Vault', document = replace(document, '\"id\":\"vault\"', '\"id\":\"Vault\"') WHERE id = 'vault'",
            keyed_op("plane", ("id", "Vault"), None),
            Some(ErrorCode::Plane),
            ErrorCode::Plane,
        ),
        (
            "plane-chunk-size",
            "a plane's chunks are not 32 tiles square",
            "DELETE FROM chunks WHERE plane = 'vault'; UPDATE planes SET chunk_size = 16, document = replace(document, '\"chunk_size\":32', '\"chunk_size\":16') WHERE id = 'vault'",
            keyed_op("plane", ("id", "vault"), None),
            Some(ErrorCode::Plane),
            ErrorCode::Plane,
        ),
        (
            "zone-parent",
            "a zone's parent is not a zone of its plane",
            "UPDATE zones SET parent = 'nowhere', document = replace(document, '\"parent\":\"island\"', '\"parent\":\"nowhere\"') WHERE id = 'village'",
            keyed_op("zone", ("plane", "overworld"), Some(("id", "village"))),
            None,
            ErrorCode::Zone,
        ),
        (
            "zone-id",
            "a zone's ID is not an identifier",
            "UPDATE zones SET id = 'a b' WHERE id = 'village'",
            keyed_op("zone", ("plane", "overworld"), Some(("id", "a b"))),
            Some(ErrorCode::Zone),
            ErrorCode::Zone,
        ),
        (
            "zone-cycle",
            "zone parents form a cycle",
            "UPDATE zones SET parent = 'village', document = replace(document, '\"parent\":\"wilderness\"', '\"parent\":\"village\"') WHERE id = 'island'",
            keyed_op("zone", ("plane", "overworld"), Some(("id", "island"))),
            None,
            ErrorCode::Zone,
        ),
        (
            "zone-plane",
            "a zone names a plane the pack does not have",
            "UPDATE zones SET plane = 'attic' WHERE id = 'village'",
            keyed_op("zone", ("plane", "attic"), Some(("id", "village"))),
            None,
            ErrorCode::Zone,
        ),
        (
            "anchor-plane",
            "an anchor names a plane the pack does not have",
            "UPDATE anchors SET plane = 'attic' WHERE id = 'village.well'",
            keyed_op("anchor", ("plane", "attic"), Some(("id", "village.well"))),
            None,
            ErrorCode::Anchor,
        ),
        (
            "anchor-x",
            "an anchor's x is text",
            "UPDATE anchors SET x = 'east' WHERE id = 'village.well'",
            keyed_op(
                "anchor",
                ("plane", "overworld"),
                Some(("id", "village.well")),
            ),
            Some(ErrorCode::Anchor),
            ErrorCode::Anchor,
        ),
        (
            "anchor-range",
            "an anchor's y is past the source format's integer range",
            "UPDATE anchors SET y = 9007199254740992 WHERE id = 'village.well'",
            keyed_op(
                "anchor",
                ("plane", "overworld"),
                Some(("id", "village.well")),
            ),
            Some(ErrorCode::Anchor),
            ErrorCode::Anchor,
        ),
        (
            "anchor-layer",
            "an anchor's layer is past 32 bits",
            "UPDATE anchors SET layer = 2147483648 WHERE id = 'village.well'",
            keyed_op(
                "anchor",
                ("plane", "overworld"),
                Some(("id", "village.well")),
            ),
            Some(ErrorCode::Anchor),
            ErrorCode::Anchor,
        ),
        (
            "anchor-id",
            "an anchor's ID is not an identifier",
            "UPDATE anchors SET id = 'Village.Well' WHERE id = 'village.well'",
            keyed_op(
                "anchor",
                ("plane", "overworld"),
                Some(("id", "Village.Well")),
            ),
            Some(ErrorCode::Anchor),
            ErrorCode::Anchor,
        ),
        (
            "anchor-id-nul",
            "an anchor's ID holds NUL",
            "UPDATE anchors SET id = 'village' || char(0) || '.well' WHERE id = 'village.well'",
            keyed_op(
                "anchor",
                ("plane", "overworld"),
                Some(("id", "cave.entrance")),
            ),
            None,
            ErrorCode::Anchor,
        ),
        (
            "definition-id",
            "a definition's ID is not an identifier",
            "UPDATE definitions SET id = 'Chest_Common', document = replace(document, '\"id\":\"chest_common\"', '\"id\":\"Chest_Common\"') WHERE id = 'chest_common'",
            keyed_op("definition", ("type", "loot"), Some(("id", "Chest_Common"))),
            Some(ErrorCode::Definition),
            ErrorCode::Definition,
        ),
        (
            "chunk-layer",
            "a chunk's layer is text",
            "UPDATE chunks SET layer = 'bad' WHERE plane = 'vault'",
            chunk_op("vault", 0, 0, 0),
            None,
            ErrorCode::Chunk,
        ),
        (
            "chunk-position",
            "a chunk's position puts tiles past the source format's integer range",
            "UPDATE chunks SET cx = 281474976710656 WHERE plane = 'vault'",
            chunk_op("vault", 0, 281_474_976_710_656, 0),
            None,
            ErrorCode::Chunk,
        ),
        (
            "chunk-plane",
            "a chunk names a plane the pack does not have",
            "UPDATE chunks SET plane = 'attic' WHERE plane = 'vault'",
            chunk_op("attic", 0, 0, 0),
            None,
            ErrorCode::Chunk,
        ),
    ] {
        base.add(
            Case::new(format!("{name}.twpack"), rule)
                .read(op, read)
                .verify(code),
            |path| sql(path, change),
        );
    }
    stairwell_row_cases(base);
}

#[allow(clippy::too_many_lines)]
fn stairwell_row_cases(base: &mut Base<'_>) {
    let tower = stairwell_op("overworld", "tower.stairs");
    let step = step_op("overworld", 0, 20, 20);
    for (name, rule, change, op, read, code) in [
        (
            "stairwell-plane",
            "a stairwell names a plane the pack does not have",
            "UPDATE stairwells SET plane = 'attic' WHERE id = 'cave.chute'",
            stairwell_op("attic", "cave.chute"),
            None,
            ErrorCode::Stairwell,
        ),
        (
            "stairwell-row-id",
            "a stairwell's row ID is not its document's",
            "UPDATE stairwells SET id = 'tower.steps' WHERE id = 'tower.stairs'; \
             UPDATE stairwell_steps SET stairwell = 'tower.steps' WHERE stairwell = 'tower.stairs'",
            stairwell_op("overworld", "tower.steps"),
            Some(ErrorCode::Stairwell),
            ErrorCode::Stairwell,
        ),
        (
            "stairwell-step-proto-row",
            "a step's row names another prototype than its stairwell's document",
            "UPDATE stairwell_steps SET proto = 'tree' WHERE layer = 0 AND x = 20 AND y = 20",
            step.clone(),
            None,
            ErrorCode::Stairwell,
        ),
        (
            "stairwell-step-missing",
            "a step of a stairwell's document has no row",
            "DELETE FROM stairwell_steps WHERE layer = 0 AND x = 20 AND y = 20",
            tower.clone(),
            None,
            ErrorCode::Stairwell,
        ),
        (
            "stairwell-step-extra",
            "stairwell_steps holds a row no stairwell lists",
            "INSERT INTO stairwell_steps VALUES ('overworld', 5, 20, 20, 'tower.stairs', 3, 'stairs_up')",
            step_op("overworld", 5, 20, 20),
            None,
            ErrorCode::StairwellStep,
        ),
        (
            "stairwell-step-proto",
            "a step's prototype is not an identifier",
            "UPDATE stairwell_steps SET proto = 'Stairs Up' WHERE layer = 0 AND x = 20 AND y = 20",
            step.clone(),
            Some(ErrorCode::StairwellStep),
            ErrorCode::StairwellStep,
        ),
        (
            "stairwell-step-index",
            "a step's index is negative",
            "UPDATE stairwell_steps SET step = -1 WHERE layer = 0 AND x = 20 AND y = 20",
            step.clone(),
            Some(ErrorCode::StairwellStep),
            ErrorCode::StairwellStep,
        ),
        (
            "stairwell-step-stairwell",
            "a step names a stairwell its plane does not have",
            "UPDATE stairwell_steps SET stairwell = 'nowhere' WHERE layer = 0 AND x = 20 AND y = 20",
            step.clone(),
            None,
            ErrorCode::StairwellStep,
        ),
        (
            "stairwell-step-unknown-proto",
            "a step's prototype is not a proto definition",
            "UPDATE stairwell_steps SET proto = 'nothing' WHERE layer = 0 AND x = 20 AND y = 20; \
             UPDATE stairwells SET document = replace(document, '{\"layer\":0,\"at\":[20,20],\"proto\":\"stairs_up\"}', '{\"layer\":0,\"at\":[20,20],\"proto\":\"nothing\"}') WHERE id = 'tower.stairs'",
            tower.clone(),
            None,
            ErrorCode::StairwellStep,
        ),
    ] {
        base.add(
            Case::new(format!("{name}.twpack"), rule)
                .read(op, read)
                .verify(code),
            |path| sql(path, change),
        );
    }
}

/// A document case: its name, rule, SQL, single read, the read's error and
/// verification's.
type DocumentCase = (
    &'static str,
    &'static str,
    String,
    J,
    Option<ErrorCode>,
    ErrorCode,
);

#[allow(clippy::too_many_lines)]
fn document_cases(base: &mut Base<'_>) {
    let replace = |table: &str, from: &str, to: &str, filter: &str| {
        format!(
            "UPDATE {table} SET document = replace(document, '{}', '{}') WHERE {filter}",
            from.replace('\'', "''"),
            to.replace('\'', "''")
        )
    };
    let profile = |from: &str, to: &str| {
        format!(
            "UPDATE meta SET value = replace(value, '{}', '{}') WHERE key = 'profile'",
            from.replace('\'', "''"),
            to.replace('\'', "''")
        )
    };
    let vault = keyed_op("plane", ("id", "vault"), None);
    let overworld = keyed_op("plane", ("id", "overworld"), None);
    let village = keyed_op("zone", ("plane", "overworld"), Some(("id", "village")));
    let coin = keyed_op("definition", ("type", "proto"), Some(("id", "coin")));
    let tower = stairwell_op("overworld", "tower.stairs");
    let chute = stairwell_op("overworld", "cave.chute");
    let cases: Vec<DocumentCase> = vec![
        (
            "plane-document-json",
            "a plane document is not JSON",
            "UPDATE planes SET document = 'not-json' WHERE id = 'vault'".into(),
            vault.clone(),
            Some(ErrorCode::Plane),
            ErrorCode::Plane,
        ),
        (
            "plane-document-kind",
            "a plane document's kind is not its row's",
            replace("planes", "\"instanced\"", "\"shared\"", "id = 'vault'"),
            vault.clone(),
            Some(ErrorCode::Plane),
            ErrorCode::Plane,
        ),
        (
            "plane-document-space",
            "a plane document is not canonical: it has a space",
            replace("planes", "\"kind\":", "\"kind\": ", "id = 'vault'"),
            vault.clone(),
            Some(ErrorCode::Plane),
            ErrorCode::Plane,
        ),
        (
            "plane-default-zone",
            "a plane's default zone is not one of its zones",
            replace(
                "planes",
                "\"default_zone\":\"wilderness\"",
                "\"default_zone\":\"nowhere\"",
                "id = 'overworld'",
            ),
            overworld,
            None,
            ErrorCode::Plane,
        ),
        (
            "plane-document-nul",
            "a plane document holds NUL after its JSON",
            "UPDATE planes SET document = document || char(0) WHERE id = 'vault'".into(),
            vault.clone(),
            Some(ErrorCode::Plane),
            ErrorCode::Plane,
        ),
        (
            "plane-document-utf8",
            "a plane document is text that is not UTF-8",
            "UPDATE planes SET document = CAST(x'7b22ff227d' AS TEXT) WHERE id = 'vault'".into(),
            vault,
            Some(ErrorCode::Plane),
            ErrorCode::Plane,
        ),
        (
            "zone-document-id",
            "a zone document's ID is not its row's",
            "UPDATE zones SET document = '{\"id\":\"elsewhere\"}' WHERE id = 'village'".into(),
            village.clone(),
            Some(ErrorCode::Zone),
            ErrorCode::Zone,
        ),
        (
            "zone-document-parent",
            "a zone document has no parent but its row has one",
            replace("zones", ",\"parent\":\"island\"", "", "id = 'village'"),
            village.clone(),
            Some(ErrorCode::Zone),
            ErrorCode::Zone,
        ),
        (
            "zone-document-custom",
            "a zone document's custom data has a key that is not a namespace",
            replace(
                "zones",
                "{\"id\":\"village\"",
                "{\"custom\":{\"notes\":1},\"id\":\"village\"",
                "id = 'village'",
            ),
            village,
            Some(ErrorCode::Zone),
            ErrorCode::Zone,
        ),
        (
            "definition-document-array",
            "a definition document is an array",
            "UPDATE definitions SET document = '[]' WHERE id = 'coin'".into(),
            coin.clone(),
            Some(ErrorCode::Definition),
            ErrorCode::Definition,
        ),
        (
            "definition-document-type",
            "a definition document's type is not its row's",
            replace(
                "definitions",
                "\"type\":\"proto\"",
                "\"type\":\"loot\"",
                "id = 'coin'",
            ),
            coin.clone(),
            Some(ErrorCode::Definition),
            ErrorCode::Definition,
        ),
        (
            "definition-document-escape",
            "a definition document escapes a character JSON does not require",
            replace(
                "definitions",
                "\"id\":\"coin\"",
                "\"id\":\"co\\u0069n\"",
                "id = 'coin'",
            ),
            coin.clone(),
            Some(ErrorCode::Definition),
            ErrorCode::Definition,
        ),
        (
            "definition-document-duplicate",
            "a definition document repeats a key",
            replace(
                "definitions",
                "\"id\":\"coin\"",
                "\"id\":\"coin\",\"id\":\"coin\"",
                "id = 'coin'",
            ),
            coin.clone(),
            Some(ErrorCode::Definition),
            ErrorCode::Definition,
        ),
        (
            "definition-document-integer",
            "a definition document has an integer past the source format's range",
            replace(
                "definitions",
                "{\"format\"",
                "{\"big\":9007199254740992,\"format\"",
                "id = 'coin'",
            ),
            coin,
            Some(ErrorCode::Definition),
            ErrorCode::Definition,
        ),
        (
            "stairwell-document-steps",
            "a stairwell's steps are on the same layer",
            replace(
                "stairwells",
                "{\"layer\":-1,\"at\":[20,20]",
                "{\"layer\":0,\"at\":[20,20]",
                "id = 'tower.stairs'",
            ),
            tower.clone(),
            Some(ErrorCode::Stairwell),
            ErrorCode::Stairwell,
        ),
        (
            "stairwell-document-key",
            "a stairwell document has a key the source format does not",
            replace(
                "stairwells",
                "{\"id\":\"tower.stairs\"",
                "{\"id\":\"tower.stairs\",\"extra\":1",
                "id = 'tower.stairs'",
            ),
            tower.clone(),
            Some(ErrorCode::Stairwell),
            ErrorCode::Stairwell,
        ),
        (
            "stairwell-document-direction",
            "a stairwell's direction is not one the format has",
            replace(
                "stairwells",
                "\"direction\":\"down\"",
                "\"direction\":\"sideways\"",
                "id = 'cave.chute'",
            ),
            chute.clone(),
            Some(ErrorCode::Stairwell),
            ErrorCode::Stairwell,
        ),
        (
            "stairwell-document-custom",
            "a stairwell's custom data is not an object",
            replace(
                "stairwells",
                "\"custom\":{\"example.org/notes\":{\"reviewed\":true}}",
                "\"custom\":[]",
                "id = 'cave.chute'",
            ),
            chute,
            Some(ErrorCode::Stairwell),
            ErrorCode::Stairwell,
        ),
        (
            "stairwell-document-space",
            "a stairwell document is not canonical: it has a space",
            replace(
                "stairwells",
                "\"steps\":",
                "\"steps\": ",
                "id = 'tower.stairs'",
            ),
            tower,
            Some(ErrorCode::Stairwell),
            ErrorCode::Stairwell,
        ),
        (
            "profile-space",
            "the profile is not canonical: it starts with a space",
            "UPDATE meta SET value = ' ' || value WHERE key = 'profile'".into(),
            profile_op(),
            Some(ErrorCode::Profile),
            ErrorCode::Profile,
        ),
        (
            "profile-id",
            "profile_id is not the profile's ID",
            "UPDATE meta SET value = 'other' WHERE key = 'profile_id'".into(),
            profile_op(),
            Some(ErrorCode::Profile),
            ErrorCode::Profile,
        ),
        (
            "profile-version",
            "the profile's version is longer than 64 characters",
            format!(
                "{}; UPDATE meta SET value = '{long}' WHERE key = 'profile_version'",
                profile(
                    "\"version\":\"1.0.0\"",
                    &format!("\"version\":\"{long}\"", long = "1".repeat(65))
                ),
                long = "1".repeat(65)
            ),
            profile_op(),
            Some(ErrorCode::Profile),
            ErrorCode::Profile,
        ),
        (
            "profile-layer-height",
            "the profile's layer_height is 0",
            profile("\"layer_height\":50", "\"layer_height\":0"),
            layer_height_op(),
            Some(ErrorCode::Profile),
            ErrorCode::Profile,
        ),
        (
            "profile-layer-height-float",
            "the profile's layer_height is not an integer",
            profile("\"layer_height\":50", "\"layer_height\":50.0"),
            layer_height_op(),
            Some(ErrorCode::Profile),
            ErrorCode::Profile,
        ),
        (
            "profile-projection",
            "the profile's projection has a key its kind does not",
            profile("\"tile_height\":24}", "\"tile_height\":24,\"tile_size\":1}"),
            profile_op(),
            Some(ErrorCode::Profile),
            ErrorCode::Profile,
        ),
        (
            "profile-render-duplicate",
            "the profile lists a render layer twice",
            profile("\"living\"", "\"mask\""),
            profile_op(),
            Some(ErrorCode::Profile),
            ErrorCode::Profile,
        ),
        (
            "profile-ground-unknown",
            "a ground layer is not a render layer",
            profile(
                "\"ground_layers\":[\"floor\",\"mask\"]",
                "\"ground_layers\":[\"floor\",\"roof\"]",
            ),
            profile_op(),
            Some(ErrorCode::Profile),
            ErrorCode::Profile,
        ),
        (
            "profile-ground-many",
            "the profile lists more than 8 ground layers",
            profile(
                "\"ground_layers\":[\"floor\",\"mask\"]",
                "\"ground_layers\":[\"floor\",\"mask\",\"a\",\"b\",\"c\",\"d\",\"e\",\"f\",\"g\"]",
            ),
            profile_op(),
            Some(ErrorCode::Profile),
            ErrorCode::Profile,
        ),
    ];
    for (name, rule, change, op, read, code) in cases {
        base.add(
            Case::new(format!("{name}.twpack"), rule)
                .read(op, read)
                .verify(code),
            |path| sql(path, &change),
        );
    }
}

/// A zstd frame of `record` at the compiler's level, with its content size
/// recorded and, when `checksum`, a checksum.
fn frame(record: &[u8], content_size: bool, checksum: bool) -> Vec<u8> {
    let mut compressor = zstd::bulk::Compressor::new(19).unwrap();
    compressor.include_contentsize(content_size).unwrap();
    compressor.include_checksum(checksum).unwrap();
    compressor.compress(record).unwrap()
}

/// A raw object record: the fixed part, then `rest`.
fn object(tile: u16, proto: u32, flags: u8, rest: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    output.extend_from_slice(&tile.to_le_bytes());
    output.extend_from_slice(&proto.to_le_bytes());
    output.extend_from_slice(&0_u16.to_le_bytes());
    output.extend_from_slice(&0_i16.to_le_bytes());
    output.extend_from_slice(&0_i32.to_le_bytes());
    output.push(flags);
    output.extend_from_slice(rest);
    output
}

/// The parts of a raw record, each replaceable on its own.
#[derive(Clone)]
struct Raw {
    header: Vec<u8>,
    palette: Vec<u8>,
    grids: Vec<u8>,
    objects: Vec<u8>,
    trailing: Vec<u8>,
}

impl Raw {
    /// A 32 × 32 record of two ground layers, every tile of the first
    /// `floor`, with the given objects.
    fn new(floor: u32, objects: &[Vec<u8>]) -> Self {
        let cells = 32 * 32;
        let mut header = b"TWC1".to_vec();
        header.extend_from_slice(&2_u16.to_le_bytes());
        header.extend_from_slice(&32_u16.to_le_bytes());
        header.extend_from_slice(&[2, 0]);
        header.extend_from_slice(&1_u32.to_le_bytes());
        let mut grids = Vec::new();
        grids.extend(std::iter::repeat_n(1_u16.to_le_bytes(), cells).flatten());
        grids.extend(std::iter::repeat_n([0_u8; 2], cells).flatten());
        grids.extend(std::iter::repeat_n([0_u8; 4], cells).flatten());
        grids.extend(std::iter::repeat_n([0_u8; 2], cells).flatten());
        let mut list = u32::try_from(objects.len()).unwrap().to_le_bytes().to_vec();
        for object in objects {
            list.extend_from_slice(object);
        }
        Self {
            header,
            palette: floor.to_le_bytes().to_vec(),
            grids,
            objects: list,
            trailing: Vec::new(),
        }
    }

    fn bytes(&self) -> Vec<u8> {
        [
            &self.header[..],
            &self.palette,
            &self.grids,
            &self.objects,
            &self.trailing,
        ]
        .concat()
    }
}

/// A field block of one field: count 1, then the name and the tagged value.
fn field(name: u32, tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut output = 1_u16.to_le_bytes().to_vec();
    output.extend_from_slice(&name.to_le_bytes());
    output.push(tag);
    output.extend_from_slice(payload);
    output
}

/// A uid as a record holds it: its length byte, then its bytes.
fn uid_bytes(value: &[u8]) -> Vec<u8> {
    let mut output = vec![u8::try_from(value.len()).unwrap()];
    output.extend_from_slice(value);
    output
}

fn text(value: &[u8]) -> Vec<u8> {
    let mut output = u32::try_from(value.len()).unwrap().to_le_bytes().to_vec();
    output.extend_from_slice(value);
    output
}

#[allow(clippy::too_many_lines)]
fn chunk_cases(base: &mut Base<'_>) {
    for (name, rule, sql_text, code) in [
        (
            "chunk-encoding",
            "a chunk's encoding is not twc1+zstd",
            "UPDATE chunks SET encoding = 'twc2+zstd' WHERE plane = 'vault'",
            ErrorCode::ChunkEncoding,
        ),
        (
            "chunk-data-text",
            "a chunk's data is text",
            "UPDATE chunks SET data = 'twc1' WHERE plane = 'vault'",
            ErrorCode::ChunkData,
        ),
    ] {
        base.add(
            Case::new(format!("{name}.twpack"), rule)
                .read(chunk_op("vault", 0, 0, 0), Some(code))
                .verify(code),
            |path| sql(path, sql_text),
        );
    }
    let floor = base.id("cave_floor");
    let chest = base.id("chest");
    let name = base.id("name");
    let control = control_object(base);
    let valid = Raw::new(floor, std::slice::from_ref(&control));
    let mut rows: Vec<(&str, Vec<u8>, ErrorCode)> = Vec::new();
    let mut add =
        |rule: &'static str, data: Vec<u8>| rows.push((rule, data, ErrorCode::ChunkRecord));
    let record = valid.bytes();
    add("the data is the record, not compressed", record.clone());
    add(
        "the data is two zstd frames",
        [frame(&record, true, false), frame(&record, true, false)].concat(),
    );
    add(
        "the frame does not record its content size",
        frame(&record, false, false),
    );
    add(
        "one byte follows the frame",
        [frame(&record, true, false), vec![0]].concat(),
    );
    add("the frame ends one byte early", {
        let mut data = frame(&record, true, false);
        data.pop();
        data
    });
    add("the frame's checksum does not match", {
        let mut data = frame(&record, true, true);
        let last = data.len() - 1;
        data[last] ^= 1;
        data
    });
    add("the frame header sets its reserved bit", {
        let mut data = frame(&record, true, false);
        data[4] |= 0x08;
        data
    });
    add("the data is a skippable frame", {
        let mut data = 0x184D_2A50_u32.to_le_bytes().to_vec();
        data.extend_from_slice(&u32::try_from(record.len()).unwrap().to_le_bytes());
        data.extend_from_slice(&record);
        data
    });
    add(
        "the frame records more than 16 MiB",
        frame(&vec![0; MAXIMUM_RECORD_BYTES + 1], true, false),
    );
    let mut raw = |rule: &'static str, change: &dyn Fn(&mut Raw)| {
        let mut changed = valid.clone();
        change(&mut changed);
        rows.push((
            rule,
            frame(&changed.bytes(), true, false),
            ErrorCode::ChunkRecord,
        ));
    };
    raw("the magic is not TWC1", &|raw| {
        raw.header[0] = b'X';
    });
    raw("the version is 1, from before object uids", &|raw| {
        raw.header[4] = 1;
    });
    raw("the version is 3", &|raw| {
        raw.header[4] = 3;
    });
    raw("the chunk size is 0", &|raw| {
        raw.header[6..8].copy_from_slice(&0_u16.to_le_bytes());
    });
    raw("the chunk size is 256", &|raw| {
        raw.header[6..8].copy_from_slice(&256_u16.to_le_bytes());
    });
    raw("the ground count is 9", &|raw| {
        raw.header[8] = 9;
    });
    raw("the reserved byte is 1", &|raw| {
        raw.header[9] = 1;
    });
    raw("the palette length passes the record", &|raw| {
        raw.header[10..14].copy_from_slice(&u32::MAX.to_le_bytes());
    });
    raw("the palette holds 0", &|raw| {
        raw.palette = 0_u32.to_le_bytes().to_vec();
    });
    raw("the palette descends", &|raw| {
        raw.header[10..14].copy_from_slice(&2_u32.to_le_bytes());
        raw.palette = [floor + 1, floor]
            .iter()
            .flat_map(|id| id.to_le_bytes())
            .collect();
    });
    raw("a ground cell is outside the palette", &|raw| {
        raw.grids[0] = 2;
    });
    raw("a zone cell is outside the palette", &|raw| {
        raw.grids[32 * 32 * 8] = 2;
    });
    raw("the record ends inside the grids", &|raw| {
        raw.grids.truncate(100);
        raw.objects.clear();
    });
    raw("the object count passes the record", &|raw| {
        raw.objects[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
    });
    raw("an object's tile is past the chunk", &|raw| {
        raw.objects = [1_u32.to_le_bytes().to_vec(), object(1024, chest, 0, &[])].concat();
    });
    raw("a top-level object has the inventory tile", &|raw| {
        raw.objects = [1_u32.to_le_bytes().to_vec(), object(0xFFFF, chest, 0, &[])].concat();
    });
    raw("an inventory entry has a tile", &|raw| {
        let mut inventory = 1_u32.to_le_bytes().to_vec();
        inventory.extend_from_slice(&object(0, chest, 0, &[]));
        raw.objects = [
            1_u32.to_le_bytes().to_vec(),
            object(0, chest, 8, &inventory),
        ]
        .concat();
    });
    raw("an object has an unknown flag", &|raw| {
        raw.objects = [1_u32.to_le_bytes().to_vec(), object(0, chest, 32, &[])].concat();
    });
    for (rule, uid) in [
        ("a uid is empty", &b""[..]),
        ("a uid has a capital letter", b"Chest"),
        ("a uid starts with an underscore", b"_chest"),
        ("a uid has a space", b"a chest"),
        ("a uid has NUL", b"chest\0"),
        ("a uid is not UTF-8", b"chest\xff"),
    ] {
        let uid = uid.to_vec();
        raw(rule, &|raw| {
            raw.objects = [
                1_u32.to_le_bytes().to_vec(),
                object(0, chest, 16, &uid_bytes(&uid)),
            ]
            .concat();
        });
    }
    raw("a uid is 129 bytes", &|raw| {
        raw.objects = [
            1_u32.to_le_bytes().to_vec(),
            object(0, chest, 16, &uid_bytes(&b"a".repeat(129))),
        ]
        .concat();
    });
    raw("a uid's length passes the record", &|raw| {
        raw.objects = [
            1_u32.to_le_bytes().to_vec(),
            object(0, chest, 16, &[200, b'a']),
        ]
        .concat();
    });
    raw("a portal has an unknown kind", &|raw| {
        raw.objects = [
            1_u32.to_le_bytes().to_vec(),
            object(0, chest, 1, &[2, 0, 0, 0, 0, 0, 0, 0, 0]),
        ]
        .concat();
    });
    raw("an object flags fields but has none", &|raw| {
        raw.objects = [
            1_u32.to_le_bytes().to_vec(),
            object(0, chest, 2, &0_u16.to_le_bytes()),
        ]
        .concat();
    });
    raw("a field count passes the record", &|raw| {
        raw.objects = [
            1_u32.to_le_bytes().to_vec(),
            object(0, chest, 2, &100_u16.to_le_bytes()),
        ]
        .concat();
    });
    raw("a field has an unknown tag", &|raw| {
        raw.objects = [
            1_u32.to_le_bytes().to_vec(),
            object(0, chest, 2, &field(name, 7, &[])),
        ]
        .concat();
    });
    raw("a number field is NaN", &|raw| {
        raw.objects = [
            1_u32.to_le_bytes().to_vec(),
            object(0, chest, 2, &field(name, 4, &f64::NAN.to_le_bytes())),
        ]
        .concat();
    });
    raw("a number field is infinite", &|raw| {
        raw.objects = [
            1_u32.to_le_bytes().to_vec(),
            object(0, chest, 2, &field(name, 4, &f64::INFINITY.to_le_bytes())),
        ]
        .concat();
    });
    let deep = [b"[".repeat(65), b"]".repeat(65)].concat();
    for (rule, json) in [
        (
            "a JSON field is not canonical: it has a space",
            &b"[1, 2]"[..],
        ),
        ("a JSON field is a number", b"3"),
        ("a JSON field is a string", b"\"text\""),
        ("a JSON field is not JSON", b"[1,"),
        ("a JSON field has a trailing comma", b"[1,]"),
        ("a JSON field repeats a key", b"{\"a\":1,\"a\":2}"),
        ("a JSON field escapes a slash", b"[\"\\/\"]"),
        ("a JSON field escapes a letter", b"[\"\\u0041\"]"),
        (
            "a JSON field escapes a control character in uppercase",
            b"[\"\\u001F\"]",
        ),
        (
            "a JSON field escapes a newline as \\u000a",
            b"[\"\\u000a\"]",
        ),
        ("a JSON field holds NUL", b"[\"\\u0000\"]"),
        ("a JSON field holds a raw tab", b"[\"\t\"]"),
        ("a JSON field is not UTF-8", b"[\"\xff\"]"),
        (
            "a JSON field has an integer past the source format's range",
            b"[9007199254740992]",
        ),
        ("a JSON field has a number that is not finite", b"[1e400]"),
        ("a JSON field has a number with a leading zero", b"[01]"),
        ("a JSON field nests 65 deep", &deep),
        ("a JSON field has a comment", b"[1/*x*/]"),
        (
            "a JSON field starts with a byte-order mark",
            "\u{feff}[1]".as_bytes(),
        ),
    ] {
        let json = json.to_vec();
        raw(rule, &|raw| {
            raw.objects = [
                1_u32.to_le_bytes().to_vec(),
                object(0, chest, 2, &field(name, 6, &text(&json))),
            ]
            .concat();
        });
    }
    for (rule, json) in [
        ("custom data is an array", &b"[1]"[..]),
        (
            "custom data has a key that is not a namespace",
            b"{\"notes\":1}",
        ),
        (
            "custom data is not canonical: it has a space",
            b"{\"example.org\": 1}",
        ),
    ] {
        let json = json.to_vec();
        raw(rule, &|raw| {
            raw.objects = [
                1_u32.to_le_bytes().to_vec(),
                object(0, chest, 4, &text(&json)),
            ]
            .concat();
        });
    }
    raw("custom data's length passes the record", &|raw| {
        raw.objects = [
            1_u32.to_le_bytes().to_vec(),
            object(0, chest, 4, &u32::MAX.to_le_bytes()),
        ]
        .concat();
    });
    raw("an object flags an inventory but has none", &|raw| {
        raw.objects = [
            1_u32.to_le_bytes().to_vec(),
            object(0, chest, 8, &0_u32.to_le_bytes()),
        ]
        .concat();
    });
    raw("inventories nest 17 deep", &|raw| {
        let mut item = object(0xFFFF, chest, 0, &[]);
        for _ in 0..16 {
            let mut inventory = 1_u32.to_le_bytes().to_vec();
            inventory.extend_from_slice(&item);
            item = object(0xFFFF, chest, 8, &inventory);
        }
        let mut inventory = 1_u32.to_le_bytes().to_vec();
        inventory.extend_from_slice(&item);
        raw.objects = [
            1_u32.to_le_bytes().to_vec(),
            object(0, chest, 8, &inventory),
        ]
        .concat();
    });
    raw("a byte follows the objects", &|raw| raw.trailing = vec![0]);
    raw("the record ends inside an object", &|raw| {
        raw.objects.pop();
    });
    let reads: Vec<(i64, &str, Vec<u8>, Option<ErrorCode>)> = std::iter::once((
        1,
        "accepted: a record with every feature",
        frame(&record, true, true),
        None,
    ))
    .chain(
        rows.into_iter()
            .zip(2..)
            .map(|((rule, data, code), cx)| (cx, rule, data, Some(code))),
    )
    .collect();
    let mut case = Case::new(
        "chunk-records.twpack",
        "vault chunks at cx 1 on: one record every reader accepts, then one malformed record each; the read's rule says how",
    )
    .verify(ErrorCode::ChunkRecord);
    for (cx, rule, _, error) in &reads {
        let J::Object(mut op) = chunk_op("vault", 0, *cx, 0) else {
            unreachable!()
        };
        op.insert(1, ("rule".into(), J::from(*rule)));
        case = case.read(J::Object(op), *error);
    }
    base.add(case, |path| {
        let connection = Connection::open(path).unwrap();
        for (cx, _, data, _) in &reads {
            connection
                .execute(
                    "INSERT INTO chunks (plane, layer, cx, cy, encoding, data) VALUES ('vault', 0, ?1, 0, 'twc1+zstd', ?2)",
                    params![cx, data],
                )
                .unwrap();
        }
        // The control record's uids, at depths 0 and 2, are indexed.
        connection
            .execute_batch(
                "INSERT INTO object_uids VALUES ('control.chest', 'vault', 0, 1, 0, 0); \
                 INSERT INTO object_uids VALUES ('control.chest.inner', 'vault', 0, 1, 0, 2);",
            )
            .unwrap();
        connection.execute_batch("VACUUM").unwrap();
    });
}

/// An object with every feature, whose targets all exist in the fixture,
/// that every reader must accept: a uid, a location portal, a field of
/// every tag, canonical JSON with escapes, non-ASCII text, an exponent, a
/// negative zero and an underflowing number, custom data, and an inventory
/// two deep whose innermost entry has a uid of its own. The case that holds
/// it indexes both uids.
fn control_object(base: &Base<'_>) -> Vec<u8> {
    let chest = base.id("chest");
    let json = "[-0,1.5e3,\"\\u001f\\n\\\"\\\\\u{e9}\u{2028}\u{7f}\",{\"a\":[]},true,null]";
    let fields = [
        (base.id("name"), 0_u8, Vec::new()),
        (base.id("locked"), 2, Vec::new()),
        (base.id("count"), 3, (-5_i64).to_le_bytes().to_vec()),
        (base.id("loot"), 4, 0.25_f64.to_le_bytes().to_vec()),
        (base.id("tags"), 6, text(json.as_bytes())),
    ];
    let mut block = u16::try_from(fields.len()).unwrap().to_le_bytes().to_vec();
    for (name, tag, payload) in &fields {
        block.extend_from_slice(&name.to_le_bytes());
        block.push(*tag);
        block.extend_from_slice(payload);
    }
    let mut portal = vec![0];
    portal.extend_from_slice(&base.id("vault").to_le_bytes());
    portal.extend_from_slice(&(-3_i32).to_le_bytes());
    portal.extend_from_slice(&1_i64.to_le_bytes());
    portal.extend_from_slice(&(-1_i64).to_le_bytes());
    let mut item_fields = 2_u16.to_le_bytes().to_vec();
    item_fields.extend_from_slice(&base.id("name").to_le_bytes());
    item_fields.push(5);
    item_fields.extend_from_slice(&chest.to_le_bytes());
    item_fields.extend_from_slice(&base.id("locked").to_le_bytes());
    item_fields.push(1);
    let custom = text(
        "{\"example.org/tool\":{\"note\":\"\\t\u{1F600}\"},\"example.org\":1e-400}".as_bytes(),
    );
    let mut inner = 1_u32.to_le_bytes().to_vec();
    inner.extend_from_slice(&object(
        0xFFFF,
        chest,
        2 | 16,
        &[uid_bytes(b"control.chest.inner"), item_fields].concat(),
    ));
    let mut inventory = 1_u32.to_le_bytes().to_vec();
    inventory.extend_from_slice(&object(0xFFFF, chest, 8, &inner));
    object(
        5,
        chest,
        1 | 2 | 4 | 8 | 16,
        &[
            uid_bytes(b"control.chest"),
            portal,
            block,
            custom,
            inventory,
        ]
        .concat(),
    )
}

fn sha256_hex(data: &[u8]) -> String {
    let digest: [u8; 32] = Sha256::digest(data).into();
    digest.iter().fold(String::new(), |mut output, byte| {
        let _ = write!(output, "{byte:02x}");
        output
    })
}

#[allow(clippy::too_many_lines)]
fn reference_cases(base: &mut Base<'_>) {
    let strings = base.strings();
    let overworld = base.chunk("overworld", 0, 0, 0);
    let vault = base.chunk("vault", 0, 0, 0);
    let island = base.id("island");
    let village = base.id("village");
    let vault_plane = base.id("vault");
    let name = base.id("name");
    let changes: Vec<(&str, &str, &str, ChunkData)> = vec![
        (
            "reference-proto-id",
            "an object's prototype is a string ID past the strings",
            "vault",
            {
                let mut chunk = vault.clone();
                chunk.objects[0].proto = strings + 1;
                chunk
            },
        ),
        (
            "reference-proto",
            "an object's prototype is a string that names no proto definition",
            "vault",
            {
                let mut chunk = vault.clone();
                chunk.objects[0].proto = village;
                chunk
            },
        ),
        (
            "reference-layer",
            "an object's render layer is past the profile's",
            "vault",
            {
                let mut chunk = vault.clone();
                chunk.objects[0].layer = 7;
                chunk
            },
        ),
        (
            "reference-portal-plane",
            "a portal names a plane that does not exist",
            "vault",
            {
                let mut chunk = vault.clone();
                chunk.objects[0].portal = Some(PortalRecord::Location {
                    plane: village,
                    layer: 0,
                    x: 0,
                    y: 0,
                });
                chunk
            },
        ),
        (
            "reference-portal-position",
            "a portal's x is past the source format's integer range",
            "vault",
            {
                let mut chunk = vault.clone();
                chunk.objects[0].portal = Some(PortalRecord::Location {
                    plane: 0,
                    layer: 0,
                    x: tilewright_jsonc::MAX_SAFE_INTEGER + 1,
                    y: 0,
                });
                chunk
            },
        ),
        (
            "reference-portal-anchor",
            "a portal names an anchor its plane does not have",
            "vault",
            {
                let mut chunk = vault.clone();
                chunk.objects[0].portal = Some(PortalRecord::Anchor {
                    plane: 0,
                    anchor: base.id("village.well"),
                });
                chunk
            },
        ),
        (
            "reference-portal-anchor-plane",
            "a portal names an anchor of a plane that does not have it",
            "overworld",
            {
                let mut chunk = overworld.clone();
                let portal = chunk
                    .objects
                    .iter_mut()
                    .find_map(|object| object.portal.as_mut())
                    .unwrap();
                *portal = PortalRecord::Anchor {
                    plane: vault_plane,
                    anchor: base.id("cave.entrance"),
                };
                chunk
            },
        ),
        (
            "reference-field-name",
            "a field name is a string that is not a field name",
            "vault",
            {
                let mut chunk = vault.clone();
                chunk.objects[0].fields[0].0 = base.id("cave.entrance");
                chunk
            },
        ),
        (
            "reference-field-repeated",
            "an object lists a field twice",
            "vault",
            {
                let mut chunk = vault.clone();
                let first = chunk.objects[0].fields[0].clone();
                chunk.objects[0].fields.push(first);
                chunk
            },
        ),
        (
            "reference-field-string",
            "a string field is a string ID past the strings",
            "vault",
            {
                let mut chunk = vault.clone();
                chunk.objects[0].fields = vec![(name, FieldRecord::String(strings + 1))];
                chunk
            },
        ),
        (
            "reference-field-integer",
            "an integer field is past the source format's range",
            "vault",
            {
                let mut chunk = vault.clone();
                chunk.objects[0].fields = vec![(
                    name,
                    FieldRecord::Integer(tilewright_jsonc::MAX_SAFE_INTEGER + 1),
                )];
                chunk
            },
        ),
        (
            "reference-inventory",
            "an inventory entry's prototype names no proto definition",
            "vault",
            {
                let mut chunk = vault.clone();
                let mut item = chunk.objects[0].clone();
                item.uid = None;
                item.tile = None;
                item.proto = island;
                chunk.objects[0].inventory = vec![item];
                chunk
            },
        ),
        (
            "reference-palette-unused",
            "a palette entry is used by no cell",
            "vault",
            {
                let mut chunk = vault.clone();
                let last = *chunk.palette.last().unwrap();
                chunk.palette.push(last + 1);
                chunk
            },
        ),
        (
            "reference-palette-ground",
            "a ground cell names a string that is no proto definition",
            "vault",
            {
                let mut chunk = vault.clone();
                chunk.palette = vec![village];
                chunk
            },
        ),
        (
            "reference-palette-zone",
            "a zone cell names a zone of another plane",
            "vault",
            {
                let mut chunk = vault.clone();
                chunk.palette.push(village);
                chunk.zones[0] = u16::try_from(chunk.palette.len()).unwrap();
                chunk
            },
        ),
        (
            "reference-palette-string",
            "a palette entry is a string ID past the strings",
            "vault",
            {
                let mut chunk = vault.clone();
                chunk.palette.push(strings + 1);
                chunk.ground[1][0] = u16::try_from(chunk.palette.len()).unwrap();
                chunk
            },
        ),
        (
            "chunk-size-mismatch",
            "a chunk's record is 16 tiles square in a plane of 32",
            "vault",
            {
                let mut chunk = vault.clone();
                chunk.chunk_size = 16;
                for grid in &mut chunk.ground {
                    grid.truncate(256);
                }
                chunk.heights.truncate(256);
                chunk.zones.truncate(256);
                chunk
            },
        ),
        (
            "chunk-ground-mismatch",
            "a chunk's record has one ground layer where the profile has two",
            "vault",
            {
                let mut chunk = vault.clone();
                chunk.ground.pop();
                chunk
            },
        ),
    ];
    for (name, rule, plane, chunk) in changes {
        let code = if name.starts_with("chunk-") {
            ErrorCode::Chunk
        } else {
            ErrorCode::Reference
        };
        let data = encode_chunk(&chunk).unwrap();
        base.add(
            Case::new(format!("{name}.twpack"), rule)
                .read(chunk_op(plane, 0, 0, 0), None)
                .verify(code),
            |path| set_chunk(path, plane, 0, 0, 0, &data),
        );
    }
    for (name, rule, change) in [
        (
            "reference-deleted-proto",
            "an object's prototype definition is deleted",
            "DELETE FROM definitions WHERE type = 'proto' AND id = 'tree'",
        ),
        (
            "reference-deleted-ground",
            "a ground cell's prototype definition is deleted",
            "DELETE FROM definitions WHERE type = 'proto' AND id = 'sand'",
        ),
        (
            "reference-deleted-zone",
            "a zone cell's zone is deleted",
            "DELETE FROM zones WHERE id = 'village'",
        ),
        (
            "reference-deleted-anchor",
            "a portal's anchor is deleted",
            "DELETE FROM anchors WHERE id = 'cave.entrance'",
        ),
    ] {
        base.add(
            Case::new(format!("{name}.twpack"), rule)
                .read(chunk_op("overworld", 0, 0, 0), None)
                .verify(ErrorCode::Reference),
            |path| sql(path, change),
        );
    }
}

/// Broken object uids: `object_uids` rows that are malformed or disagree
/// with the chunk records, and records whose uids the table does not index.
#[allow(clippy::too_many_lines)]
fn uid_cases(base: &mut Base<'_>) {
    let found = None;
    let rejected = Some(ErrorCode::ObjectUid);
    for (name, rule, change, reads) in [
        (
            "uid-row-index",
            "a uid's row names another object of its chunk",
            "UPDATE object_uids SET object = 5 WHERE uid = 'village.chest'",
            vec![
                (object_location_op("village.chest"), found),
                (object_op("village.chest"), rejected),
            ],
        ),
        (
            "uid-row-past",
            "a uid's row names an index past its chunk's objects",
            "UPDATE object_uids SET object = 9 WHERE uid = 'village.chest'",
            vec![
                (object_location_op("village.chest"), found),
                (object_op("village.chest"), rejected),
            ],
        ),
        (
            "uid-row-chunk",
            "a uid's row names a chunk that does not exist",
            "UPDATE object_uids SET cx = 3 WHERE uid = 'village.chest'",
            vec![
                (object_location_op("village.chest"), found),
                (object_op("village.chest"), rejected),
            ],
        ),
        (
            "uid-row-other-chunk",
            "a uid's row names an existing chunk other than its object's",
            "UPDATE object_uids SET plane = 'vault' WHERE uid = 'village.chest'",
            vec![(object_op("village.chest"), rejected)],
        ),
        (
            "uid-row-missing",
            "an object's uid has no row",
            "DELETE FROM object_uids WHERE uid = 'vault.chest'",
            vec![
                (object_location_op("vault.chest"), found),
                (object_op("vault.chest"), found),
            ],
        ),
        (
            "uid-row-extra",
            "a row's uid is no object's: it names an object without one",
            "INSERT INTO object_uids VALUES ('zz.extra', 'overworld', 0, 0, 0, 0)",
            vec![
                (object_location_op("zz.extra"), found),
                (object_op("zz.extra"), rejected),
            ],
        ),
        (
            "uid-row-uid",
            "a row's uid is not an identifier",
            "UPDATE object_uids SET uid = 'Village.chest' WHERE uid = 'village.chest'",
            vec![(object_location_op("Village.chest"), found)],
        ),
        (
            "uid-row-uid-nul",
            "a row's uid holds NUL",
            "UPDATE object_uids SET uid = 'village.chest' || char(0) WHERE uid = 'village.chest'",
            vec![],
        ),
        (
            "uid-row-plane",
            "a row's plane is not an identifier",
            "UPDATE object_uids SET plane = 'Overworld' WHERE uid = 'village.chest'",
            vec![(object_location_op("village.chest"), rejected)],
        ),
        (
            "uid-row-plane-utf8",
            "a row's plane is text that is not UTF-8",
            "UPDATE object_uids SET plane = CAST(x'ff' AS TEXT) WHERE uid = 'village.chest'",
            vec![(object_location_op("village.chest"), rejected)],
        ),
        (
            "uid-row-layer",
            "a row's layer is past 32 bits",
            "UPDATE object_uids SET layer = 2147483648 WHERE uid = 'village.chest'",
            vec![(object_location_op("village.chest"), rejected)],
        ),
        (
            "uid-row-cx",
            "a row's chunk position puts tiles past the source format's range",
            "UPDATE object_uids SET cx = 281474976710656 WHERE uid = 'village.chest'",
            vec![(object_location_op("village.chest"), rejected)],
        ),
        (
            "uid-row-object",
            "a row's index is past 16,777,215",
            "UPDATE object_uids SET object = 16777216 WHERE uid = 'village.chest'",
            vec![(object_location_op("village.chest"), rejected)],
        ),
        (
            "uid-row-object-real",
            "a row's index is not an integer",
            "UPDATE object_uids SET object = 4.5 WHERE uid = 'village.chest'",
            vec![(object_location_op("village.chest"), rejected)],
        ),
    ] {
        let mut case = Case::new(format!("{name}.twpack"), rule).verify(ErrorCode::ObjectUid);
        for (op, error) in reads {
            case = case.read(op, error);
        }
        base.add(case, |path| sql(path, change));
    }
    let vault = base.chunk("vault", 0, 0, 0);
    for (name, rule, uid) in [
        (
            "uid-record-repeated",
            "a record repeats a uid that another plane's object has",
            Some("village.chest"),
        ),
        (
            "uid-record-dropped",
            "a record drops a uid that its row still names",
            None,
        ),
    ] {
        let mut chunk = vault.clone();
        chunk.objects[0].uid = uid.map(str::to_owned);
        let data = encode_chunk(&chunk).unwrap();
        base.add(
            Case::new(format!("{name}.twpack"), rule)
                .read(chunk_op("vault", 0, 0, 0), None)
                .verify(ErrorCode::ObjectUid),
            |path| set_chunk(path, "vault", 0, 0, 0, &data),
        );
    }
}

/// The valid pack's expected contents, read straight from its tables.
#[allow(clippy::too_many_lines)]
fn valid_entry(path: &Path, name: &str) -> J {
    let pack = Pack::open(path).unwrap();
    let summary = pack.verify().unwrap();
    let connection = Connection::open(path).unwrap();
    let rows = |sql: &str, columns: &[&'static str]| -> J {
        let mut statement = connection.prepare(sql).unwrap();
        let rows = statement
            .query_map([], |row| {
                Ok(J::object(
                    columns
                        .iter()
                        .enumerate()
                        .map(|(index, column)| {
                            let value = match row.get_ref(index).unwrap() {
                                rusqlite::types::ValueRef::Null => J::Null,
                                rusqlite::types::ValueRef::Integer(value) => J::Int(value),
                                rusqlite::types::ValueRef::Text(text) => {
                                    J::from(std::str::from_utf8(text).unwrap())
                                }
                                _ => unreachable!("packs hold no other values here"),
                            };
                            (*column, value)
                        })
                        .collect::<Vec<_>>(),
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        J::Array(rows)
    };
    let meta = connection
        .prepare("SELECT key, value FROM meta ORDER BY key")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .map(|row| {
            let (key, value) = row.unwrap();
            (key, J::from(value.as_str()))
        })
        .collect::<Vec<_>>();
    let strings: Vec<J> = connection
        .prepare("SELECT value FROM strings ORDER BY id")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(|value| J::from(value.unwrap().as_str()))
        .collect();
    let keys: Vec<(String, i32, i64, i64)> = connection
        .prepare("SELECT plane, layer, cx, cy FROM chunks ORDER BY plane, layer, cx, cy")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let chunks = keys
        .iter()
        .map(|(plane, layer, cx, cy)| {
            let chunk = pack.chunk(plane, *layer, *cx, *cy).unwrap().unwrap();
            let palette = chunk
                .palette
                .iter()
                .map(|id| J::from(pack.string(*id).unwrap().unwrap().as_str()))
                .collect();
            J::object([
                ("plane", J::from(plane.as_str())),
                ("layer", J::from(*layer)),
                ("cx", J::from(*cx)),
                ("cy", J::from(*cy)),
                ("palette", J::Array(palette)),
                ("objects", J::from(crate::render::count(&chunk.objects))),
                (
                    "render_sha256",
                    J::from(sha256_hex(crate::render::render(&chunk).as_bytes()).as_str()),
                ),
            ])
        })
        .collect();
    J::object([
        ("file", J::from(name)),
        (
            "summary",
            J::object([
                ("strings", J::from(summary.strings)),
                ("planes", J::from(summary.planes)),
                ("zones", J::from(summary.zones)),
                ("anchors", J::from(summary.anchors)),
                ("stairwells", J::from(summary.stairwells)),
                ("chunks", J::from(summary.chunks)),
                ("definitions", J::from(summary.definitions)),
                ("objects", J::from(summary.objects)),
                ("object_uids", J::from(summary.object_uids)),
            ]),
        ),
        ("meta", J::Object(meta)),
        (
            "layer_height",
            pack.layer_height().unwrap().map_or(J::Null, J::from),
        ),
        ("strings", J::Array(strings)),
        (
            "planes",
            rows(
                "SELECT id, kind, chunk_size, document FROM planes ORDER BY id",
                &["id", "kind", "chunk_size", "document"],
            ),
        ),
        (
            "zones",
            rows(
                "SELECT plane, id, parent, document FROM zones ORDER BY plane, id",
                &["plane", "id", "parent", "document"],
            ),
        ),
        (
            "anchors",
            rows(
                "SELECT plane, id, layer, x, y FROM anchors ORDER BY plane, id",
                &["plane", "id", "layer", "x", "y"],
            ),
        ),
        (
            "stairwells",
            rows(
                "SELECT plane, id, document FROM stairwells ORDER BY plane, id",
                &["plane", "id", "document"],
            ),
        ),
        (
            "steps",
            rows(
                "SELECT plane, layer, x, y, stairwell, step, proto FROM stairwell_steps ORDER BY plane, layer, x, y",
                &["plane", "layer", "x", "y", "stairwell", "step", "proto"],
            ),
        ),
        (
            "definitions",
            rows(
                "SELECT type, id, document FROM definitions ORDER BY type, id",
                &["type", "id", "document"],
            ),
        ),
        ("chunks", J::Array(chunks)),
        (
            "object_uids",
            rows(
                "SELECT uid, plane, layer, cx, cy, object FROM object_uids ORDER BY uid",
                &["uid", "plane", "layer", "cx", "cy", "object"],
            ),
        ),
    ])
}
