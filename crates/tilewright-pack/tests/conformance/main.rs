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
use tilewright_pack::{Error, ErrorCode, Pack, VerifyLimits};

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

/// Single reads the Rust reader has no method for.
const GO_ONLY_READS: [&str; 4] = ["plane", "zone", "anchor", "definition"];

/// Runs a single read and returns its code, or `None` when it succeeds.
fn read(pack: &Pack, op: &Value) -> Option<&'static str> {
    let plane = || text(op, "plane").unwrap();
    let layer = || i32::try_from(integer(op, "layer")).unwrap();
    match text(op, "op").unwrap() {
        "chunk" => outcome(pack.chunk(plane(), layer(), integer(op, "cx"), integer(op, "cy"))),
        "string" => outcome(pack.string(u32::try_from(integer(op, "id")).unwrap())),
        "stairwell" => outcome(pack.stairwell(plane(), text(op, "id").unwrap())),
        "stairwell_step" => {
            outcome(pack.stairwell_step(plane(), layer(), integer(op, "x"), integer(op, "y")))
        }
        // Reporting the layer height checks the whole stored profile.
        "layer_height" | "profile" => outcome(pack.layer_height()),
        other => panic!("unknown read {other}"),
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
    let manifest = manifest();
    let root = manifest.root();
    assert_eq!(text(root, "format"), Some(generate::MANIFEST_FORMAT));
    let mut failures = Vec::new();
    for case in root.get("cases").and_then(Value::as_array).unwrap() {
        let file = text(case, "file").unwrap();
        let mut check = |what: String, found: Option<&str>, expected: Option<&str>| {
            if found != expected {
                failures.push(format!("{file}: {what}: {found:?}, expected {expected:?}"));
            }
        };
        let mut limits = VerifyLimits::default();
        if let Some(maximum) = case
            .get("open_limits")
            .and_then(|limits| unsigned(limits, "max_pack_bytes"))
        {
            limits.maximum_pack_bytes = maximum;
        }
        let pack = match Pack::open_with(&fixtures().join(file), &limits) {
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
            if !GO_ONLY_READS.contains(&text(op, "op").unwrap()) {
                check(manifest.canonical(op), read(&pack, op), text(op, "error"));
            }
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
        let pack = Pack::open(&fixtures().join(file)).unwrap();
        let summary = pack.verify().unwrap();
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
