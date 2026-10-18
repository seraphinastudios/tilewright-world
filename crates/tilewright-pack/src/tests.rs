// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

use super::{
    ChunkData, FieldRecord, ObjectRecord, Pack, PortalRecord, compile, decode_chunk, decode_record,
    encode_chunk, encode_record, write_pack,
};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tilewright_source::World;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/basic")
}

fn world() -> World {
    tilewright_source::load(&fixture()).expect("the fixture is valid")
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

/// A pack written from the fixture, and a writable copy of it to tamper with.
fn tampered(name: &str, sql: &str) -> (tempfile::TempDir, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let written = write_pack(&world(), directory.path()).unwrap();
    let copy = directory.path().join(name);
    std::fs::copy(&written.path, &copy).unwrap();
    Connection::open(&copy).unwrap().execute_batch(sql).unwrap();
    (directory, copy)
}

fn hex(bytes: &[u8]) -> String {
    super::compile::hex(bytes)
}

fn small_chunk() -> ChunkData {
    ChunkData {
        chunk_size: 2,
        palette: vec![7, 9],
        ground: vec![vec![1, 2, 0, 1]],
        heights: vec![0, -4, 12, i32::MAX],
        zones: vec![0, 0, 2, 2],
        objects: vec![ObjectRecord {
            tile: Some(3),
            proto: 7,
            layer: 2,
            sub_layer: -1,
            z: 6,
            portal: Some(PortalRecord::Location {
                plane: 0,
                layer: -1,
                x: -40,
                y: 1 << 40,
            }),
            fields: vec![
                (1, FieldRecord::Null),
                (2, FieldRecord::Bool(true)),
                (3, FieldRecord::Integer(-5)),
                (4, FieldRecord::Number(0.25)),
                (5, FieldRecord::String(9)),
                (6, FieldRecord::Json("[1,2]".to_owned())),
            ],
            custom: Some("{\"example.org\":1}".to_owned()),
            inventory: vec![ObjectRecord {
                tile: None,
                proto: 9,
                layer: 0,
                sub_layer: 0,
                z: 0,
                portal: Some(PortalRecord::Anchor {
                    plane: 3,
                    anchor: 4,
                }),
                fields: Vec::new(),
                custom: None,
                inventory: Vec::new(),
            }],
        }],
    }
}

#[test]
fn compilation_is_deterministic() {
    let world = world();
    let first = compile(&world).unwrap();
    assert_eq!(compile(&world).unwrap(), first);
    assert_eq!(&first[..16], b"SQLite format 3\0");
}

#[test]
fn packs_are_named_by_digest_and_never_replaced() {
    let world = world();
    let directory = tempfile::tempdir().unwrap();
    let written = write_pack(&world, directory.path()).unwrap();
    assert!(written.created);
    let bytes = std::fs::read(&written.path).unwrap();
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    assert_eq!(written.digest, digest);
    assert_eq!(
        written.path.file_name().unwrap().to_str().unwrap(),
        format!("{}.twpack", hex(&digest))
    );
    let again = write_pack(&world, directory.path()).unwrap();
    assert!(!again.created);
    assert_eq!(again.path, written.path);
    std::fs::write(&written.path, b"tampered").unwrap();
    assert!(write_pack(&world, directory.path()).is_err());
    assert_eq!(std::fs::read(&written.path).unwrap(), b"tampered");
}

#[test]
fn packs_verify_and_decode_back_to_the_source() {
    let world = world();
    let directory = tempfile::tempdir().unwrap();
    let written = write_pack(&world, directory.path()).unwrap();
    let pack = Pack::open(&written.path).unwrap();
    let summary = pack.verify().unwrap();
    assert_eq!(
        (
            summary.planes,
            summary.zones,
            summary.anchors,
            summary.chunks,
            summary.definitions,
            summary.objects
        ),
        (2, 3, 2, 4, 11, 11)
    );
    assert_eq!(
        pack.meta("source_digest").unwrap().unwrap(),
        hex(&world.source_digest)
    );
    assert_eq!(pack.meta("profile_id").unwrap().as_deref(), Some("example"));

    let chunk = pack.chunk("overworld", 0, 0, 0).unwrap().unwrap();
    let name = |id: u32| pack.string(id).unwrap().unwrap();
    let palette: Vec<String> = chunk.palette.iter().map(|id| name(*id)).collect();
    assert_eq!(palette, ["edge_n", "grass", "island", "sand", "village"]);
    let cell = |grid: &[u16], index: usize| palette[usize::from(grid[index]) - 1].clone();
    assert_eq!(cell(&chunk.ground[0], 0), "sand");
    assert_eq!(cell(&chunk.ground[0], 1), "grass");
    assert_eq!(cell(&chunk.ground[1], 5 * 32 + 10), "edge_n");
    assert_eq!(chunk.ground[1][0], 0);
    assert_eq!(cell(&chunk.zones, 9 * 32 + 9), "village");
    assert_eq!(chunk.heights[12 * 32 + 12], 32);
    assert_eq!(chunk.objects.len(), 6);
    let wall = &chunk.objects[1];
    assert_eq!(
        (name(wall.proto).as_str(), wall.layer, wall.tile),
        ("wall", 4, Some(15))
    );
    match &chunk.objects[3].portal {
        Some(PortalRecord::Anchor { plane: 0, anchor }) => {
            assert_eq!(name(*anchor), "cave.entrance");
        }
        other => panic!("unexpected portal {other:?}"),
    }
    assert_eq!(chunk.objects[4].inventory[1].inventory.len(), 1);
    assert_eq!(
        chunk.objects[5].custom.as_deref(),
        Some("{\"example.org/notes\":{\"reviewed\":true}}")
    );
    assert!(pack.chunk("overworld", 0, 5, 5).unwrap().is_none());
}

#[test]
fn readers_reject_tampered_packs() {
    let world = world();
    let directory = tempfile::tempdir().unwrap();
    let written = write_pack(&world, directory.path()).unwrap();

    let renamed = directory.path().join(format!("{}.twpack", "0".repeat(64)));
    std::fs::copy(&written.path, &renamed).unwrap();
    let error = Pack::open(&renamed).unwrap().verify().unwrap_err();
    assert!(
        error.to_string().contains("does not match its name"),
        "{error}"
    );

    let triggered = directory.path().join("triggered.twpack");
    std::fs::copy(&written.path, &triggered).unwrap();
    Connection::open(&triggered)
        .unwrap()
        .execute_batch("CREATE VIEW extra AS SELECT 1")
        .unwrap();
    assert!(Pack::open(&triggered).is_err());

    let plain = directory.path().join("plain.sqlite");
    Connection::open(&plain)
        .unwrap()
        .execute_batch(
            "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID",
        )
        .unwrap();
    assert!(Pack::open(&plain).is_err());
}

#[test]
fn chunk_records_round_trip() {
    let chunk = small_chunk();
    let record = encode_record(&chunk).unwrap();
    assert_eq!(&record[..4], b"TWC1");
    assert_eq!(decode_record(&record).unwrap(), chunk);
    let compressed = encode_chunk(&chunk).unwrap();
    assert_eq!(decode_chunk(&compressed).unwrap(), chunk);
}

#[test]
fn malformed_chunk_records_are_rejected() {
    let record = encode_record(&small_chunk()).unwrap();
    let mut trailing = record.clone();
    trailing.push(0);
    assert!(decode_record(&trailing).is_err());
    assert!(decode_record(&record[..record.len() - 1]).is_err());
    let mut magic = record.clone();
    magic[0] = b'X';
    assert!(decode_record(&magic).is_err());
    let mut version = record.clone();
    version[4] = 2;
    assert!(decode_record(&version).is_err());

    let mut outside = small_chunk();
    outside.zones[0] = 3;
    assert!(encode_record(&outside).is_err());
    let mut misplaced = small_chunk();
    misplaced.objects[0].tile = Some(4);
    assert!(encode_record(&misplaced).is_err());

    // Flags byte of the top-level object: header 14 + palette 8 + grids
    // (4 × 2 + 4 × 4 + 4 × 2) + object count 4 + object fields 14.
    let flags = 14 + 8 + 32 + 4 + 14;
    let mut flagged = record.clone();
    flagged[flags] |= 0x80;
    assert!(decode_record(&flagged).is_err());

    let mut deep = small_chunk();
    let mut item = deep.objects[0].inventory[0].clone();
    for _ in 0..=super::MAXIMUM_DEPTH {
        let mut parent = deep.objects[0].inventory[0].clone();
        parent.inventory = vec![item];
        item = parent;
    }
    deep.objects[0].inventory = vec![item];
    assert!(encode_record(&deep).is_err());

    assert!(decode_chunk(&record).is_err());
    let oversized = zstd::bulk::compress(&vec![0; super::MAXIMUM_RECORD_BYTES + 1], 1).unwrap();
    assert!(decode_chunk(&oversized).is_err());
}

#[test]
fn unused_palette_entries_compile_and_are_not_interned() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("source");
    copy_tree(&fixture(), &root);
    std::fs::write(
        root.join("defs/proto/moss.jsonc"),
        "{\"format\": \"tilewright.def/1\", \"type\": \"proto\", \"id\": \"moss\", \"fields\": {\"face\": \"moss.png\", \"layer\": \"floor\"}}\n",
    )
    .unwrap();
    let vault = root.join("planes/vault/layers/0/0_0.jsonc");
    let text = std::fs::read_to_string(&vault).unwrap();
    std::fs::write(
        &vault,
        text.replace(
            "{\"c\": \"cave_floor\"}",
            "{\"c\": \"cave_floor\", \"m\": \"moss\"}",
        ),
    )
    .unwrap();
    let world = tilewright_source::load(&root).unwrap();
    let written = write_pack(&world, &directory.path().join("nested/output")).unwrap();
    let pack = Pack::open(&written.path).unwrap();
    pack.verify().unwrap();
    let strings = pack.summary().unwrap().strings;
    let all: Vec<String> = (1..=u32::try_from(strings).unwrap())
        .map(|id| pack.string(id).unwrap().unwrap())
        .collect();
    assert!(all.contains(&"cave_floor".to_owned()));
    assert!(!all.contains(&"moss".to_owned()));
}

