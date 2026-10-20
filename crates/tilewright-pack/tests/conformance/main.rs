// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! The shared reader conformance fixtures in `fixtures/packs`: they are
//! current with their generator, and the Rust reader treats each as the
//! manifest says. The Go reader runs the same manifest.
//!
//! Regenerate them with
//! `TILEWRIGHT_UPDATE_PACK_FIXTURES=1 cargo test --locked -p tilewright-pack --test conformance fixtures_are_current`.

mod generate;
mod json;
mod render;

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    path::{Path, PathBuf},
};
use tilewright_jsonc::{Document, Limits, Value};
use tilewright_pack::{Error, ErrorCode, Pack, Page, VerifyLimits};

const UPDATE: &str = "TILEWRIGHT_UPDATE_PACK_FIXTURES";

fn repository() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixtures() -> PathBuf {
    repository().join("fixtures/packs")
}

/// The generated files of a fixture directory, by name: every file but
/// the README.
fn generated(directory: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    if let Ok(entries) = std::fs::read_dir(directory) {
        for entry in entries {
            let entry = entry.unwrap();
            let name = entry.file_name().into_string().unwrap();
            if name != "README.md" {
                files.insert(name, std::fs::read(entry.path()).unwrap());
            }
        }
    }
    files
}

#[test]
fn fixtures_are_current() {
    let directory = tempfile::tempdir().unwrap();
    let manifest = generate::generate(directory.path(), &repository().join("fixtures/basic"));
    std::fs::write(directory.path().join("manifest.json"), manifest).unwrap();
    let expected = generated(directory.path());
    let target = fixtures();
    if std::env::var_os(UPDATE).is_some() {
        std::fs::create_dir_all(&target).unwrap();
        for name in generated(&target).keys() {
            std::fs::remove_file(target.join(name)).unwrap();
        }
        for (name, data) in &expected {
            std::fs::write(target.join(name), data).unwrap();
        }
        return;
    }
    let committed = generated(&target);
    let stale: Vec<&String> = expected
        .keys()
        .chain(committed.keys())
        .filter(|name| expected.get(*name) != committed.get(*name))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    assert!(
        stale.is_empty(),
        "fixtures/packs is not current: {stale:?} differ. Regenerate with \
         `{UPDATE}=1 cargo test --locked -p tilewright-pack --test conformance fixtures_are_current`."
    );
}

fn text<'v>(value: &'v Value, key: &str) -> Option<&'v str> {
    value.get(key).and_then(Value::as_str)
}

fn integer(value: &Value, key: &str) -> i64 {
    value
        .get(key)
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("{key} is an integer"))
}

fn unsigned(value: &Value, key: &str) -> Option<u64> {
    value
        .get(key)
        .map(|value| u64::try_from(value.as_i64().unwrap()).unwrap())
}

fn manifest() -> Document {
    let text = std::fs::read_to_string(fixtures().join("manifest.json")).unwrap();
    Document::parse(text, &Limits::default()).unwrap()
}

/// The code of a result, or `None` when it succeeded.
fn outcome<T>(result: Result<T, Error>) -> Option<&'static str> {
    result.err().map(|error| error.code().as_str())
}

/// Runs a single read and returns its code, or `None` when it succeeds.
fn read(pack: &Pack, op: &Value) -> Option<&'static str> {
    let plane = || text(op, "plane").unwrap();
    let id = || text(op, "id").unwrap();
    let layer = || i32::try_from(integer(op, "layer")).unwrap();
    match text(op, "op").unwrap() {
        "chunk" => outcome(pack.chunk(plane(), layer(), integer(op, "cx"), integer(op, "cy"))),
        "string" => outcome(pack.string(u32::try_from(integer(op, "id")).unwrap())),
        "stairwell" => outcome(pack.stairwell(plane(), id())),
        "stairwell_step" => {
            outcome(pack.stairwell_step(plane(), layer(), integer(op, "x"), integer(op, "y")))
        }
        // Reporting the layer height checks the whole stored profile.
        "layer_height" => outcome(pack.layer_height()),
        "profile" => outcome(pack.profile()),
        "object_location" => outcome(pack.object_location(text(op, "uid").unwrap())),
        "object" => outcome(pack.object(text(op, "uid").unwrap())),
        "plane" => outcome(pack.plane(id())),
        "zone" => outcome(pack.zone(plane(), id())),
        "anchor" => outcome(pack.anchor(plane(), id())),
        "definition" => outcome(pack.definition(text(op, "type").unwrap(), id())),
        other => panic!("unknown read {other}"),
    }
}

