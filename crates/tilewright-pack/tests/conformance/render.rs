// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! The text rendering of a decoded chunk whose SHA-256 the manifest records,
//! as `fixtures/packs/README.md` specifies it. Readers in other languages
//! render the same text, so equal digests mean equal decodes.

use std::fmt::Write as _;
use tilewright_pack::{ChunkData, FieldRecord, ObjectRecord, PortalRecord};

fn list<T: ToString>(values: &[T]) -> String {
    values
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn render(chunk: &ChunkData) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "chunk_size {}", chunk.chunk_size);
    let _ = writeln!(output, "palette {}", list(&chunk.palette));
    for grid in &chunk.ground {
        let _ = writeln!(output, "ground {}", list(grid));
    }
    let _ = writeln!(output, "heights {}", list(&chunk.heights));
    let _ = writeln!(output, "zones {}", list(&chunk.zones));
    for object in &chunk.objects {
        render_object(object, 0, &mut output);
    }
    output
}

fn render_object(object: &ObjectRecord, depth: usize, output: &mut String) {
    let _ = writeln!(
        output,
        "object {depth} {} {} {} {} {}",
        object.tile.unwrap_or(u16::MAX),
        object.proto,
        object.layer,
        object.sub_layer,
        object.z
    );
    if let Some(uid) = &object.uid {
        let _ = writeln!(output, "uid {uid}");
    }
    match &object.portal {
        Some(PortalRecord::Location { plane, layer, x, y }) => {
            let _ = writeln!(output, "portal location {plane} {layer} {x} {y}");
        }
        Some(PortalRecord::Anchor { plane, anchor }) => {
            let _ = writeln!(output, "portal anchor {plane} {anchor}");
        }
        None => {}
    }
    for (name, value) in &object.fields {
        let value = match value {
            FieldRecord::Null => "null".to_owned(),
            FieldRecord::Bool(value) => format!("bool {value}"),
            FieldRecord::Integer(value) => format!("integer {value}"),
            FieldRecord::Number(value) => format!("number {:016x}", value.to_bits()),
            FieldRecord::String(id) => format!("string {id}"),
            FieldRecord::Json(text) => format!("json {text}"),
        };
        let _ = writeln!(output, "field {name} {value}");
    }
    if let Some(custom) = &object.custom {
        let _ = writeln!(output, "custom {custom}");
    }
    for item in &object.inventory {
        render_object(item, depth + 1, output);
    }
}

/// Objects at any depth.
pub fn count(objects: &[ObjectRecord]) -> u64 {
    objects
        .iter()
        .map(|object| 1 + count(&object.inventory))
        .sum()
}
