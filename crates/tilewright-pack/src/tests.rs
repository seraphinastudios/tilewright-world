// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

use super::{
    ChunkData, FieldRecord, ObjectRecord, Pack, PortalRecord, compile, decode_chunk, decode_record,
    encode_chunk, encode_record, write_pack,
};
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use std::path::Path;
use tilewright_source::World;

fn world() -> World {
    tilewright_source::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/basic"))
        .expect("the fixture is valid")
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
