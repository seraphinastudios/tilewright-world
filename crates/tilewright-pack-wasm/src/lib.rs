// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! The Tilewright pack reader for WebAssembly: the `tilewright-pack` reader,
//! with its rules, bounds and error codes, behind a JavaScript interface.
//!
//! The ESM wrapper in `packages/tilewright-pack` is the public API; these
//! exports are its internals. The wrapper checks argument types and ranges
//! before calling them, since `wasm-bindgen` converts numbers silently.
//! Every value returned is a new JavaScript value: plain objects, arrays,
//! strings, numbers and typed arrays, with no reference into WebAssembly
//! memory.

use js_sys::{Array, Int32Array, Object, Reflect, Uint8Array, Uint16Array, Uint32Array};
use tilewright_pack::{
    Anchor, ChunkData, Definition, Error, FieldRecord, ObjectRecord, Pack, Page, Plane,
    PortalRecord, Summary, UidLocation, VerifyLimits, Zone,
};
use tilewright_source::Stairwell;
use wasm_bindgen::prelude::*;

/// The largest integer a JavaScript number holds exactly.
const MAX_SAFE_INTEGER: i64 = (1 << 53) - 1;

/// A JavaScript `Error` for a rejected pack: its `name` is `PackError` and
/// its `code` the stable code, such as `pack.chunk_record`.
fn rejected(error: &Error) -> JsValue {
    let thrown = js_sys::Error::new(&error.to_string());
    thrown.set_name("PackError");
    set(&thrown, "code", &error.code().as_str().into());
    thrown.into()
}

/// A JavaScript `TypeError` for an argument the reader cannot take.
fn bad_argument(message: &str) -> JsValue {
    js_sys::TypeError::new(message).into()
}

fn set(target: &JsValue, key: &str, value: &JsValue) {
    // Setting a property of a plain object or error cannot fail.
    let _ = Reflect::set(target, &JsValue::from_str(key), value);
}

/// A plain object with `entries`.
fn object(entries: &[(&str, JsValue)]) -> JsValue {
    let target: JsValue = Object::new().into();
    for (key, value) in entries {
        set(&target, key, value);
    }
    target
}

/// An integer as a number when it is exact as one, else as a `BigInt`.
fn integer(value: i64) -> JsValue {
    if (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&value) {
        #[expect(
            clippy::cast_precision_loss,
            reason = "safe integers are exact as numbers"
        )]
        JsValue::from_f64(value as f64)
    } else {
        JsValue::from(value)
    }
}

/// A count as a number; counts of an image in memory are far below 2^53.
fn count(value: u64) -> JsValue {
    integer(i64::try_from(value).unwrap_or(i64::MAX))
}

fn text(value: &str) -> JsValue {
    JsValue::from_str(value)
}

fn optional_text(value: Option<&str>) -> JsValue {
    value.map_or(JsValue::NULL, text)
}

fn summary(summary: &Summary) -> JsValue {
    object(&[
        ("strings", count(summary.strings)),
        ("planes", count(summary.planes)),
        ("zones", count(summary.zones)),
        ("anchors", count(summary.anchors)),
        ("stairwells", count(summary.stairwells)),
        ("chunks", count(summary.chunks)),
        ("definitions", count(summary.definitions)),
        ("objects", count(summary.objects)),
        ("objectUids", count(summary.object_uids)),
    ])
}

fn plane(plane: &Plane) -> JsValue {
    object(&[
        ("id", text(&plane.id)),
        ("kind", text(&plane.kind)),
        ("chunkSize", plane.chunk_size.into()),
        ("document", text(&plane.document)),
    ])
}

fn zone(zone: &Zone) -> JsValue {
    object(&[
        ("plane", text(&zone.plane)),
        ("id", text(&zone.id)),
        ("parent", optional_text(zone.parent.as_deref())),
        ("document", text(&zone.document)),
    ])
}

fn anchor(anchor: &Anchor) -> JsValue {
    object(&[
        ("plane", text(&anchor.plane)),
        ("id", text(&anchor.id)),
        ("layer", anchor.layer.into()),
        ("x", integer(anchor.x)),
        ("y", integer(anchor.y)),
    ])
}

fn definition(definition: &Definition) -> JsValue {
    object(&[
        ("type", text(&definition.kind)),
        ("id", text(&definition.id)),
        ("document", text(&definition.document)),
    ])
}

fn stairwell(plane: &str, stairwell: &Stairwell) -> JsValue {
    let steps: Array = stairwell
        .steps
        .iter()
        .map(|step| {
            object(&[
                ("layer", step.layer.into()),
                ("x", integer(step.at[0])),
                ("y", integer(step.at[1])),
                ("proto", text(&step.proto)),
            ])
        })
        .collect();
    object(&[
        ("plane", text(plane)),
        ("id", text(&stairwell.id)),
        ("steps", steps.into()),
        ("direction", text(stairwell.direction.as_str())),
        (
            "hiddenOnOtherLayers",
            stairwell.hidden_on_other_layers.into(),
        ),
        ("custom", optional_text(stairwell.custom.as_deref())),
        ("document", text(&stairwell.canonical)),
    ])
}

