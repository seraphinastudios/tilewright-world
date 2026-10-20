// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

use super::{
    ChunkData, FieldRecord, ObjectRecord, Pack, PortalRecord, VerifyLimits, compile, decode_chunk,
    decode_record, encode_chunk, encode_record, write_pack,
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
            uid: None,
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
                uid: Some("chest.coin".to_owned()),
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
        (2, 3, 2, 5, 12, 11)
    );
    assert_eq!(
        pack.meta("source_digest").unwrap().unwrap(),
        hex(&world.source_digest())
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
    // Only this version decodes: version 1 had no uids.
    assert_eq!(super::RECORD_VERSION, 2);
    for other in [1, 3] {
        let mut version = record.clone();
        version[4] = other;
        assert!(decode_record(&version).is_err(), "version {other}");
    }

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
        ("version.twpack", "PRAGMA user_version = 1"),
        ("later.twpack", "PRAGMA user_version = 3"),
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
    // A file system that ignores case, as macOS and Windows do by default,
    // has the pack under that name already.
    if !upper.exists() {
        std::fs::copy(&written.path, &upper).unwrap();
        let error = Pack::open(&upper).unwrap().verify().unwrap_err();
        assert!(
            error.to_string().contains("does not match its name"),
            "{error}"
        );
    }

    for (name, sql, problem) in [
        (
            "ground.twpack",
            "DELETE FROM chunks; DELETE FROM object_uids; UPDATE meta SET value = replace(value, '\"ground_layers\":[\"floor\",\"mask\"]', '\"ground_layers\":[\"floor\",\"mask\",\"a\",\"b\",\"c\",\"d\",\"e\",\"f\",\"g\"]') WHERE key = 'profile'",
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

#[test]
fn grid_and_object_values_must_name_their_targets() {
    for (name, sql) in [
        (
            "ground.twpack",
            "DELETE FROM definitions WHERE type = 'proto' AND id = 'sand'",
        ),
        ("zone.twpack", "DELETE FROM zones WHERE id = 'village'"),
        (
            "object.twpack",
            "DELETE FROM definitions WHERE type = 'proto' AND id = 'tree'",
        ),
    ] {
        let (_directory, copy) = tampered(name, sql);
        assert!(
            Pack::open(&copy).unwrap().verify().is_err(),
            "{name} verified"
        );
    }

    let (_directory, copy) = tampered("unused.twpack", "SELECT 1");
    let pack = Pack::open(&copy).unwrap();
    let mut chunk = pack.chunk("vault", 0, 0, 0).unwrap().unwrap();
    drop(pack);
    let last = *chunk.palette.last().unwrap();
    chunk.palette.push(last + 1);
    Connection::open(&copy)
        .unwrap()
        .execute(
            "UPDATE chunks SET data = ?1 WHERE plane = 'vault'",
            [encode_chunk(&chunk).unwrap()],
        )
        .unwrap();
    let error = Pack::open(&copy).unwrap().verify().unwrap_err();
    assert!(error.to_string().contains("unused"), "{error}");
}

#[test]
fn a_minimal_object_fits_the_last_bytes_of_a_record() {
    // `length` characters over a fixed 17 strings (each under the 1 MiB
    // string limit), so the JSON's overhead does not depend on `length`.
    let custom = |length: usize, fields: usize| {
        let strings: Vec<String> = (0..17)
            .map(|index| {
                let start = (index * 1_000_000).min(length);
                let end = ((index + 1) * 1_000_000).min(length);
                format!("\"{}\"", "x".repeat(end - start))
            })
            .collect();
        let mut chunk = small_chunk();
        let mut minimal = chunk.objects[0].inventory[0].clone();
        minimal.uid = None;
        minimal.tile = Some(0);
        minimal.portal = None;
        minimal.fields = (1..=fields)
            .map(|name| (u32::try_from(name).unwrap(), FieldRecord::Null))
            .collect();
        chunk.objects[0].custom = Some(format!("{{\"example.org\":[{}]}}", strings.join(",")));
        chunk.objects.push(minimal);
        chunk
    };
    // A bare object, and one whose 256 null fields take 5 bytes each.
    for fields in [0, 256] {
        let base = encode_record(&custom(0, fields)).unwrap().len();
        let exact = custom(super::MAXIMUM_RECORD_BYTES - base, fields);
        let record = encode_record(&exact).unwrap();
        assert_eq!(record.len(), super::MAXIMUM_RECORD_BYTES);
        assert_eq!(decode_record(&record).unwrap(), exact);
        assert!(encode_record(&custom(super::MAXIMUM_RECORD_BYTES - base + 1, fields)).is_err());
    }
}

#[test]
fn stored_default_zones_and_profile_layers_must_resolve() {
    for (name, sql, problem) in [
        (
            "default.twpack",
            "UPDATE planes SET document = replace(document, '\"default_zone\":\"wilderness\"', '\"default_zone\":\"nowhere\"') WHERE id = 'overworld'",
            "disagrees with its row",
        ),
        (
            "duplicate.twpack",
            "DELETE FROM chunks; DELETE FROM object_uids; UPDATE meta SET value = replace(value, '\"living\"', '\"mask\"') WHERE key = 'profile'",
            "unique render_layers",
        ),
        (
            "null.twpack",
            "DELETE FROM chunks; DELETE FROM object_uids; UPDATE meta SET value = replace(value, '\"living\"', 'null') WHERE key = 'profile'",
            "unique render_layers",
        ),
        (
            "ground.twpack",
            "DELETE FROM chunks; DELETE FROM object_uids; UPDATE meta SET value = replace(value, '\"ground_layers\":[\"floor\",\"mask\"]', '\"ground_layers\":[\"floor\",\"roof\"]') WHERE key = 'profile'",
            "not a render layer",
        ),
    ] {
        let (_directory, copy) = tampered(name, sql);
        let error = Pack::open(&copy).unwrap().verify().unwrap_err();
        assert!(error.to_string().contains(problem), "{name}: {error}");
    }
}

#[test]
fn identity_page_size_and_profile_limits_are_checked() {
    let (_directory, copy) = tampered("pages.twpack", "PRAGMA page_size = 8192; VACUUM;");
    let error = Pack::open(&copy).err().unwrap();
    assert!(error.to_string().contains("4096-byte pages"), "{error}");

    let long = "1".repeat(65);
    for (name, sql) in [
        (
            "upper.twpack",
            "UPDATE meta SET value = replace(value, '\"id\":\"example\"', '\"id\":\"Example\"') WHERE key = 'profile'; UPDATE meta SET value = 'Example' WHERE key = 'profile_id'".to_owned(),
        ),
        (
            "version.twpack",
            format!("UPDATE meta SET value = replace(value, '\"version\":\"1.0.0\"', '\"version\":\"{long}\"') WHERE key = 'profile'; UPDATE meta SET value = '{long}' WHERE key = 'profile_version'"),
        ),
    ] {
        let (_directory, copy) = tampered(name, &sql);
        let error = Pack::open(&copy).unwrap().verify().unwrap_err();
        assert!(error.to_string().contains("profile"), "{name}: {error}");
    }
}

// A `World` is only built by `tilewright_source::load`, so the compiler's own
// bounds are checked through its internal steps, with parts of a loaded world.

fn vault_chunk(world: &World) -> tilewright_source::Chunk {
    world.planes()["vault"]
        .chunks
        .values()
        .next()
        .unwrap()
        .clone()
}

#[test]
fn compile_checks_grids_before_copying_them() {
    use super::compile::{check_chunk_shape, palette_value};
    let world = world();
    let ground = world.profile().ground_layers.len();
    let mut chunk = vault_chunk(&world);
    check_chunk_shape(&chunk, ground).unwrap();
    assert!(palette_value(&chunk.ground[0], 98).is_err());
    chunk.heights.truncate(2);
    let error = check_chunk_shape(&chunk, ground).unwrap_err();
    assert!(error.to_string().contains("grids"), "{error}");
}

#[test]
fn compile_bounds_object_records() {
    use super::compile::{Strings, object_record, render_layers};
    let world = world();
    let strings = Strings::collect(&world).unwrap();
    let layers = render_layers(world.profile()).unwrap();
    let chest = vault_chunk(&world).objects[0].clone();
    let record = |object: &tilewright_source::Object, budget: usize| {
        let mut budget = budget;
        object_record(object, &layers, &strings, 0, &mut budget)
    };
    record(&chest, usize::MAX).unwrap();

    let mut outside = chest.clone();
    outside.at = Some([40, 0]);
    assert!(record(&outside, usize::MAX).is_err());

    let mut portal = chest.clone();
    portal.portal = Some(tilewright_source::Portal::Location {
        plane: None,
        layer: 0,
        at: [tilewright_jsonc::MAX_SAFE_INTEGER + 1, 0],
    });
    let error = record(&portal, usize::MAX).unwrap_err();
    assert!(error.to_string().contains("integer range"), "{error}");

    let mut large = chest.clone();
    large.custom = Some(format!(
        "{{\"example.org\":\"{}\"}}",
        "x".repeat(super::MAXIMUM_RECORD_BYTES)
    ));
    let error = record(&large, super::MAXIMUM_RECORD_BYTES).unwrap_err();
    assert!(error.to_string().contains("16 MiB"), "{error}");

    // Far deeper than any stack could recurse through.
    let coin = |inventory| tilewright_source::Object {
        uid: None,
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
    let mut deep = chest;
    deep.inventory = vec![item];
    let error = record(&deep, usize::MAX).unwrap_err();
    assert!(error.to_string().contains("deep"), "{error}");
    // Dismantle the chain without recursing through it.
    let mut stack = std::mem::take(&mut deep.inventory);
    while let Some(mut object) = stack.pop() {
        stack.append(&mut object.inventory);
    }
}

#[test]
fn compile_bounds_strings_and_documents_by_role() {
    use super::compile::{document, field_name, identifier, text};
    assert!(identifier("coin").is_ok());
    let error = identifier(&"x".repeat(10 * 1024 * 1024)).unwrap_err();
    assert!(error.to_string().contains("not an identifier"), "{error}");
    assert!(field_name("count").is_ok());
    assert!(field_name("Count").is_err());
    assert!(text(&"x".repeat(1024 * 1024)).is_ok());
    let error = text(&"x".repeat(1024 * 1024 + 1)).unwrap_err();
    assert!(error.to_string().contains("longer than 1 MiB"), "{error}");
    assert!(text("a\0b").is_err());
    let error = document(&"x".repeat(64 * 1024 * 1024 + 1)).unwrap_err();
    assert!(error.to_string().contains("longer than 64 MiB"), "{error}");
}

#[test]
fn the_profile_document_must_list_the_typed_layers() {
    use super::compile::check_profile_document;
    let world = world();
    check_profile_document(world.profile()).unwrap();
    let mut reordered = world.profile().clone();
    reordered.render_layers.swap(0, 1);
    let error = check_profile_document(&reordered).unwrap_err();
    assert!(
        error.to_string().contains("differ from the typed"),
        "{error}"
    );
    let mut ground = world.profile().clone();
    ground.ground_layers.reverse();
    assert!(check_profile_document(&ground).is_err());
    for height in [None, Some(51)] {
        let mut stacked = world.profile().clone();
        stacked.layer_height = height;
        assert!(check_profile_document(&stacked).is_err(), "{height:?}");
    }
}

#[test]
fn a_chunk_of_exactly_16_mib_compiles() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("source");
    copy_tree(&fixture(), &root);
    let vault = root.join("planes/vault/layers/0/0_0.jsonc");
    let original = std::fs::read_to_string(&vault).unwrap();
    // `length` characters over a fixed 17 strings on their own lines, each
    // under the source string and line limits.
    let sized = |length: usize| {
        let strings: Vec<String> = (0..17)
            .map(|index| {
                let start = (index * 1_000_000).min(length);
                let end = ((index + 1) * 1_000_000).min(length);
                format!("\"{}\"", "x".repeat(end - start))
            })
            .collect();
        std::fs::write(
            &vault,
            original.replace(
                "\"fields\": {\"loot\": \"chest_common\"}",
                &format!(
                    "\"fields\": {{\"loot\": \"chest_common\"}}, \"custom\": {{\"example.org\": [\n{}\n]}}",
                    strings.join(",\n")
                ),
            ),
        )
        .unwrap();
        tilewright_source::load(&root).unwrap()
    };
    let record_length = |world: &World| {
        let output = tempfile::tempdir().unwrap();
        let written = write_pack(world, output.path()).unwrap();
        let chunk = Pack::open(&written.path)
            .unwrap()
            .chunk("vault", 0, 0, 0)
            .unwrap()
            .unwrap();
        encode_record(&chunk).unwrap().len()
    };
    let base = record_length(&sized(0));
    assert_eq!(
        record_length(&sized(super::MAXIMUM_RECORD_BYTES - base)),
        super::MAXIMUM_RECORD_BYTES
    );
    assert!(compile(&sized(super::MAXIMUM_RECORD_BYTES - base + 1)).is_err());
}

#[test]
fn verification_stops_at_its_decoded_byte_budget() {
    assert_eq!(super::DEFAULT_MAXIMUM_DECODED_BYTES, 8 * 1024 * 1024 * 1024);
    let directory = tempfile::tempdir().unwrap();
    let written = write_pack(&world(), directory.path()).unwrap();
    let pack = Pack::open(&written.path).unwrap();
    let total: u64 = [
        ("overworld", -1, 0, 0),
        ("overworld", 0, -1, 0),
        ("overworld", 0, 0, 0),
        ("overworld", 1, 0, 0),
        ("vault", 0, 0, 0),
    ]
    .iter()
    .map(|(plane, layer, cx, cy)| {
        let chunk = pack.chunk(plane, *layer, *cx, *cy).unwrap().unwrap();
        u64::try_from(encode_record(&chunk).unwrap().len()).unwrap()
    })
    .sum();
    let limits = |maximum_decoded_bytes| VerifyLimits {
        maximum_decoded_bytes,
        ..VerifyLimits::default()
    };
    pack.verify_with(&limits(total)).unwrap();
    let error = pack.verify_with(&limits(total - 1)).unwrap_err();
    assert!(error.to_string().contains("verification limit"), "{error}");
    assert!(pack.verify_with(&limits(0)).is_err());
}

#[test]
fn chunks_are_exactly_one_frame() {
    let single = encode_chunk(&small_chunk()).unwrap();
    let empty = zstd::bulk::compress(&[], 1).unwrap();
    let mut prefixed = empty.clone();
    prefixed.extend_from_slice(&single);
    let mut suffixed = single.clone();
    suffixed.extend_from_slice(&empty);
    for data in [&prefixed, &suffixed] {
        let error = decode_chunk(data).unwrap_err();
        assert!(error.to_string().contains("one zstd frame"), "{error}");
    }

    let (_directory, copy) = tampered("frames.twpack", "SELECT 1");
    let stored: Vec<u8> = Connection::open(&copy)
        .unwrap()
        .query_row("SELECT data FROM chunks WHERE plane = 'vault'", [], |row| {
            row.get(0)
        })
        .unwrap();
    let mut data = empty;
    data.extend_from_slice(&stored);
    Connection::open(&copy)
        .unwrap()
        .execute("UPDATE chunks SET data = ?1 WHERE plane = 'vault'", [data])
        .unwrap();
    let error = Pack::open(&copy).unwrap().verify().unwrap_err();
    assert!(error.to_string().contains("one zstd frame"), "{error}");
}

#[test]
fn integer_fields_must_be_within_the_source_range() {
    let (_directory, copy) = tampered("integer.twpack", "SELECT 1");
    let pack = Pack::open(&copy).unwrap();
    let mut chunk = pack.chunk("vault", 0, 0, 0).unwrap().unwrap();
    drop(pack);
    chunk.objects[0].fields[0].1 = FieldRecord::Integer(i64::MAX);
    Connection::open(&copy)
        .unwrap()
        .execute(
            "UPDATE chunks SET data = ?1 WHERE plane = 'vault'",
            [encode_chunk(&chunk).unwrap()],
        )
        .unwrap();
    let error = Pack::open(&copy).unwrap().verify().unwrap_err();
    assert!(error.to_string().contains("integer field"), "{error}");
}

/// Re-encodes the vault chunk of a fresh pack copy after `change`, and
/// returns the verification error.
fn tampered_vault(name: &str, change: impl FnOnce(&mut ChunkData, &dyn Fn(&str) -> u32)) -> String {
    let (_directory, copy) = tampered(name, "SELECT 1");
    let connection = Connection::open(&copy).unwrap();
    let id = |value: &str| -> u32 {
        connection
            .query_row("SELECT id FROM strings WHERE value = ?1", [value], |row| {
                row.get(0)
            })
            .unwrap()
    };
    let pack = Pack::open(&copy).unwrap();
    let mut chunk = pack.chunk("vault", 0, 0, 0).unwrap().unwrap();
    drop(pack);
    change(&mut chunk, &id);
    connection
        .execute(
            "UPDATE chunks SET data = ?1 WHERE plane = 'vault'",
            [encode_chunk(&chunk).unwrap()],
        )
        .unwrap();
    Pack::open(&copy).unwrap().verify().unwrap_err().to_string()
}

#[test]
fn field_names_are_names_and_listed_once() {
    let error = tampered_vault("misnamed.twpack", |chunk, id| {
        chunk.objects[0].fields[0].0 = id("cave.entrance");
    });
    assert!(error.contains("misnames a field"), "{error}");
    let error = tampered_vault("repeated.twpack", |chunk, _| {
        let field = chunk.objects[0].fields[0].clone();
        chunk.objects[0].fields.push(field);
    });
    assert!(error.contains("repeats"), "{error}");
}

#[test]
fn lookups_relative_to_a_plane_are_not_reused_for_another() {
    // The overworld, checked first, has `cave.entrance`; the vault does not.
    let error = tampered_vault("anchor.twpack", |chunk, id| {
        chunk.objects[0].portal = Some(PortalRecord::Anchor {
            plane: 0,
            anchor: id("cave.entrance"),
        });
    });
    assert!(error.contains("anchor"), "{error}");
}

#[test]
fn custom_data_keys_are_namespaces() {
    let mut custom = small_chunk();
    custom.objects[0].custom = Some("{\"x\":1}".to_owned());
    let error = encode_record(&custom).unwrap_err();
    assert!(error.to_string().contains("namespaces"), "{error}");

    let record = encode_record(&small_chunk()).unwrap();
    let text = b"{\"example.org\":1}";
    let at = record
        .windows(text.len())
        .position(|window| window == text)
        .unwrap();
    let mut patched = record.clone();
    patched[at + 9] = b'X';
    let error = decode_record(&patched).unwrap_err();
    assert!(error.to_string().contains("namespaces"), "{error}");
}

#[test]
fn streaming_decodes_visit_every_object_and_keep_none() {
    let data = encode_chunk(&small_chunk()).unwrap();
    let mut seen = Vec::new();
    let mut visit = |object: &ObjectRecord| {
        assert!(object.inventory.is_empty());
        seen.push(object.proto);
        Ok(())
    };
    let chunk = super::chunk::decode_chunk_with(&data, Some(&mut visit)).unwrap();
    assert!(chunk.objects.is_empty());
    assert_eq!(seen, [7, 9], "the chest, then its inventory entry");
    assert_eq!(chunk.palette, small_chunk().palette);

    // A visitor's error stops decoding, and verification reports its own.
    let mut stop = |_: &ObjectRecord| Err(super::DecodeError("stop".into()));
    assert_eq!(
        super::chunk::decode_chunk_with(&data, Some(&mut stop)).unwrap_err(),
        super::DecodeError("stop".into())
    );
    let directory = tempfile::tempdir().unwrap();
    let written = write_pack(&world(), directory.path()).unwrap();
    assert_eq!(
        Pack::open(&written.path).unwrap().verify().unwrap().objects,
        11
    );
}

#[test]
fn stored_document_custom_data_is_namespaced() {
    let (_directory, copy) = tampered(
        "custom.twpack",
        "UPDATE planes SET document = replace(document, '}', ',\"custom\":{\"x\":1}}') WHERE id = 'vault'",
    );
    let error = Pack::open(&copy).unwrap().verify().unwrap_err();
    assert!(
        error.to_string().contains("namespaced custom data"),
        "{error}"
    );
}

#[test]
fn a_pack_file_is_exactly_its_pages() {
    let directory = tempfile::tempdir().unwrap();
    let written = write_pack(&world(), directory.path()).unwrap();
    for (name, extend) in [("trailing.twpack", 1_u64), ("sparse.twpack", 1 << 32)] {
        let copy = directory.path().join(name);
        std::fs::copy(&written.path, &copy).unwrap();
        let file = std::fs::OpenOptions::new().write(true).open(&copy).unwrap();
        file.set_len(written.bytes + extend).unwrap();
        let error = Pack::open(&copy).err().unwrap();
        assert!(
            error.to_string().contains("pages of 4096 bytes"),
            "{name}: {error}"
        );
    }
}

#[test]
fn packs_have_no_free_pages() {
    // Dropping a table leaves its pages free without changing the schema,
    // and SQLite's integrity check accepts them.
    let (directory, copy) = tampered(
        "free.twpack",
        "CREATE TABLE scratch (x); INSERT INTO scratch VALUES (zeroblob(1000000)); DROP TABLE scratch;",
    );
    let connection = Connection::open(&copy).unwrap();
    let free: i64 = connection
        .query_row("PRAGMA freelist_count", [], |row| row.get(0))
        .unwrap();
    assert!(free > 200, "{free}");
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
    drop(connection);
    let error = Pack::open(&copy).err().unwrap();
    assert!(error.to_string().contains("free pages"), "{error}");

    // A header that hides the free pages leaves them unused, which the
    // integrity check reports before the file is hashed: the name is not
    // the file's digest, and that is not what fails.
    let mut bytes = std::fs::read(&copy).unwrap();
    bytes[32..40].fill(0);
    let hidden = directory.path().join(format!("{}.twpack", "0".repeat(64)));
    std::fs::write(&hidden, bytes).unwrap();
    let error = Pack::open(&hidden).unwrap().verify().unwrap_err();
    assert!(error.to_string().contains("never used"), "{error}");
}

#[test]
fn packs_past_the_size_limit_are_not_opened_or_hashed() {
    assert_eq!(super::DEFAULT_MAXIMUM_PACK_BYTES, 8 * 1024 * 1024 * 1024);
    let directory = tempfile::tempdir().unwrap();
    let written = write_pack(&world(), directory.path()).unwrap();
    let limits = |maximum_pack_bytes| VerifyLimits {
        maximum_pack_bytes,
        ..VerifyLimits::default()
    };
    let exact = limits(written.bytes);
    let short = limits(written.bytes - 1);

    // The name is the digest, so verifying hashes the whole file.
    let pack = Pack::open_with(&written.path, &exact).unwrap();
    pack.verify_with(&exact).unwrap();
    assert_eq!(pack.file_digest().unwrap(), written.digest);
    let error = pack.verify_with(&short).unwrap_err();
    assert!(error.to_string().contains("pack size limit"), "{error}");
    let error = Pack::open_with(&written.path, &short).err().unwrap();
    assert!(error.to_string().contains("pack size limit"), "{error}");

    // The length is checked before SQLite opens the file: a sparse file
    // past the default limit is rejected without being read.
    let sparse = directory.path().join("sparse.twpack");
    std::fs::File::create(&sparse)
        .unwrap()
        .set_len(super::DEFAULT_MAXIMUM_PACK_BYTES + 1)
        .unwrap();
    let error = Pack::open(&sparse).err().unwrap();
    assert!(error.to_string().contains("pack size limit"), "{error}");

    // Hashing stops at the limit the pack was opened with, even if the
    // file at its path has grown since.
    let grown = directory.path().join("grown.twpack");
    std::fs::copy(&written.path, &grown).unwrap();
    let pack = Pack::open_with(&grown, &exact).unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&grown)
        .unwrap()
        .set_len(written.bytes + 1)
        .unwrap();
    let error = pack.file_digest().unwrap_err();
    assert!(error.to_string().contains("pack size limit"), "{error}");

    // Verifying keeps the limit the pack was opened with, whatever limits
    // it is given: it fails on the size before the name's digest is
    // compared.
    let named = tempfile::tempdir().unwrap();
    let copy = named.path().join(written.path.file_name().unwrap());
    std::fs::copy(&written.path, &copy).unwrap();
    let pack = Pack::open_with(&copy, &exact).unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(&copy)
        .unwrap()
        .set_len(written.bytes + 1)
        .unwrap();
    for error in [
        pack.verify().unwrap_err(),
        pack.verify_with(&VerifyLimits::default()).unwrap_err(),
    ] {
        assert!(error.to_string().contains("pack size limit"), "{error}");
    }
}

#[test]
fn the_stored_projection_is_checked() {
    let isometric = "\"projection\":{\"kind\":\"isometric\",\"tile_width\":48,\"tile_height\":24}";
    for (name, projection) in [
        ("null.twpack", "\"projection\":null"),
        (
            "missing.twpack",
            "\"projection\":{\"kind\":\"isometric\",\"tile_width\":48}",
        ),
        (
            "zero.twpack",
            "\"projection\":{\"kind\":\"isometric\",\"tile_width\":0,\"tile_height\":24}",
        ),
        (
            "extra.twpack",
            "\"projection\":{\"kind\":\"top_down\",\"tile_size\":32,\"tile_width\":48}",
        ),
        (
            "kind.twpack",
            "\"projection\":{\"kind\":\"oblique\",\"tile_size\":32}",
        ),
    ] {
        let (_directory, copy) = tampered(
            name,
            &format!(
                "UPDATE meta SET value = replace(value, '{isometric}', '{projection}') WHERE key = 'profile'"
            ),
        );
        let error = Pack::open(&copy).unwrap().verify().unwrap_err();
        assert!(error.to_string().contains("projection"), "{name}: {error}");
    }
    let (_directory, copy) = tampered(
        "top-down.twpack",
        &format!(
            "UPDATE meta SET value = replace(value, '{isometric}', '\"projection\":{{\"kind\":\"top_down\",\"tile_size\":32}}') WHERE key = 'profile'"
        ),
    );
    Pack::open(&copy).unwrap().verify().unwrap();
}

#[cfg(unix)]
#[test]
fn paths_beginning_with_two_slashes_open() {
    let directory = tempfile::tempdir().unwrap();
    let written = write_pack(&world(), directory.path()).unwrap();
    let doubled = PathBuf::from(format!("/{}", written.path.display()));
    assert!(doubled.to_str().unwrap().starts_with("//"));
    let pack = Pack::open(&doubled).unwrap();
    assert_eq!(pack.verify().unwrap().chunks, 5);
}

#[test]
fn stairwells_layer_heights_and_list_fields_compile_and_read_back() {
    let world = world();
    let directory = tempfile::tempdir().unwrap();
    let written = write_pack(&world, directory.path()).unwrap();
    let pack = Pack::open(&written.path).unwrap();
    assert_eq!(pack.verify().unwrap().stairwells, 3);
    assert_eq!(pack.layer_height().unwrap(), Some(50));

    let overworld = &world.planes()["overworld"];
    for (id, stairwell) in &overworld.stairwells {
        assert_eq!(
            pack.stairwell("overworld", id).unwrap().as_ref(),
            Some(stairwell)
        );
    }
    assert!(pack.stairwell("overworld", "nowhere").unwrap().is_none());
    assert!(pack.stairwell("vault", "cliff.path").unwrap().is_none());
    let mut visited = Vec::new();
    pack.visit_stairwells("overworld", &mut |stairwell| {
        visited.push(stairwell.id);
        Ok(())
    })
    .unwrap();
    assert_eq!(visited, ["cave.chute", "cliff.path", "tower.stairs"]);
    pack.visit_stairwells("vault", &mut |_| panic!("the vault has none"))
        .unwrap();

    assert_eq!(
        pack.stairwell_step("overworld", 0, 25, 8).unwrap(),
        Some(super::StairwellTile {
            stairwell: "cliff.path".to_owned(),
            step: 1,
            proto: "stairs_down".to_owned(),
        })
    );
    assert!(
        pack.stairwell_step("overworld", 1, 25, 8)
            .unwrap()
            .is_none()
    );
    assert!(pack.stairwell_step("vault", 0, 25, 8).unwrap().is_none());

    // List and object field values are canonical JSON fields.
    let chunk = pack.chunk("overworld", 0, 0, 0).unwrap().unwrap();
    let wall = &chunk.objects[1];
    assert_eq!(
        pack.string(wall.fields[1].0).unwrap().as_deref(),
        Some("tags")
    );
    assert_eq!(
        wall.fields[1].1,
        FieldRecord::Json("[\"ruin\",\"north\"]".to_owned())
    );
}

#[test]
fn stairwell_rows_steps_and_documents_are_verified() {
    let chute = "WHERE stairwell = 'cave.chute' AND step = 0";
    let row = "a stairwell has an invalid ID";
    let step = "a stairwell step names an unknown";
    let disagrees = "disagrees with its rows";
    for (name, sql, problem) in [
        (
            "plane",
            "UPDATE stairwells SET plane = 'attic' WHERE id = 'cave.chute'".to_owned(),
            row,
        ),
        (
            "id",
            "UPDATE stairwells SET id = 'Chute' WHERE id = 'cave.chute'".to_owned(),
            row,
        ),
        (
            "step-proto",
            format!("UPDATE stairwell_steps SET proto = 'ladder' {chute}"),
            step,
        ),
        (
            "step-stairwell",
            format!("UPDATE stairwell_steps SET stairwell = 'nowhere' {chute}"),
            step,
        ),
        (
            "step-layer",
            format!("UPDATE stairwell_steps SET layer = 2147483648 {chute}"),
            step,
        ),
        (
            "step-x",
            format!("UPDATE stairwell_steps SET x = 9007199254740992 {chute}"),
            step,
        ),
        (
            "step-negative",
            format!("UPDATE stairwell_steps SET step = -1 {chute}"),
            step,
        ),
        (
            "step-huge",
            format!("UPDATE stairwell_steps SET step = 250000 {chute}"),
            step,
        ),
        (
            "step-index",
            format!("UPDATE stairwell_steps SET step = 1 {chute}"),
            disagrees,
        ),
        (
            "step-other-proto",
            format!("UPDATE stairwell_steps SET proto = 'wall' {chute}"),
            disagrees,
        ),
        (
            "step-moved",
            format!("UPDATE stairwell_steps SET x = 29 {chute}"),
            disagrees,
        ),
        (
            "step-missing",
            "DELETE FROM stairwell_steps WHERE stairwell = 'cave.chute' AND step = 1".to_owned(),
            disagrees,
        ),
        (
            "step-extra",
            "INSERT INTO stairwell_steps VALUES ('overworld', 5, 0, 0, 'cave.chute', 2, 'stairs_up')"
                .to_owned(),
            "no stairwell lists",
        ),
    ] {
        let (_directory, copy) = tampered(&format!("{name}.twpack"), &sql);
        let error = Pack::open(&copy).unwrap().verify().unwrap_err();
        assert!(error.to_string().contains(problem), "{name}: {error}");
    }
}

#[test]
fn stored_stairwell_documents_have_the_source_shape_and_agree_with_their_rows() {
    let disagrees = "disagrees with its rows";
    for (name, sql, problem) in [
        (
            "document-direction",
            "UPDATE stairwells SET document = replace(document, '\"direction\":\"down\"', '\"direction\":\"sideways\"') WHERE id = 'cave.chute'".to_owned(),
            "source format's shape",
        ),
        (
            "document-hidden",
            "UPDATE stairwells SET document = replace(document, '\"hidden_on_other_layers\":false', '\"hidden_on_other_layers\":0') WHERE id = 'cliff.path'".to_owned(),
            "source format's shape",
        ),
        (
            "document-id",
            "UPDATE stairwells SET document = replace(document, '\"id\":\"cave.chute\"', '\"id\":\"cave.slide\"') WHERE id = 'cave.chute'".to_owned(),
            disagrees,
        ),
        (
            "document-key",
            "UPDATE stairwells SET document = replace(document, '\"direction\":\"down\"', '\"direction\":\"down\",\"speed\":1') WHERE id = 'cave.chute'".to_owned(),
            "source format's shape",
        ),
        (
            "document-step-key",
            "UPDATE stairwells SET document = replace(document, '\"proto\":\"cave_floor\"', '\"proto\":\"cave_floor\",\"z\":1') WHERE id = 'cave.chute'".to_owned(),
            "source format's shape",
        ),
        (
            "document-proto",
            "UPDATE stairwells SET document = replace(document, '\"proto\":\"cave_floor\"', '\"proto\":\"Cave\"') WHERE id = 'cave.chute'".to_owned(),
            "source format's shape",
        ),
        (
            "document-spacing",
            "UPDATE stairwells SET document = replace(document, '\"id\":', '\"id\": ') WHERE id = 'cave.chute'".to_owned(),
            "canonical JSON",
        ),
        (
            // The document and its rows agree, but the step is two tiles
            // from the one before it.
            "document-gap",
            "UPDATE stairwells SET document = replace(document, '\"at\":[26,9]', '\"at\":[27,9]') WHERE id = 'cliff.path'; \
             UPDATE stairwell_steps SET x = 27 WHERE stairwell = 'cliff.path' AND step = 2"
                .to_owned(),
            "not on adjacent layers",
        ),
    ] {
        let (_directory, copy) = tampered(&format!("{name}.twpack"), &sql);
        let error = Pack::open(&copy).unwrap().verify().unwrap_err();
        assert!(error.to_string().contains(problem), "{name}: {error}");
    }

    // Single reads check what they load too.
    let (_directory, copy) = tampered(
        "read-id.twpack",
        "UPDATE stairwells SET document = replace(document, '\"id\":\"cave.chute\"', '\"id\":\"cave.slide\"') WHERE id = 'cave.chute'",
    );
    let pack = Pack::open(&copy).unwrap();
    assert!(pack.stairwell("overworld", "cave.chute").is_err());
    assert!(pack.visit_stairwells("overworld", &mut |_| Ok(())).is_err());
    // An ID longer than an identifier is not loaded.
    let (_directory, copy) = tampered(
        "read-long-id.twpack",
        "UPDATE stairwells SET id = id || replace(hex(zeroblob(150)), '00', 'a') WHERE id = 'cave.chute'",
    );
    let pack = Pack::open(&copy).unwrap();
    let long = format!("cave.chute{}", "a".repeat(150));
    let error = pack.stairwell("overworld", &long).unwrap_err();
    assert!(
        error.to_string().contains("disagrees with its row"),
        "{error}"
    );
    assert!(pack.visit_stairwells("overworld", &mut |_| Ok(())).is_err());
    let (_directory, copy) = tampered(
        "read-step.twpack",
        "UPDATE stairwell_steps SET proto = 'Not a proto' WHERE stairwell = 'cave.chute' AND step = 0",
    );
    let pack = Pack::open(&copy).unwrap();
    assert!(pack.stairwell_step("overworld", 0, 28, 28).is_err());
    let (_directory, copy) = tampered(
        "read-step-index.twpack",
        "UPDATE stairwell_steps SET step = 9223372036854775807 WHERE stairwell = 'cave.chute' AND step = 0",
    );
    let pack = Pack::open(&copy).unwrap();
    assert!(pack.stairwell_step("overworld", 0, 28, 28).is_err());
    assert!(
        pack.stairwell_step("overworld", 0, 20, 20)
            .unwrap()
            .is_some()
    );
}

#[test]
fn stored_layer_heights_are_checked() {
    for (name, height) in [
        ("zero.twpack", "0"),
        ("high.twpack", "65536"),
        ("text.twpack", "\"50\""),
    ] {
        let (_directory, copy) = tampered(
            name,
            &format!(
                "UPDATE meta SET value = replace(value, '\"layer_height\":50', '\"layer_height\":{height}') WHERE key = 'profile'"
            ),
        );
        let pack = Pack::open(&copy).unwrap();
        let error = pack.verify().unwrap_err();
        assert!(
            error.to_string().contains("layer_height"),
            "{name}: {error}"
        );
        assert!(pack.layer_height().is_err(), "{name}");
    }
    // The whole stored profile is checked, not just its layer height.
    let (_directory, copy) = tampered(
        "bare.twpack",
        "UPDATE meta SET value = '{\"layer_height\":50}' WHERE key = 'profile'",
    );
    assert!(Pack::open(&copy).unwrap().layer_height().is_err());
    let (_directory, copy) = tampered(
        "flat.twpack",
        "UPDATE meta SET value = replace(value, ',\"layer_height\":50', '') WHERE key = 'profile'",
    );
    let pack = Pack::open(&copy).unwrap();
    pack.verify().unwrap();
    assert_eq!(pack.layer_height().unwrap(), None);
}

#[test]
fn compile_rejects_stairwell_steps_that_share_a_tile() {
    use super::compile::stairwell_tiles;
    let world = world();
    assert_eq!(
        stairwell_tiles(&world.planes()["overworld"]).unwrap().len(),
        8
    );
    let mut plane = world.planes()["overworld"].clone();
    plane.stairwells.get_mut("cave.chute").unwrap().steps[0].at = [20, 20];
    let error = stairwell_tiles(&plane).unwrap_err();
    assert!(
        error.to_string().contains("share layer 0 tile [20, 20]"),
        "{error}"
    );

    // The public model's IDs, prototypes and step counts are checked
    // before anything is kept.
    let mut plane = world.planes()["overworld"].clone();
    let chute = plane.stairwells.remove("cave.chute").unwrap();
    plane.stairwells.insert("X".repeat(1 << 20), chute.clone());
    assert!(stairwell_tiles(&plane).is_err());
    let mut plane = world.planes()["overworld"].clone();
    plane.stairwells.get_mut("cave.chute").unwrap().steps[1].proto = "Not a proto".to_owned();
    assert!(stairwell_tiles(&plane).is_err());
    let mut plane = world.planes()["overworld"].clone();
    let mut long = chute;
    long.steps = (0..1_000_001)
        .map(|layer| tilewright_source::StairwellStep {
            layer,
            at: [0, 0],
            proto: "stairs_up".to_owned(),
        })
        .collect();
    plane.stairwells.insert("cave.chute".to_owned(), long);
    let error = stairwell_tiles(&plane).unwrap_err();
    assert!(
        error.to_string().contains("more stairwell steps"),
        "{error}"
    );
    // A plane holds at most an array's worth of stairwells.
    let mut plane = world.planes()["overworld"].clone();
    let chute = plane.stairwells["cave.chute"].clone();
    plane.stairwells = (0..250_001)
        .map(|index| (format!("s{index}"), chute.clone()))
        .collect();
    let error = stairwell_tiles(&plane).unwrap_err();
    assert!(error.to_string().contains("more stairwells"), "{error}");
}

#[test]
fn verification_caps_stairwells_per_plane() {
    // 250,001 two-step stairwells on distinct tiles.
    let rows = "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 250001) ";
    let (_directory, copy) = tampered(
        "many.twpack",
        &format!(
            "DELETE FROM stairwell_steps; DELETE FROM stairwells; \
             {rows}INSERT INTO stairwells SELECT 'overworld', 's' || i, \
             '{{\"id\":\"s' || i || '\",\"steps\":[{{\"layer\":0,\"at\":[' || i || ',0],\"proto\":\"stairs_up\"}},{{\"layer\":1,\"at\":[' || i || ',0],\"proto\":\"stairs_up\"}}]}}' FROM n; \
             {rows}INSERT INTO stairwell_steps SELECT 'overworld', 0, i, 0, 's' || i, 0, 'stairs_up' FROM n; \
             {rows}INSERT INTO stairwell_steps SELECT 'overworld', 1, i, 0, 's' || i, 1, 'stairs_up' FROM n;"
        ),
    );
    let error = Pack::open(&copy).unwrap().verify().unwrap_err();
    assert!(error.to_string().contains("more stairwells"), "{error}");
}

#[test]
fn verification_caps_zones_per_plane() {
    // 250,001 root zones in one plane: more than its one source document
    // holds, so the plane's parent chains are not followed.
    let (_directory, copy) = tampered(
        "zones.twpack",
        "DELETE FROM zones WHERE plane = 'vault'; \
         WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 250001) \
         INSERT INTO zones SELECT 'vault', 'z' || i, NULL, '{\"id\":\"z' || i || '\"}' FROM n;",
    );
    let error = Pack::open(&copy).unwrap().verify().unwrap_err();
    assert_eq!(error.code(), super::ErrorCode::Zone);
    assert!(error.to_string().contains("more zones"), "{error}");
}

/// The uids of the fixture, with the plane, layer, chunk position and
/// depth-first index of each object, in uid order.
const FIXTURE_UIDS: [(&str, &str, i32, i64, i64, i64); 5] = [
    ("o.k3vq7zr2mxa5d6wfh4pnyt2cgu", "overworld", 0, 0, 0, 7),
    ("ruin.north_wall", "overworld", 0, 0, 0, 1),
    ("vault.chest", "vault", 0, 0, 0, 0),
    ("village.chest", "overworld", 0, 0, 0, 4),
    ("village.chest.small", "overworld", 0, 0, 0, 6),
];

#[test]
fn object_uids_compile_into_records_and_an_index() {
    let world = world();
    let first = compile(&world).unwrap();
    assert_eq!(
        compile(&world).unwrap(),
        first,
        "compilation is deterministic"
    );
    let directory = tempfile::tempdir().unwrap();
    let written = write_pack(&world, directory.path()).unwrap();
    let pack = Pack::open(&written.path).unwrap();
    assert_eq!(pack.verify().unwrap().object_uids, 5);
    let connection =
        Connection::open_with_flags(&written.path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let mut statement = connection
        .prepare("SELECT uid, plane, layer, cx, cy, object FROM object_uids")
        .unwrap();
    let rows: Vec<(String, String, i32, i64, i64, i64)> = statement
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let expected: Vec<_> = FIXTURE_UIDS
        .iter()
        .map(|(uid, plane, layer, cx, cy, index)| {
            (
                (*uid).to_owned(),
                (*plane).to_owned(),
                *layer,
                *cx,
                *cy,
                *index,
            )
        })
        .collect();
    assert_eq!(rows, expected);

    // Each uid reads back as the source object it names.
    for (uid, source) in world.object_uids() {
        let (location, record) = pack.object(uid).unwrap().unwrap();
        assert_eq!(pack.object_location(uid).unwrap(), Some(location.clone()));
        assert_eq!(
            (
                location.plane.as_str(),
                (location.layer, location.cx, location.cy)
            ),
            (source.plane.as_str(), source.chunk)
        );
        assert_eq!(record.uid.as_deref(), Some(uid.as_str()));
        let proto = pack.string(record.proto).unwrap().unwrap();
        assert_eq!(proto, world.object(uid).unwrap().proto);
        assert_eq!(
            record.inventory.len(),
            world.object(uid).unwrap().inventory.len()
        );
    }
    assert!(pack.object("nothing").unwrap().is_none());
    assert!(pack.object_location("Not an identifier").unwrap().is_none());

    // Decoded chunks keep uids at every depth, by depth-first index.
    let chunk = pack.chunk("overworld", 0, 0, 0).unwrap().unwrap();
    let uids: Vec<Option<&str>> = (0..9)
        .map(|index| chunk.object(index).unwrap().uid.as_deref())
        .collect();
    assert_eq!(
        uids,
        [
            None,
            Some("ruin.north_wall"),
            None,
            None,
            Some("village.chest"),
            None,
            Some("village.chest.small"),
            Some("o.k3vq7zr2mxa5d6wfh4pnyt2cgu"),
            None
        ]
    );
    assert!(chunk.object(9).is_none());
}

#[test]
fn record_uids_are_identifiers_of_at_most_128_bytes() {
    let mut chunk = small_chunk();
    chunk.objects[0].uid = Some(format!("a{}", "0".repeat(127)));
    let record = encode_record(&chunk).unwrap();
    assert_eq!(decode_record(&record).unwrap(), chunk);
    for bad in [
        String::new(),
        "Chest".to_owned(),
        "a b".to_owned(),
        "a".repeat(129),
    ] {
        chunk.objects[0].uid = Some(bad.clone());
        let error = encode_record(&chunk).unwrap_err();
        assert!(error.to_string().contains("not an identifier"), "{bad}");
    }

    // The inventory entry's uid, after its length byte.
    let record = encode_record(&small_chunk()).unwrap();
    let at = record
        .windows(10)
        .position(|window| window == b"chest.coin")
        .unwrap();
    assert_eq!(record[at - 1], 10);
    for (offset, byte) in [(0, b'C'), (3, b' '), (9, 0), (9, 0xFF)] {
        let mut patched = record.clone();
        patched[at + offset] = byte;
        let error = decode_record(&patched).unwrap_err();
        assert!(error.to_string().contains("not an identifier"), "{byte}");
    }
    let mut empty = record.clone();
    empty[at - 1] = 0;
    assert!(decode_record(&empty).is_err());
    let mut long = record;
    long[at - 1] = 0xFF;
    assert!(decode_record(&long).is_err());
}

#[test]
fn verification_checks_object_uids_against_the_records() {
    for (name, sql, problem) in [
        (
            "index.twpack",
            "UPDATE object_uids SET object = 5 WHERE uid = 'village.chest'",
            "does not name its object",
        ),
        (
            "plane.twpack",
            "UPDATE object_uids SET plane = 'vault' WHERE uid = 'village.chest'",
            "does not name its object",
        ),
        (
            "layer.twpack",
            "UPDATE object_uids SET layer = 1 WHERE uid = 'village.chest'",
            "does not name its object",
        ),
        (
            "missing.twpack",
            "DELETE FROM object_uids WHERE uid = 'vault.chest'",
            "does not name its object",
        ),
        (
            "extra.twpack",
            "INSERT INTO object_uids VALUES ('zz.extra', 'overworld', 0, 0, 0, 0)",
            "no object has",
        ),
        (
            "chunk.twpack",
            "UPDATE object_uids SET cx = 5 WHERE uid = 'village.chest'",
            "names a missing chunk",
        ),
        (
            "syntax.twpack",
            "UPDATE object_uids SET uid = 'Village.chest' WHERE uid = 'village.chest'",
            "invalid uid",
        ),
        (
            "nul.twpack",
            "UPDATE object_uids SET uid = 'village.chest' || char(0) WHERE uid = 'village.chest'",
            "invalid uid",
        ),
        (
            "blob.twpack",
            "UPDATE object_uids SET uid = CAST(uid AS BLOB) WHERE uid = 'village.chest'",
            "invalid uid",
        ),
        (
            "negative.twpack",
            "UPDATE object_uids SET object = -1 WHERE uid = 'village.chest'",
            "invalid uid or index",
        ),
        (
            "real.twpack",
            "UPDATE object_uids SET object = 4.5 WHERE uid = 'village.chest'",
            "invalid uid or index",
        ),
        (
            "large.twpack",
            "UPDATE object_uids SET object = 16777216 WHERE uid = 'village.chest'",
            "invalid uid or index",
        ),
    ] {
        let (_directory, copy) = tampered(name, sql);
        let error = Pack::open(&copy).unwrap().verify().unwrap_err();
        assert!(error.to_string().contains(problem), "{name}: {error}");
        assert_eq!(error.code(), super::ErrorCode::ObjectUid, "{name}");
    }

    // A uid used twice in the records: across planes, and in one chunk.
    let error = tampered_vault("twice.twpack", |chunk, _| {
        chunk.objects[0].uid = Some("village.chest".to_owned());
    });
    assert!(error.contains("does not name its object"), "{error}");
    let error = tampered_vault("same.twpack", |chunk, _| {
        let mut copy = chunk.objects[0].clone();
        copy.tile = Some(0);
        chunk.objects.push(copy);
    });
    assert!(error.contains("does not name its object"), "{error}");
    // A uid dropped from a record leaves its row unused.
    let error = tampered_vault("dropped.twpack", |chunk, _| {
        chunk.objects[0].uid = None;
    });
    assert!(error.contains("no object has"), "{error}");
}

#[test]
fn single_uid_reads_check_bounds_and_agreement() {
    for (name, sql) in [
        (
            "plane.twpack",
            "UPDATE object_uids SET plane = 'Overworld' WHERE uid = 'village.chest'",
        ),
        (
            "layer.twpack",
            "UPDATE object_uids SET layer = 2147483648 WHERE uid = 'village.chest'",
        ),
        (
            "cx.twpack",
            "UPDATE object_uids SET cx = 281474976710656 WHERE uid = 'village.chest'",
        ),
        (
            "cy.twpack",
            "UPDATE object_uids SET cy = 'north' WHERE uid = 'village.chest'",
        ),
        (
            "index.twpack",
            "UPDATE object_uids SET object = 16777216 WHERE uid = 'village.chest'",
        ),
    ] {
        let (_directory, copy) = tampered(name, sql);
        let pack = Pack::open(&copy).unwrap();
        let error = pack.object_location("village.chest").unwrap_err();
        assert!(
            error.to_string().contains("out of range"),
            "{name}: {error}"
        );
        assert_eq!(error.code(), super::ErrorCode::ObjectUid, "{name}");
        assert!(pack.object("village.chest").is_err(), "{name}");
        // Other rows still read.
        assert!(pack.object("vault.chest").unwrap().is_some(), "{name}");
    }
    for (name, sql) in [
        (
            "other.twpack",
            "UPDATE object_uids SET object = 5 WHERE uid = 'village.chest'",
        ),
        (
            "past.twpack",
            "UPDATE object_uids SET object = 9 WHERE uid = 'village.chest'",
        ),
        (
            "chunk.twpack",
            "UPDATE object_uids SET cx = 3 WHERE uid = 'village.chest'",
        ),
    ] {
        let (_directory, copy) = tampered(name, sql);
        let pack = Pack::open(&copy).unwrap();
        assert!(pack.object_location("village.chest").unwrap().is_some());
        let error = pack.object("village.chest").unwrap_err();
        assert!(
            error.to_string().contains("does not name an object"),
            "{name}: {error}"
        );
        assert_eq!(error.code(), super::ErrorCode::ObjectUid, "{name}");
    }
}

#[test]
fn compile_rejects_object_uids_used_twice_or_malformed() {
    use super::compile::{Strings, UidRows, collect_uids, object_record, render_layers};
    let world = world();
    let chunk = vault_chunk(&world);
    let mut rows = UidRows::new();
    collect_uids(&mut rows, "vault", (0, 0, 0), &chunk).unwrap();
    assert_eq!(rows["vault.chest"], ("vault", 0, 0, 0, 0));
    let error = collect_uids(&mut rows, "vault", (0, 0, 1), &chunk).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("two objects have uid vault.chest"),
        "{error}"
    );
    let mut nested = chunk.clone();
    let mut item = nested.objects[0].clone();
    item.at = None;
    nested.objects[0].inventory = vec![item];
    let error = collect_uids(&mut UidRows::new(), "vault", (0, 0, 0), &nested).unwrap_err();
    assert!(error.to_string().contains("two objects"), "{error}");

    let mut bad = chunk;
    bad.objects[0].uid = Some("Vault".to_owned());
    let error = collect_uids(&mut UidRows::new(), "vault", (0, 0, 0), &bad).unwrap_err();
    assert!(error.to_string().contains("not an identifier"), "{error}");
    let strings = Strings::collect(&world).unwrap();
    let layers = render_layers(world.profile()).unwrap();
    let error = object_record(
        &bad.objects[0],
        &layers,
        &strings,
        0,
        &mut usize::MAX.clone(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("not an identifier"), "{error}");
}