#[test]
fn direct_chunk_reads_reject_unknown_encodings() {
    let (_directory, copy) = tampered(
        "encoding.twpack",
        "UPDATE chunks SET encoding = 'twc2+zstd' WHERE plane = 'vault'",
    );
    let pack = Pack::open(&copy).unwrap();
    assert!(pack.chunk("vault", 0, 0, 0).is_err());
    assert!(pack.chunk("overworld", 0, 0, 0).unwrap().is_some());
}

#[test]
fn metadata_must_be_exactly_the_format_keys() {
    for (name, sql) in [
        ("missing.twpack", "DELETE FROM meta WHERE key = 'compiler'"),
        ("extra.twpack", "INSERT INTO meta VALUES ('zzz', 'x')"),
        (
            "digest.twpack",
            "UPDATE meta SET value = 'XYZ' WHERE key = 'source_digest'",
        ),
        ("version.twpack", "PRAGMA user_version = 2"),
        (
            "long.twpack",
            "UPDATE meta SET value = value || replace(hex(zeroblob(1024)), '00', 'x') WHERE key = 'compiler'",
        ),
        (
            "blob.twpack",
            "UPDATE meta SET value = CAST(value AS BLOB) WHERE key = 'profile_id'",
        ),
    ] {
        let (_directory, copy) = tampered(name, sql);
        assert!(Pack::open(&copy).is_err(), "{name} opened");
    }
}