fn location(location: &UidLocation) -> JsValue {
    object(&[
        ("plane", text(&location.plane)),
        ("layer", location.layer.into()),
        ("cx", integer(location.cx)),
        ("cy", integer(location.cy)),
        (
            "index",
            count(u64::try_from(location.index).unwrap_or(u64::MAX)),
        ),
    ])
}

fn field(value: &FieldRecord) -> JsValue {
    let (kind, value) = match value {
        FieldRecord::Null => ("null", JsValue::NULL),
        FieldRecord::Bool(value) => ("bool", (*value).into()),
        FieldRecord::Integer(value) => ("integer", integer(*value)),
        FieldRecord::Number(value) => ("number", JsValue::from_f64(*value)),
        FieldRecord::String(id) => ("string", (*id).into()),
        FieldRecord::Json(json) => ("json", text(json)),
    };
    object(&[("type", text(kind)), ("value", value)])
}

fn portal(portal: Option<&PortalRecord>) -> JsValue {
    match portal {
        None => JsValue::NULL,
        Some(PortalRecord::Location { plane, layer, x, y }) => object(&[
            ("kind", text("location")),
            ("plane", (*plane).into()),
            ("layer", (*layer).into()),
            ("x", integer(*x)),
            ("y", integer(*y)),
        ]),
        Some(PortalRecord::Anchor { plane, anchor }) => object(&[
            ("kind", text("anchor")),
            ("plane", (*plane).into()),
            ("anchor", (*anchor).into()),
        ]),
    }
}

/// An object record and its inventory, which nests at most 16 deep.
fn object_record(record: &ObjectRecord) -> JsValue {
    let fields: Array = record
        .fields
        .iter()
        .map(|(name, value)| object(&[("name", (*name).into()), ("value", field(value))]))
        .collect();
    let inventory: Array = record.inventory.iter().map(object_record).collect();
    object(&[
        ("uid", optional_text(record.uid.as_deref())),
        ("tile", record.tile.map_or(JsValue::NULL, JsValue::from)),
        ("proto", record.proto.into()),
        ("layer", record.layer.into()),
        ("subLayer", record.sub_layer.into()),
        ("z", record.z.into()),
        ("portal", portal(record.portal.as_ref())),
        ("fields", fields.into()),
        ("custom", optional_text(record.custom.as_deref())),
        ("inventory", inventory.into()),
    ])
}

fn chunk(chunk: &ChunkData) -> JsValue {
    let ground: Array = chunk
        .ground
        .iter()
        .map(|grid| JsValue::from(Uint16Array::from(grid.as_slice())))
        .collect();
    let objects: Array = chunk.objects.iter().map(object_record).collect();
    object(&[
        ("chunkSize", chunk.chunk_size.into()),
        (
            "palette",
            Uint32Array::from(chunk.palette.as_slice()).into(),
        ),
        ("ground", ground.into()),
        ("heights", Int32Array::from(chunk.heights.as_slice()).into()),
        ("zones", Uint16Array::from(chunk.zones.as_slice()).into()),
        ("objects", objects.into()),
    ])
}

/// Parses an expected digest: 64 lowercase hex digits.
fn parse_digest(digest: &str) -> Option<[u8; 32]> {
    let bytes = digest.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let nibble = |byte: u8| match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    };
    let mut parsed = [0; 32];
    for (value, pair) in parsed.iter_mut().zip(bytes.chunks_exact(2)) {
        *value = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Some(parsed)
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|byte| {
            [
                char::from(DIGITS[usize::from(byte >> 4)]),
                char::from(DIGITS[usize::from(byte & 15)]),
            ]
        })
        .collect()
}

/// Reads a JavaScript byte array into the reader's copy, a block at a time,
/// so the image is copied once, straight into the memory SQLite reads.
struct Bytes {
    array: Uint8Array,
    offset: u32,
}

impl std::io::Read for Bytes {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let left = self.array.length().saturating_sub(self.offset);
        let count = u32::try_from(buffer.len()).unwrap_or(u32::MAX).min(left);
        let end = self.offset + count;
        let length = usize::try_from(count).unwrap_or(0);
        if let Some(target) = buffer.get_mut(..length) {
            self.array.subarray(self.offset, end).copy_to(target);
        }
        self.offset = end;
        Ok(length)
    }
}

/// An open pack. Its bytes are copied when it opens, so later changes to
/// the caller's buffer do not reach it.
#[wasm_bindgen]
pub struct Reader {
    pack: Pack,
}

type Found = Result<JsValue, JsValue>;