/// How a conformance pack is opened: from its file, or from its bytes
/// with the digest its name gives, as the WebAssembly reader opens packs.
#[derive(Clone, Copy, Debug)]
enum Source {
    File,
    Image,
}

/// The digest a file name gives, when it is 64 lowercase hex digits.
fn named_digest(file: &str) -> Option<[u8; 32]> {
    let stem = file.strip_suffix(".twpack")?;
    if stem.len() != 64
        || !stem
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    let mut digest = [0; 32];
    for (byte, pair) in digest.iter_mut().zip(stem.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
    }
    Some(digest)
}

fn open(file: &str, limits: &VerifyLimits, source: Source) -> Result<Pack, Error> {
    let path = fixtures().join(file);
    match source {
        Source::File => Pack::open_with(&path, limits),
        Source::Image => {
            Pack::open_bytes(&std::fs::read(path).unwrap(), named_digest(file), limits)
        }
    }
}

#[test]
fn the_manifest_lists_every_reader_code() {
    let manifest = manifest();
    let listed: Vec<&str> = manifest
        .root()
        .get("codes")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .map(|code| code.as_str().unwrap())
        .collect();
    let codes: Vec<&str> = ErrorCode::ALL
        .iter()
        .filter(|code| **code != ErrorCode::Invalid)
        .map(|code| code.as_str())
        .collect();
    assert_eq!(listed, codes);
}

#[test]
fn the_rust_reader_conforms() {
    for source in [Source::File, Source::Image] {
        conforms(source);
    }
}