#[test]
fn verification_checks_rows_and_portal_targets() {
    for (name, sql) in [
        (
            "parent.twpack",
            "UPDATE zones SET parent = 'nowhere' WHERE id = 'village'",
        ),
        (
            "plane.twpack",
            "UPDATE anchors SET plane = 'attic' WHERE id = 'village.well'",
        ),
        (
            "kind.twpack",
            "UPDATE planes SET kind = 'shared-ish' WHERE id = 'vault'",
        ),
        (
            "anchor.twpack",
            "DELETE FROM anchors WHERE id = 'cave.entrance'",
        ),
    ] {
        let (_directory, copy) = tampered(name, sql);
        assert!(
            Pack::open(&copy).unwrap().verify().is_err(),
            "{name} verified"
        );
    }
}

#[test]
fn palettes_and_embedded_json_must_be_canonical() {
    let mut descending = small_chunk();
    descending.palette = vec![9, 7];
    assert!(encode_record(&descending).is_err());
    let mut zero = small_chunk();
    zero.palette = vec![0, 7];
    assert!(encode_record(&zero).is_err());
    for text in ["not-json", "[1, 2]", "3", "{\"a\":1,}"] {
        let mut json = small_chunk();
        json.objects[0].fields[5].1 = FieldRecord::Json(text.to_owned());
        assert!(encode_record(&json).is_err(), "{text} encoded");
    }
    let mut custom = small_chunk();
    custom.objects[0].custom = Some("[1]".to_owned());
    assert!(encode_record(&custom).is_err());

    // A record built by hand with a descending palette fails to decode too.
    let mut record = encode_record(&small_chunk()).unwrap();
    record[14..18].copy_from_slice(&9_u32.to_le_bytes());
    record[18..22].copy_from_slice(&7_u32.to_le_bytes());
    assert!(decode_record(&record).is_err());
}