/// `found` as a JavaScript value, or `undefined` when there is none.
fn optional<T>(found: Result<Option<T>, Error>, convert: impl Fn(&T) -> JsValue) -> Found {
    match found {
        Ok(Some(value)) => Ok(convert(&value)),
        Ok(None) => Ok(JsValue::UNDEFINED),
        Err(error) => Err(rejected(&error)),
    }
}

/// The values `visit_rows` yields, as an array: one page of a listing, at
/// most the limit the caller gave.
fn listed<T>(
    visit_rows: impl FnOnce(&mut dyn FnMut(T) -> Result<(), Error>) -> Result<(), Error>,
    convert: impl Fn(&T) -> JsValue,
) -> Found {
    let rows = Array::new();
    visit_rows(&mut |row| {
        rows.push(&convert(&row));
        Ok(())
    })
    .map_err(|error| rejected(&error))?;
    Ok(rows.into())
}

/// A page of a listing: at most `limit` rows after `after`, stopping after
/// the row whose text reaches `bytes`.
fn page<K>(after: Option<K>, limit: u32, bytes: u32) -> Page<K> {
    Page {
        after,
        limit: usize::try_from(limit).unwrap_or(usize::MAX),
        bytes: usize::try_from(bytes).unwrap_or(usize::MAX),
    }
}