fn conforms(source: Source) {
    let manifest = manifest();
    let root = manifest.root();
    assert_eq!(text(root, "format"), Some(generate::MANIFEST_FORMAT));
    let mut failures = Vec::new();
    for case in root.get("cases").and_then(Value::as_array).unwrap() {
        let file = text(case, "file").unwrap();
        let mut check = |what: String, found: Option<&str>, expected: Option<&str>| {
            if found != expected {
                failures.push(format!(
                    "{file} ({source:?}): {what}: {found:?}, expected {expected:?}"
                ));
            }
        };
        let mut limits = VerifyLimits::default();
        if let Some(maximum) = case
            .get("open_limits")
            .and_then(|limits| unsigned(limits, "max_pack_bytes"))
        {
            limits.maximum_pack_bytes = maximum;
        }
        let pack = match open(file, &limits, source) {
            Ok(pack) => pack,
            Err(error) => {
                check(
                    format!("open ({error})"),
                    Some(error.code().as_str()),
                    text(case, "open"),
                );
                continue;
            }
        };
        check("open".into(), None, text(case, "open"));
        for op in case
            .get("reads")
            .and_then(Value::as_array)
            .unwrap_or_default()
        {
            check(manifest.canonical(op), read(&pack, op), text(op, "error"));
        }
        let mut limits = VerifyLimits::default();
        if let Some(verify) = case.get("verify_limits") {
            if let Some(maximum) = unsigned(verify, "max_pack_bytes") {
                limits.maximum_pack_bytes = maximum;
            }
            if let Some(maximum) = unsigned(verify, "max_decoded_bytes") {
                limits.maximum_decoded_bytes = maximum;
            }
        }
        let error = pack.verify_with(&limits).err();
        check(
            format!("verify ({error:?})"),
            error.as_ref().map(|error| error.code().as_str()),
            text(case, "verify"),
        );
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
#[allow(clippy::too_many_lines)]
fn the_valid_packs_read_back_as_the_manifest_lists() {
    let manifest = manifest();
    for valid in manifest
        .root()
        .get("valid")
        .and_then(Value::as_array)
        .unwrap()
    {
        let file = text(valid, "file").unwrap();
        let pack = open(file, &VerifyLimits::default(), Source::Image).unwrap();
        assert_eq!(pack.file_digest().unwrap(), named_digest(file).unwrap());
        let summary = pack.verify().unwrap();
        assert_eq!(
            summary,
            Pack::open(&fixtures().join(file))
                .unwrap()
                .verify()
                .unwrap()
        );
        let counts = valid.get("summary").unwrap();
        for (key, count) in [
            ("strings", summary.strings),
            ("planes", summary.planes),
            ("zones", summary.zones),
            ("anchors", summary.anchors),
            ("stairwells", summary.stairwells),
            ("chunks", summary.chunks),
            ("definitions", summary.definitions),
            ("objects", summary.objects),
            ("object_uids", summary.object_uids),
        ] {
            assert_eq!(unsigned(counts, key), Some(count), "{file}: {key}");
        }
        for member in valid.get("meta").and_then(Value::as_object).unwrap() {
            assert_eq!(
                pack.meta(&member.key).unwrap().as_deref(),
                member.value.as_str(),
                "{file}: {}",
                member.key
            );
        }
        assert_eq!(
            pack.layer_height().unwrap().map(i64::from),
            valid.get("layer_height").and_then(Value::as_i64)
        );
        rows_read_back(&pack, valid);
        let strings = valid.get("strings").and_then(Value::as_array).unwrap();
        for (id, value) in (1..).zip(strings) {
            assert_eq!(pack.string(id).unwrap().as_deref(), value.as_str());
        }
        let past = u32::try_from(strings.len() + 1).unwrap();
        assert_eq!(pack.string(past).unwrap(), None);
        for stairwell in valid.get("stairwells").and_then(Value::as_array).unwrap() {
            let read = pack
                .stairwell(
                    text(stairwell, "plane").unwrap(),
                    text(stairwell, "id").unwrap(),
                )
                .unwrap()
                .unwrap();
            assert_eq!(Some(read.canonical.as_str()), text(stairwell, "document"));
        }
        for step in valid.get("steps").and_then(Value::as_array).unwrap() {
            let read = pack
                .stairwell_step(
                    text(step, "plane").unwrap(),
                    i32::try_from(integer(step, "layer")).unwrap(),
                    integer(step, "x"),
                    integer(step, "y"),
                )
                .unwrap()
                .unwrap();
            assert_eq!(Some(read.stairwell.as_str()), text(step, "stairwell"));
            assert_eq!(
                i64::try_from(read.step).ok(),
                step.get("step").and_then(Value::as_i64)
            );
            assert_eq!(Some(read.proto.as_str()), text(step, "proto"));
        }
        for row in valid.get("object_uids").and_then(Value::as_array).unwrap() {
            let uid = text(row, "uid").unwrap();
            let (location, object) = pack.object(uid).unwrap().unwrap();
            assert_eq!(
                (
                    Some(location.plane.as_str()),
                    i64::from(location.layer),
                    location.cx,
                    location.cy,
                    i64::try_from(location.index).unwrap()
                ),
                (
                    text(row, "plane"),
                    integer(row, "layer"),
                    integer(row, "cx"),
                    integer(row, "cy"),
                    integer(row, "object")
                ),
                "{file}: {uid}"
            );
            assert_eq!(object.uid.as_deref(), Some(uid));
        }
        for chunk in valid.get("chunks").and_then(Value::as_array).unwrap() {
            let read = pack
                .chunk(
                    text(chunk, "plane").unwrap(),
                    i32::try_from(integer(chunk, "layer")).unwrap(),
                    integer(chunk, "cx"),
                    integer(chunk, "cy"),
                )
                .unwrap()
                .unwrap();
            let palette: Vec<Option<String>> = read
                .palette
                .iter()
                .map(|id| pack.string(*id).unwrap())
                .collect();
            let expected: Vec<Option<String>> = chunk
                .get("palette")
                .and_then(Value::as_array)
                .unwrap()
                .iter()
                .map(|value| value.as_str().map(str::to_owned))
                .collect();
            assert_eq!(palette, expected);
            assert_eq!(
                unsigned(chunk, "objects"),
                Some(render::count(&read.objects))
            );
            let digest = sha2::Digest::finalize(sha2::Digest::chain_update(
                sha2::Sha256::default(),
                render::render(&read).as_bytes(),
            ));
            let digest = digest.iter().fold(String::new(), |mut output, byte| {
                let _ = write!(output, "{byte:02x}");
                output
            });
            assert_eq!(Some(digest.as_str()), text(chunk, "render_sha256"));
        }
    }
}

/// Every value a visit yields.
fn collect<T>(
    visit: impl FnOnce(&mut dyn FnMut(T) -> Result<(), Error>) -> Result<(), Error>,
) -> Vec<T> {
    let mut values = Vec::new();
    visit(&mut |value| {
        values.push(value);
        Ok(())
    })
    .unwrap();
    values
}

/// The planes, zones, anchors, layers and chunk positions of a valid pack
/// read back, one at a time and listed, as the manifest lists them.
fn rows_read_back(pack: &Pack, valid: &Value) {
    let listed = |key: &str| valid.get(key).and_then(Value::as_array).unwrap();
    let planes = collect(|visit| pack.visit_planes(Page::all(), visit));
    // Walking a page of one row at a time visits the same rows.
    let mut paged = Vec::new();
    loop {
        let after = paged
            .last()
            .map(|plane: &tilewright_pack::Plane| plane.id.clone());
        let page = collect(|visit| {
            pack.visit_planes(
                Page {
                    after: after.as_deref(),
                    limit: 1,
                },
                visit,
            )
        });
        if page.is_empty() {
            break;
        }
        paged.extend(page);
    }
    assert_eq!(paged, planes);
    assert_eq!(planes.len(), listed("planes").len());
    for (plane, expected) in planes.iter().zip(listed("planes")) {
        assert_eq!(Some(plane.id.as_str()), text(expected, "id"));
        assert_eq!(Some(plane.kind.as_str()), text(expected, "kind"));
        assert_eq!(i64::from(plane.chunk_size), integer(expected, "chunk_size"));
        assert_eq!(Some(plane.document.as_str()), text(expected, "document"));
        assert_eq!(pack.plane(&plane.id).unwrap().as_ref(), Some(plane));
    }
    let mut zones = Vec::new();
    let mut anchors = Vec::new();
    let mut chunks = Vec::new();
    for plane in &planes {
        zones.extend(collect(|visit| {
            pack.visit_zones(&plane.id, Page::all(), visit)
        }));
        anchors.extend(collect(|visit| {
            pack.visit_anchors(&plane.id, Page::all(), visit)
        }));
        for layer in collect(|visit| pack.visit_layers(&plane.id, Page::all(), visit)) {
            for (cx, cy) in collect(|visit| {
                pack.visit_chunk_keys(&plane.id, layer, Page::all(), &mut |cx, cy| visit((cx, cy)))
            }) {
                chunks.push((plane.id.clone(), i64::from(layer), cx, cy));
            }
        }
    }
    assert_eq!(zones.len(), listed("zones").len());
    for (zone, expected) in zones.iter().zip(listed("zones")) {
        assert_eq!(Some(zone.plane.as_str()), text(expected, "plane"));
        assert_eq!(Some(zone.id.as_str()), text(expected, "id"));
        assert_eq!(zone.parent.as_deref(), text(expected, "parent"));
        assert_eq!(Some(zone.document.as_str()), text(expected, "document"));
        assert_eq!(
            pack.zone(&zone.plane, &zone.id).unwrap().as_ref(),
            Some(zone)
        );
    }
    assert_eq!(anchors.len(), listed("anchors").len());
    for (anchor, expected) in anchors.iter().zip(listed("anchors")) {
        assert_eq!(
            (
                Some(anchor.plane.as_str()),
                Some(anchor.id.as_str()),
                i64::from(anchor.layer),
                anchor.x,
                anchor.y
            ),
            (
                text(expected, "plane"),
                text(expected, "id"),
                integer(expected, "layer"),
                integer(expected, "x"),
                integer(expected, "y")
            )
        );
        assert_eq!(
            pack.anchor(&anchor.plane, &anchor.id).unwrap().as_ref(),
            Some(anchor)
        );
    }
    let expected: Vec<(String, i64, i64, i64)> = listed("chunks")
        .iter()
        .map(|chunk| {
            (
                text(chunk, "plane").unwrap().to_owned(),
                integer(chunk, "layer"),
                integer(chunk, "cx"),
                integer(chunk, "cy"),
            )
        })
        .collect();
    assert_eq!(chunks, expected);
    assert_eq!(pack.plane("missing").unwrap(), None);
    definitions_read_back(pack, valid);
}

/// The definitions and profile of a valid pack read back as the manifest
/// lists them.
fn definitions_read_back(pack: &Pack, valid: &Value) {
    let listed = valid.get("definitions").and_then(Value::as_array).unwrap();
    let definitions = collect(|visit| pack.visit_definitions(None, Page::all(), visit));
    assert_eq!(definitions.len(), listed.len());
    for (definition, expected) in definitions.iter().zip(listed) {
        assert_eq!(Some(definition.kind.as_str()), text(expected, "type"));
        assert_eq!(Some(definition.id.as_str()), text(expected, "id"));
        assert_eq!(
            Some(definition.document.as_str()),
            text(expected, "document")
        );
        assert_eq!(
            pack.definition(&definition.kind, &definition.id)
                .unwrap()
                .as_ref(),
            Some(definition)
        );
        let of_type =
            collect(|visit| pack.visit_definitions(Some(&definition.kind), Page::all(), visit));
        assert!(of_type.iter().all(|listed| listed.kind == definition.kind));
        assert!(of_type.contains(definition));
    }
    assert_eq!(pack.definition("proto", "missing").unwrap(), None);
    assert_eq!(
        pack.profile().unwrap(),
        valid
            .get("meta")
            .and_then(|meta| text(meta, "profile"))
            .unwrap()
    );
}