#[test]
fn oversized_chunk_data_is_rejected_before_decoding() {
    assert_eq!(
        zstd::zstd_safe::compress_bound(super::MAXIMUM_RECORD_BYTES),
        super::MAXIMUM_CHUNK_BYTES
    );
    let error = decode_chunk(&vec![0; super::MAXIMUM_CHUNK_BYTES + 1]).unwrap_err();
    assert!(error.to_string().contains("exceeds"), "{error}");
    for (name, sql) in [
        (
            "large.twpack",
            format!(
                "UPDATE chunks SET data = zeroblob({}) WHERE plane = 'vault'",
                super::MAXIMUM_CHUNK_BYTES + 1
            ),
        ),
        (
            "text.twpack",
            "UPDATE chunks SET data = 'twc1' WHERE plane = 'vault'".to_owned(),
        ),
    ] {
        let (_directory, copy) = tampered(name, &sql);
        let pack = Pack::open(&copy).unwrap();
        let error = pack.chunk("vault", 0, 0, 0).unwrap_err();
        assert!(
            error.to_string().contains("not a blob of at most"),
            "{name}: {error}"
        );
        let error = pack.verify().unwrap_err();
        assert!(
            error.to_string().contains("not a blob of at most"),
            "{name}: {error}"
        );
    }
}

#[test]
fn verification_bounds_strings_and_looks_up_targets() {
    for (name, sql) in [
        (
            "renamed.twpack",
            "UPDATE strings SET value = 'vaultx' WHERE value = 'vault'",
        ),
        (
            "blob.twpack",
            "UPDATE strings SET value = CAST(value AS BLOB) WHERE id = 1",
        ),
        (
            "long.twpack",
            "UPDATE strings SET value = replace(hex(zeroblob(1048577)), '00', 'x') WHERE id = 1",
        ),
        (
            "anchor-id.twpack",
            "UPDATE anchors SET id = 'Village.Well' WHERE id = 'village.well'",
        ),
        (
            "zone-id.twpack",
            "UPDATE zones SET id = 'a b' WHERE id = 'village'",
        ),
    ] {
        let (_directory, copy) = tampered(name, sql);
        assert!(
            Pack::open(&copy).unwrap().verify().is_err(),
            "{name} verified"
        );
    }
    let (_directory, copy) = tampered(
        "string.twpack",
        "UPDATE strings SET value = replace(hex(zeroblob(1048577)), '00', 'x') WHERE id = 1",
    );
    assert!(Pack::open(&copy).unwrap().string(1).is_err());
}