#[wasm_bindgen]
impl Reader {
    /// Opens a pack from its bytes; see `Pack::open_from`. `digest`, when
    /// given, is the SHA-256 verification compares the bytes with, as 64
    /// lowercase hex digits; `max_pack_bytes` is the pack size limit.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "wasm-bindgen passes optional strings by value"
    )]
    pub fn open(
        bytes: Uint8Array,
        digest: Option<String>,
        max_pack_bytes: Option<u64>,
    ) -> Result<Reader, JsValue> {
        let digest = match digest.as_deref() {
            None => None,
            Some(digest) => Some(parse_digest(digest).ok_or_else(|| {
                bad_argument("the expected digest is not 64 lowercase hex digits")
            })?),
        };
        let mut limits = VerifyLimits::default();
        if let Some(maximum) = max_pack_bytes {
            limits.maximum_pack_bytes = maximum;
        }
        let length = u64::from(bytes.length());
        let image = Bytes {
            array: bytes,
            offset: 0,
        };
        Pack::open_from(image, length, digest, &limits)
            .map(|pack| Reader { pack })
            .map_err(|error| rejected(&error))
    }

    /// Row counts; `objects` is counted by `verify` only.
    pub fn summary(&self) -> Found {
        self.pack
            .summary()
            .map(|counts| summary(&counts))
            .map_err(|error| rejected(&error))
    }

    pub fn meta(&self, key: &str) -> Found {
        optional(self.pack.meta(key), |value: &String| text(value))
    }

    pub fn profile(&self) -> Found {
        self.pack
            .profile()
            .map(|profile| text(&profile))
            .map_err(|error| rejected(&error))
    }

    #[wasm_bindgen(js_name = layerHeight)]
    pub fn layer_height(&self) -> Found {
        optional(self.pack.layer_height(), |height: &u32| (*height).into())
    }

    pub fn string(&self, id: u32) -> Found {
        optional(self.pack.string(id), |value: &String| text(value))
    }

    pub fn plane(&self, id: &str) -> Found {
        optional(self.pack.plane(id), plane)
    }

    /// A page of planes: at most `limit`, after the plane `after`.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "wasm-bindgen passes optional strings by value"
    )]
    pub fn planes(&self, after: Option<String>, limit: u32, bytes: u32) -> Found {
        listed(
            |visit| {
                self.pack
                    .visit_planes(page(after.as_deref(), limit, bytes), visit)
            },
            plane,
        )
    }

    pub fn zone(&self, plane: &str, id: &str) -> Found {
        optional(self.pack.zone(plane, id), zone)
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "wasm-bindgen passes optional strings by value"
    )]
    pub fn zones(&self, plane: &str, after: Option<String>, limit: u32, bytes: u32) -> Found {
        listed(
            |visit| {
                self.pack
                    .visit_zones(plane, page(after.as_deref(), limit, bytes), visit)
            },
            zone,
        )
    }

    pub fn anchor(&self, plane: &str, id: &str) -> Found {
        optional(self.pack.anchor(plane, id), anchor)
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "wasm-bindgen passes optional strings by value"
    )]
    pub fn anchors(&self, plane: &str, after: Option<String>, limit: u32, bytes: u32) -> Found {
        listed(
            |visit| {
                self.pack
                    .visit_anchors(plane, page(after.as_deref(), limit, bytes), visit)
            },
            anchor,
        )
    }

    pub fn stairwell(&self, plane: &str, id: &str) -> Found {
        optional(self.pack.stairwell(plane, id), |found| {
            stairwell(plane, found)
        })
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "wasm-bindgen passes optional strings by value"
    )]
    pub fn stairwells(&self, plane: &str, after: Option<String>, limit: u32, bytes: u32) -> Found {
        listed(
            |visit| {
                self.pack
                    .visit_stairwells(plane, page(after.as_deref(), limit, bytes), visit)
            },
            |found| stairwell(plane, found),
        )
    }

    #[wasm_bindgen(js_name = stairwellStep)]
    pub fn stairwell_step(&self, plane: &str, layer: i32, x: i64, y: i64) -> Found {
        optional(self.pack.stairwell_step(plane, layer, x, y), |step| {
            object(&[
                ("stairwell", text(&step.stairwell)),
                ("step", count(u64::try_from(step.step).unwrap_or(u64::MAX))),
                ("proto", text(&step.proto)),
            ])
        })
    }

    pub fn definition(&self, kind: &str, id: &str) -> Found {
        optional(self.pack.definition(kind, id), definition)
    }

    /// A page of definitions, of type `kind` or of every type, after the
    /// definition `after_type`/`after_id` when both are given.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "wasm-bindgen passes optional strings by value"
    )]
    pub fn definitions(
        &self,
        kind: Option<String>,
        after_type: Option<String>,
        after_id: Option<String>,
        limit: u32,
        bytes: u32,
    ) -> Found {
        let after = after_type.as_deref().zip(after_id.as_deref());
        listed(
            |visit| {
                self.pack
                    .visit_definitions(kind.as_deref(), page(after, limit, bytes), visit)
            },
            definition,
        )
    }

    pub fn layers(&self, plane: &str, after: Option<i32>, limit: u32, bytes: u32) -> Found {
        listed(
            |visit| {
                self.pack
                    .visit_layers(plane, page(after, limit, bytes), visit)
            },
            |layer: &i32| (*layer).into(),
        )
    }

    #[wasm_bindgen(js_name = chunkKeys)]
    pub fn chunk_keys(
        &self,
        plane: &str,
        layer: i32,
        after_x: Option<i64>,
        after_y: Option<i64>,
        limit: u32,
        bytes: u32,
    ) -> Found {
        let after = after_x.zip(after_y);
        listed(
            |visit| {
                self.pack.visit_chunk_keys(
                    plane,
                    layer,
                    page(after, limit, bytes),
                    &mut |cx, cy| visit((cx, cy)),
                )
            },
            |(cx, cy): &(i64, i64)| object(&[("cx", integer(*cx)), ("cy", integer(*cy))]),
        )
    }

    pub fn chunk(&self, plane: &str, layer: i32, cx: i64, cy: i64) -> Found {
        optional(self.pack.chunk(plane, layer, cx, cy), chunk)
    }

    #[wasm_bindgen(js_name = objectLocation)]
    pub fn object_location(&self, uid: &str) -> Found {
        optional(self.pack.object_location(uid), location)
    }

    pub fn object(&self, uid: &str) -> Found {
        optional(self.pack.object(uid), |(found, record)| {
            object(&[
                ("location", location(found)),
                ("object", object_record(record)),
            ])
        })
    }

    /// Verifies the whole pack; see `Pack::verify_with`. Limits not given
    /// are the defaults, and the pack size limit is at most the one the
    /// pack was opened with.
    pub fn verify(&self, max_pack_bytes: Option<u64>, max_decoded_bytes: Option<u64>) -> Found {
        let defaults = VerifyLimits::default();
        let limits = VerifyLimits {
            maximum_pack_bytes: max_pack_bytes.unwrap_or(u64::MAX),
            maximum_decoded_bytes: max_decoded_bytes.unwrap_or(defaults.maximum_decoded_bytes),
        };
        self.pack
            .verify_with(&limits)
            .map(|counts| summary(&counts))
            .map_err(|error| rejected(&error))
    }

    /// The SHA-256 of the pack's bytes, as 64 lowercase hex digits.
    pub fn digest(&self) -> Result<String, JsValue> {
        self.pack
            .file_digest()
            .map(|digest| hex(&digest))
            .map_err(|error| rejected(&error))
    }
}

#[cfg(test)]
mod tests {
    use super::{hex, parse_digest};

    #[test]
    fn digests_are_64_lowercase_hex_digits() {
        let text = "5830e26e449a1f0d0c786b32801b7ed68e351777db5f586532478cb4384844b2";
        let digest = parse_digest(text).unwrap();
        assert_eq!(digest[0], 0x58);
        assert_eq!(hex(&digest), text);
        assert_eq!(parse_digest(&text.to_uppercase()), None);
        assert_eq!(parse_digest(&text[1..]), None);
        assert_eq!(parse_digest(&format!("{text}0")), None);
        assert_eq!(parse_digest(&text.replace('5', "g")), None);
    }
}