#[test]
fn stored_documents_must_be_canonical_and_agree_with_their_rows() {
    for (name, sql) in [
        (
            "plane.twpack",
            "UPDATE planes SET document = 'not-json' WHERE id = 'vault'",
        ),
        (
            "kind.twpack",
            "UPDATE planes SET document = replace(document, '\"instanced\"', '\"shared\"') WHERE id = 'vault'",
        ),
        (
            "zone.twpack",
            "UPDATE zones SET document = '{\"id\":\"elsewhere\"}' WHERE id = 'village'",
        ),
        (
            "parent.twpack",
            "UPDATE zones SET document = replace(document, ',\"parent\":\"island\"', '') WHERE id = 'village'",
        ),
        (
            "spacing.twpack",
            "UPDATE definitions SET document = replace(document, '\"id\":', '\"id\": ') WHERE id = 'coin'",
        ),
        (
            "array.twpack",
            "UPDATE definitions SET document = '[]' WHERE id = 'coin'",
        ),
        (
            "profile.twpack",
            "UPDATE meta SET value = ' ' || value WHERE key = 'profile'",
        ),
        (
            "profile-id.twpack",
            "UPDATE meta SET value = 'other' WHERE key = 'profile_id'",
        ),
    ] {
        let (_directory, copy) = tampered(name, sql);
        let pack = Pack::open(&copy).unwrap();
        assert!(pack.verify().is_err(), "{name} verified");
    }
}

#[test]
fn canonical_json_longer_than_a_source_line_compiles_and_verifies() {
    // Two lines of 100,000 eight-digit numbers: each source line is under
    // the 1 MiB line limit, but the canonical JSON is one line of ~1.8 MB.
    let row = vec!["12345678"; 100_000].join(",");
    let big = format!("[\n[{row}],\n[{row}]\n]");
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("source");
    copy_tree(&fixture(), &root);
    std::fs::write(
        root.join("defs/proto/rock.jsonc"),
        format!("{{\"format\": \"tilewright.def/1\", \"type\": \"proto\", \"id\": \"rock\", \"fields\": {{\"face\": \"rock.png\", \"layer\": \"item\", \"footprint\": {big}}}}}\n"),
    )
    .unwrap();
    let vault = root.join("planes/vault/layers/0/0_0.jsonc");
    let text = std::fs::read_to_string(&vault).unwrap();
    std::fs::write(
        &vault,
        text.replace(
            "\"objects\": [",
            &format!("\"objects\": [{{\"at\": [2, 2], \"proto\": \"rock\", \"custom\": {{\"example.org\": {big}}}}}, "),
        ),
    )
    .unwrap();
    let world = tilewright_source::load(&root).unwrap();
    let written = write_pack(&world, &directory.path().join("output")).unwrap();
    let pack = Pack::open(&written.path).unwrap();
    pack.verify().unwrap();
    let chunk = pack.chunk("vault", 0, 0, 0).unwrap().unwrap();
    assert!(chunk.objects[0].custom.as_ref().unwrap().len() > 1024 * 1024);
}

#[test]
fn verification_checks_keys_sizes_order_and_cycles() {
    for (name, sql, problem) in [
        (
            "layer.twpack",
            "UPDATE chunks SET layer = 'bad' WHERE plane = 'vault'",
            "invalid layer or position",
        ),
        (
            "cx.twpack",
            "UPDATE chunks SET cx = 281474976710656 WHERE plane = 'vault'",
            "invalid layer or position",
        ),
        (
            "x.twpack",
            "UPDATE anchors SET x = 'east' WHERE id = 'village.well'",
            "invalid ID, layer or position",
        ),
        (
            "size.twpack",
            "DELETE FROM chunks WHERE plane = 'vault'; UPDATE planes SET chunk_size = 1, document = replace(document, '\"chunk_size\":32', '\"chunk_size\":1') WHERE id = 'vault'",
            "chunk size other than 32",
        ),
        (
            "cycle.twpack",
            "UPDATE zones SET parent = 'village', document = replace(document, '\"parent\":\"wilderness\"', '\"parent\":\"village\"') WHERE id = 'island'",
            "cycle",
        ),
        (
            "order.twpack",
            "CREATE TEMP TABLE s AS SELECT id, value FROM strings WHERE id IN (1, 2); \
             UPDATE strings SET value = 'zz.tmp' WHERE id = 1; \
             UPDATE strings SET value = (SELECT value FROM s WHERE id = 1) WHERE id = 2; \
             UPDATE strings SET value = (SELECT value FROM s WHERE id = 2) WHERE id = 1",
            "byte order",
        ),
        (
            "nul-anchor.twpack",
            "UPDATE anchors SET id = 'village' || char(0) || '!' WHERE id = 'village.well'",
            "invalid ID",
        ),
        (
            "nul-string.twpack",
            "UPDATE strings SET value = value || char(0) WHERE id = (SELECT max(id) FROM strings)",
            "without NUL",
        ),
    ] {
        let (_directory, copy) = tampered(name, sql);
        let error = Pack::open(&copy).unwrap().verify().unwrap_err();
        assert!(error.to_string().contains(problem), "{name}: {error}");
    }
}

#[test]
fn opening_stops_at_an_oversized_schema() {
    let tables = (0..100)
        .map(|index| format!("CREATE TABLE extra{index} (x);"))
        .collect::<Vec<_>>()
        .join("\n");
    let long = format!("CREATE TABLE extra ({} INTEGER)", "x".repeat(2000));
    for (name, sql) in [("tables.twpack", tables), ("long.twpack", long)] {
        let (_directory, copy) = tampered(name, &sql);
        let error = Pack::open(&copy).err().unwrap();
        assert!(error.to_string().contains("schema"), "{name}: {error}");
    }
}

#[test]
fn malformed_worlds_fail_to_compile_without_panicking() {
    let mut cell = world();
    let chunk = cell
        .planes
        .get_mut("vault")
        .unwrap()
        .chunks
        .values_mut()
        .next()
        .unwrap();
    chunk.ground[0].cells[0] = 99;
    assert!(compile(&cell).is_err());

    let mut outside = world();
    let chunk = outside
        .planes
        .get_mut("vault")
        .unwrap()
        .chunks
        .values_mut()
        .next()
        .unwrap();
    chunk.objects[0].at = Some([40, 0]);
    assert!(compile(&outside).is_err());
}

#[test]
fn existing_packs_are_compared_without_replacing_them() {
    let world = world();
    let directory = tempfile::tempdir().unwrap();
    let written = write_pack(&world, directory.path()).unwrap();
    let mut bytes = std::fs::read(&written.path).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    std::fs::write(&written.path, &bytes).unwrap();
    assert!(write_pack(&world, directory.path()).is_err());
    assert_eq!(std::fs::read(&written.path).unwrap(), bytes);
    std::fs::remove_file(&written.path).unwrap();
    std::fs::create_dir(&written.path).unwrap();
    assert!(write_pack(&world, directory.path()).is_err());
}

#[test]
fn malformed_sizes_and_depths_fail_to_compile_without_panicking() {
    let mut size = world();
    let vault = size.planes.get_mut("vault").unwrap();
    vault.chunk_size = 300;
    vault.chunks.values_mut().next().unwrap().objects[0].at = Some([299, 299]);
    assert!(compile(&size).is_err());

    // Far deeper than any stack could recurse through.
    let mut deep = world();
    let chunk = deep
        .planes
        .get_mut("vault")
        .unwrap()
        .chunks
        .values_mut()
        .next()
        .unwrap();
    let coin = |inventory| tilewright_source::Object {
        at: None,
        proto: "coin".to_owned(),
        layer: None,
        sub_layer: 0,
        z: 0,
        fields: Vec::new(),
        portal: None,
        inventory,
        custom: None,
    };
    let mut item = coin(Vec::new());
    for _ in 0..200_000 {
        item = coin(vec![item]);
    }
    chunk.objects[0].inventory = vec![item];
    let error = compile(&deep).unwrap_err();
    assert!(error.to_string().contains("deep"), "{error}");
    // Dismantle the chain without recursing through it.
    let chunk = deep
        .planes
        .get_mut("vault")
        .unwrap()
        .chunks
        .values_mut()
        .next()
        .unwrap();
    let mut stack = std::mem::take(&mut chunk.objects);
    while let Some(mut object) = stack.pop() {
        stack.append(&mut object.inventory);
    }
}

#[test]
fn encoding_stops_at_the_record_limit() {
    let mut large = small_chunk();
    large.palette = (1..=5_000_000).collect();
    assert!(encode_record(&large).is_err());
    let mut custom = small_chunk();
    custom.objects[0].custom = Some(format!(
        "{{\"example.org\":\"{}\"}}",
        "x".repeat(super::MAXIMUM_RECORD_BYTES)
    ));
    let error = encode_record(&custom).unwrap_err();
    assert!(error.to_string().contains("16 MiB"), "{error}");
}

#[test]
fn a_concurrent_identical_pack_is_not_an_error() {
    let data = compile(&world()).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pack.twpack");
    let temporary = || {
        let mut file = tempfile::NamedTempFile::new_in(directory.path()).unwrap();
        std::io::Write::write_all(&mut file, &data).unwrap();
        file
    };
    std::fs::write(&path, &data).unwrap();
    assert!(!super::compile::persist(temporary(), &path, &data).unwrap());
    std::fs::write(&path, b"other").unwrap();
    assert!(super::compile::persist(temporary(), &path, &data).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"other");
}

#[test]
fn verification_checks_names_profiles_portals_and_encodings() {
    let directory = tempfile::tempdir().unwrap();
    let written = write_pack(&world(), directory.path()).unwrap();
    let name = written.path.file_name().unwrap().to_str().unwrap();
    let upper = directory
        .path()
        .join(name.replace(".twpack", "").to_ascii_uppercase() + ".twpack");
    std::fs::copy(&written.path, &upper).unwrap();
    let error = Pack::open(&upper).unwrap().verify().unwrap_err();
    assert!(
        error.to_string().contains("does not match its name"),
        "{error}"
    );

    for (name, sql, problem) in [
        (
            "ground.twpack",
            "DELETE FROM chunks; UPDATE meta SET value = replace(value, '\"ground_layers\":[\"floor\",\"mask\"]', '\"ground_layers\":[\"floor\",\"mask\",\"a\",\"b\",\"c\",\"d\",\"e\",\"f\",\"g\"]') WHERE key = 'profile'",
            "ground_layers",
        ),
        (
            "utf8.twpack",
            "UPDATE strings SET value = CAST(x'ff' AS TEXT) WHERE id = (SELECT max(id) FROM strings)",
            "UTF-8",
        ),
    ] {
        let (_directory, copy) = tampered(name, sql);
        let error = Pack::open(&copy).unwrap().verify().unwrap_err();
        assert!(error.to_string().contains(problem), "{name}: {error}");
    }

    let (_directory, copy) = tampered("portal.twpack", "SELECT 1");
    let pack = Pack::open(&copy).unwrap();
    let mut chunk = pack.chunk("overworld", 0, 0, 0).unwrap().unwrap();
    drop(pack);
    let portal = chunk
        .objects
        .iter_mut()
        .find_map(|object| match &mut object.portal {
            Some(PortalRecord::Location { x, .. }) => Some(x),
            _ => None,
        })
        .unwrap();
    *portal = tilewright_jsonc::MAX_SAFE_INTEGER + 1;
    Connection::open(&copy)
        .unwrap()
        .execute(
            "UPDATE chunks SET data = ?1 WHERE plane = 'overworld' AND layer = 0 AND cx = 0 AND cy = 0",
            [encode_chunk(&chunk).unwrap()],
        )
        .unwrap();
    let error = Pack::open(&copy).unwrap().verify().unwrap_err();
    assert!(
        error.to_string().contains("references a missing"),
        "{error}"
    );
}
