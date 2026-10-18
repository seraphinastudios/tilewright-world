// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! The `twc1+zstd` chunk encoding (`docs/spec/PACK.md` section 2).
//!
//! Decoding treats its input as untrusted: sizes are bounded before any
//! allocation, every index is range-checked, and trailing bytes are errors.

use std::fmt;
use tilewright_jsonc::{Document, Value};

/// The `chunks.encoding` value of this encoding.
pub const CHUNK_ENCODING: &str = "twc1+zstd";
/// zstd compression level used by the compiler.
pub const ZSTD_LEVEL: i32 = 19;
/// Largest decompressed record accepted.
pub const MAXIMUM_RECORD_BYTES: usize = 16 * 1024 * 1024;
/// Largest stored (compressed) chunk accepted: zstd's compression bound for
/// a record of [`MAXIMUM_RECORD_BYTES`], so every encoded chunk fits.
pub const MAXIMUM_CHUNK_BYTES: usize = MAXIMUM_RECORD_BYTES + (MAXIMUM_RECORD_BYTES >> 8);
/// Deepest inventory nesting accepted.
pub const MAXIMUM_DEPTH: usize = 16;

const MAGIC: &[u8; 4] = b"TWC1";
const VERSION: u16 = 1;
const NO_TILE: u16 = u16::MAX;
const FLAG_PORTAL: u8 = 1;
const FLAG_FIELDS: u8 = 2;
const FLAG_CUSTOM: u8 = 4;
const FLAG_INVENTORY: u8 = 8;

/// A decoded chunk. Strings are IDs into the pack's `strings` table.
#[derive(Clone, Debug, PartialEq)]
pub struct ChunkData {
    pub chunk_size: u16,
    /// String IDs referenced by the grids.
    pub palette: Vec<u32>,
    /// One grid per ground layer: `chunk_size²` cells, each a palette index
    /// plus one, or 0 for empty.
    pub ground: Vec<Vec<u16>>,
    pub heights: Vec<i32>,
    /// Palette index plus one, or 0 for the plane's default zone.
    pub zones: Vec<u16>,
    pub objects: Vec<ObjectRecord>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ObjectRecord {
    /// `y * chunk_size + x`; `None` for inventory entries.
    pub tile: Option<u16>,
    pub proto: u32,
    /// Render layer index plus one, or 0 for the prototype's layer.
    pub layer: u16,
    pub sub_layer: i16,
    pub z: i32,
    pub portal: Option<PortalRecord>,
    pub fields: Vec<(u32, FieldRecord)>,
    /// Canonical JSON of the object's `custom` data.
    pub custom: Option<String>,
    pub inventory: Vec<ObjectRecord>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PortalRecord {
    /// `plane` is a string ID, or 0 for the same plane.
    Location {
        plane: u32,
        layer: i32,
        x: i64,
        y: i64,
    },
    Anchor {
        plane: u32,
        anchor: u32,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum FieldRecord {
    Null,
    Bool(bool),
    Integer(i64),
    Number(f64),
    /// A string ID.
    String(u32),
    /// Canonical JSON for arrays and objects.
    Json(String),
}

/// Why a chunk record was rejected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodeError(pub String);

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid chunk record: {}", self.0)
    }
}

impl std::error::Error for DecodeError {}

fn invalid<T>(message: impl Into<String>) -> Result<T, DecodeError> {
    Err(DecodeError(message.into()))
}

/// Encodes and compresses a chunk.
///
/// # Errors
/// Returns an error if the chunk violates the encoding's bounds or zstd fails.
pub fn encode_chunk(chunk: &ChunkData) -> Result<Vec<u8>, DecodeError> {
    let record = encode_record(chunk)?;
    zstd::bulk::compress(&record, ZSTD_LEVEL)
        .or_else(|error| invalid(format!("zstd compression failed: {error}")))
}

/// Encodes a chunk as an uncompressed `twc1` record.
///
/// # Errors
/// Returns an error if the chunk violates the encoding's bounds.
pub fn encode_record(chunk: &ChunkData) -> Result<Vec<u8>, DecodeError> {
    let cells = check_shape(chunk)?;
    // Sizes are checked before each part is appended, so encoding stops as
    // soon as the record would pass its limit.
    let grids = cells * (2 * chunk.ground.len() + 6);
    room(
        &[],
        22_usize
            .saturating_add(chunk.palette.len().saturating_mul(4))
            .saturating_add(grids),
    )?;
    let mut output = Vec::with_capacity(16 + grids);
    output.extend_from_slice(MAGIC);
    output.extend_from_slice(&VERSION.to_le_bytes());
    output.extend_from_slice(&chunk.chunk_size.to_le_bytes());
    output.push(u8::try_from(chunk.ground.len()).or_else(|_| invalid("too many ground layers"))?);
    output.push(0);
    push_u32(&mut output, chunk.palette.len())?;
    for id in &chunk.palette {
        output.extend_from_slice(&id.to_le_bytes());
    }
    for grid in &chunk.ground {
        for cell in grid {
            output.extend_from_slice(&cell.to_le_bytes());
        }
    }
    for height in &chunk.heights {
        output.extend_from_slice(&height.to_le_bytes());
    }
    for cell in &chunk.zones {
        output.extend_from_slice(&cell.to_le_bytes());
    }
    push_u32(&mut output, chunk.objects.len())?;
    for object in &chunk.objects {
        encode_object(&mut output, object, cells, 0)?;
    }
    if output.len() > MAXIMUM_RECORD_BYTES {
        return invalid("record exceeds 16 MiB");
    }
    Ok(output)
}

fn check_shape(chunk: &ChunkData) -> Result<usize, DecodeError> {
    if chunk.chunk_size == 0 || chunk.chunk_size > 255 {
        return invalid("chunk size must be 1..=255");
    }
    let cells = usize::from(chunk.chunk_size) * usize::from(chunk.chunk_size);
    if chunk.ground.len() > 8
        || chunk.ground.iter().any(|grid| grid.len() != cells)
        || chunk.heights.len() != cells
        || chunk.zones.len() != cells
    {
        return invalid("grid sizes do not match the chunk size");
    }
    if chunk.palette.first() == Some(&0) || chunk.palette.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return invalid("palette string IDs must be nonzero and strictly increasing");
    }
    let limit = chunk.palette.len();
    if chunk
        .ground
        .iter()
        .flatten()
        .chain(&chunk.zones)
        .any(|cell| usize::from(*cell) > limit)
    {
        return invalid("a grid cell is outside the palette");
    }
    Ok(cells)
}

/// Checks that `text` is canonical JSON (as `Document::canonical` writes it)
/// for an object, or for an array when `array_allowed`.
fn check_json(text: &str, array_allowed: bool) -> Result<(), DecodeError> {
    let document = Document::parse(text, &crate::canonical_limits())
        .or_else(|error| invalid(format!("embedded JSON: {error}")))?;
    let shape_ok = match document.root() {
        Value::Object { .. } => true,
        Value::Array { .. } => array_allowed,
        _ => false,
    };
    if !shape_ok {
        return invalid(if array_allowed {
            "JSON field values must be arrays or objects"
        } else {
            "custom data must be an object"
        });
    }
    if document.canonical(document.root()) != text {
        return invalid("embedded JSON is not canonical");
    }
    Ok(())
}

/// Fails unless `additional` more bytes fit in a record of at most
/// [`MAXIMUM_RECORD_BYTES`].
fn room(output: &[u8], additional: usize) -> Result<(), DecodeError> {
    if output.len().saturating_add(additional) > MAXIMUM_RECORD_BYTES {
        invalid("record exceeds 16 MiB")
    } else {
        Ok(())
    }
}

fn push_u32(output: &mut Vec<u8>, value: usize) -> Result<(), DecodeError> {
    let value = u32::try_from(value).or_else(|_| invalid("count exceeds u32"))?;
    output.extend_from_slice(&value.to_le_bytes());
    Ok(())
}

fn encode_object(
    output: &mut Vec<u8>,
    object: &ObjectRecord,
    cells: usize,
    depth: usize,
) -> Result<(), DecodeError> {
    if depth > MAXIMUM_DEPTH {
        return invalid("inventory nesting too deep");
    }
    // The fixed part is 15 bytes; a portal adds its kind and fields.
    let portal = match object.portal {
        Some(PortalRecord::Location { .. }) => 25,
        Some(PortalRecord::Anchor { .. }) => 9,
        None => 0,
    };
    room(output, 15 + portal)?;
    let tile = match (object.tile, depth) {
        (Some(tile), 0) if usize::from(tile) < cells => tile,
        (None, depth) if depth > 0 => NO_TILE,
        _ => return invalid("top-level objects need a tile in the chunk; inventory entries none"),
    };
    output.extend_from_slice(&tile.to_le_bytes());
    output.extend_from_slice(&object.proto.to_le_bytes());
    output.extend_from_slice(&object.layer.to_le_bytes());
    output.extend_from_slice(&object.sub_layer.to_le_bytes());
    output.extend_from_slice(&object.z.to_le_bytes());
    let mut flags = 0;
    if object.portal.is_some() {
        flags |= FLAG_PORTAL;
    }
    if !object.fields.is_empty() {
        flags |= FLAG_FIELDS;
    }
    if object.custom.is_some() {
        flags |= FLAG_CUSTOM;
    }
    if !object.inventory.is_empty() {
        flags |= FLAG_INVENTORY;
    }
    output.push(flags);
    match &object.portal {
        Some(PortalRecord::Location { plane, layer, x, y }) => {
            output.push(0);
            output.extend_from_slice(&plane.to_le_bytes());
            output.extend_from_slice(&layer.to_le_bytes());
            output.extend_from_slice(&x.to_le_bytes());
            output.extend_from_slice(&y.to_le_bytes());
        }
        Some(PortalRecord::Anchor { plane, anchor }) => {
            output.push(1);
            output.extend_from_slice(&plane.to_le_bytes());
            output.extend_from_slice(&anchor.to_le_bytes());
        }
        None => {}
    }
    if !object.fields.is_empty() {
        let count = u16::try_from(object.fields.len()).or_else(|_| invalid("too many fields"))?;
        room(output, 2 + usize::from(count) * 13)?;
        output.extend_from_slice(&count.to_le_bytes());
        for (name, value) in &object.fields {
            output.extend_from_slice(&name.to_le_bytes());
            match value {
                FieldRecord::Null => output.push(0),
                FieldRecord::Bool(false) => output.push(1),
                FieldRecord::Bool(true) => output.push(2),
                FieldRecord::Integer(number) => {
                    output.push(3);
                    output.extend_from_slice(&number.to_le_bytes());
                }
                FieldRecord::Number(number) => {
                    if !number.is_finite() {
                        return invalid("numbers must be finite");
                    }
                    output.push(4);
                    output.extend_from_slice(&number.to_le_bytes());
                }
                FieldRecord::String(id) => {
                    output.push(5);
                    output.extend_from_slice(&id.to_le_bytes());
                }
                FieldRecord::Json(text) => {
                    room(output, 5_usize.saturating_add(text.len()))?;
                    check_json(text, true)?;
                    output.push(6);
                    push_u32(output, text.len())?;
                    output.extend_from_slice(text.as_bytes());
                }
            }
        }
    }
    if let Some(custom) = &object.custom {
        room(output, 4_usize.saturating_add(custom.len()))?;
        check_json(custom, false)?;
        push_u32(output, custom.len())?;
        output.extend_from_slice(custom.as_bytes());
    }
    if !object.inventory.is_empty() {
        room(output, 4)?;
        push_u32(output, object.inventory.len())?;
        for item in &object.inventory {
            encode_object(output, item, cells, depth + 1)?;
        }
    }
    Ok(())
}

/// Decompresses and decodes a chunk.
///
/// # Errors
/// Returns an error for anything other than a well-formed record within the
/// encoding's bounds.
pub fn decode_chunk(data: &[u8]) -> Result<ChunkData, DecodeError> {
    if data.len() > MAXIMUM_CHUNK_BYTES {
        return invalid(format!("stored chunk exceeds {MAXIMUM_CHUNK_BYTES} bytes"));
    }
    match zstd::zstd_safe::get_frame_content_size(data) {
        Ok(Some(size)) if size <= MAXIMUM_RECORD_BYTES as u64 => {}
        Ok(Some(_)) => return invalid("record exceeds 16 MiB"),
        _ => return invalid("not a zstd frame with a recorded content size"),
    }
    let record = zstd::bulk::decompress(data, MAXIMUM_RECORD_BYTES)
        .or_else(|error| invalid(format!("zstd decompression failed: {error}")))?;
    decode_record(&record)
}

struct Cursor<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], DecodeError> {
        let end = self
            .position
            .checked_add(count)
            .filter(|end| *end <= self.bytes.len())
            .ok_or_else(|| DecodeError("record ends early".to_owned()))?;
        let slice = &self.bytes[self.position..end];
        self.position = end;
        Ok(slice)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let mut output = [0; N];
        output.copy_from_slice(self.take(N)?);
        Ok(output)
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    fn i16(&mut self) -> Result<i16, DecodeError> {
        Ok(i16::from_le_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn i32(&mut self) -> Result<i32, DecodeError> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    fn i64(&mut self) -> Result<i64, DecodeError> {
        Ok(i64::from_le_bytes(self.array()?))
    }

    fn f64(&mut self) -> Result<f64, DecodeError> {
        Ok(f64::from_le_bytes(self.array()?))
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.position
    }

    /// A count whose items each need at least `minimum` bytes.
    fn count(&mut self, minimum: usize) -> Result<usize, DecodeError> {
        let count = usize::try_from(self.u32()?).or_else(|_| invalid("count exceeds usize"))?;
        if count.saturating_mul(minimum) > self.remaining() {
            return invalid("count exceeds the record");
        }
        Ok(count)
    }

    fn text(&mut self) -> Result<String, DecodeError> {
        let length = self.count(1)?;
        String::from_utf8(self.take(length)?.to_vec()).or_else(|_| invalid("text is not UTF-8"))
    }
}

/// Decodes an uncompressed `twc1` record.
///
/// # Errors
/// Returns an error for anything other than a well-formed record.
pub fn decode_record(record: &[u8]) -> Result<ChunkData, DecodeError> {
    if record.len() > MAXIMUM_RECORD_BYTES {
        return invalid("record exceeds 16 MiB");
    }
    let mut cursor = Cursor {
        bytes: record,
        position: 0,
    };
    if cursor.take(4)? != MAGIC {
        return invalid("bad magic");
    }
    if cursor.u16()? != VERSION {
        return invalid("unsupported version");
    }
    let chunk_size = cursor.u16()?;
    if chunk_size == 0 || chunk_size > 255 {
        return invalid("chunk size must be 1..=255");
    }
    let ground_count = usize::from(cursor.u8()?);
    if ground_count > 8 || cursor.u8()? != 0 {
        return invalid("bad ground count or reserved byte");
    }
    let palette_length = cursor.count(4)?;
    let mut palette = Vec::with_capacity(palette_length);
    for _ in 0..palette_length {
        palette.push(cursor.u32()?);
    }
    let cells = usize::from(chunk_size) * usize::from(chunk_size);
    let mut ground = Vec::with_capacity(ground_count);
    for _ in 0..ground_count {
        if cursor.remaining() < cells * 2 {
            return invalid("record ends early");
        }
        ground.push(
            (0..cells)
                .map(|_| cursor.u16())
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    if cursor.remaining() < cells * 6 {
        return invalid("record ends early");
    }
    let heights = (0..cells)
        .map(|_| cursor.i32())
        .collect::<Result<Vec<_>, _>>()?;
    let zones = (0..cells)
        .map(|_| cursor.u16())
        .collect::<Result<Vec<_>, _>>()?;
    let object_count = cursor.count(15)?;
    let mut objects = Vec::with_capacity(object_count);
    for _ in 0..object_count {
        objects.push(decode_object(&mut cursor, cells, 0)?);
    }
    if cursor.remaining() != 0 {
        return invalid("trailing bytes");
    }
    let chunk = ChunkData {
        chunk_size,
        palette,
        ground,
        heights,
        zones,
        objects,
    };
    check_shape(&chunk)?;
    Ok(chunk)
}

fn decode_object(
    cursor: &mut Cursor<'_>,
    cells: usize,
    depth: usize,
) -> Result<ObjectRecord, DecodeError> {
    if depth > MAXIMUM_DEPTH {
        return invalid("inventory nesting too deep");
    }
    let tile = match (cursor.u16()?, depth) {
        (tile, 0) if usize::from(tile) < cells => Some(tile),
        (NO_TILE, depth) if depth > 0 => None,
        _ => return invalid("object tile out of range"),
    };
    let proto = cursor.u32()?;
    let layer = cursor.u16()?;
    let sub_layer = cursor.i16()?;
    let z = cursor.i32()?;
    let flags = cursor.u8()?;
    if flags & !(FLAG_PORTAL | FLAG_FIELDS | FLAG_CUSTOM | FLAG_INVENTORY) != 0 {
        return invalid("unknown object flags");
    }
    let portal = if flags & FLAG_PORTAL == 0 {
        None
    } else {
        Some(match cursor.u8()? {
            0 => PortalRecord::Location {
                plane: cursor.u32()?,
                layer: cursor.i32()?,
                x: cursor.i64()?,
                y: cursor.i64()?,
            },
            1 => PortalRecord::Anchor {
                plane: cursor.u32()?,
                anchor: cursor.u32()?,
            },
            _ => return invalid("unknown portal kind"),
        })
    };
    let mut fields = Vec::new();
    if flags & FLAG_FIELDS != 0 {
        let count = usize::from(cursor.u16()?);
        if count == 0 || count * 5 > cursor.remaining() {
            return invalid("bad field count");
        }
        for _ in 0..count {
            let name = cursor.u32()?;
            let value = match cursor.u8()? {
                0 => FieldRecord::Null,
                1 => FieldRecord::Bool(false),
                2 => FieldRecord::Bool(true),
                3 => FieldRecord::Integer(cursor.i64()?),
                4 => {
                    let number = cursor.f64()?;
                    if !number.is_finite() {
                        return invalid("numbers must be finite");
                    }
                    FieldRecord::Number(number)
                }
                5 => FieldRecord::String(cursor.u32()?),
                6 => {
                    let text = cursor.text()?;
                    check_json(&text, true)?;
                    FieldRecord::Json(text)
                }
                _ => return invalid("unknown field tag"),
            };
            fields.push((name, value));
        }
    }
    let custom = if flags & FLAG_CUSTOM == 0 {
        None
    } else {
        let text = cursor.text()?;
        check_json(&text, false)?;
        Some(text)
    };
    let mut inventory = Vec::new();
    if flags & FLAG_INVENTORY != 0 {
        let count = cursor.count(15)?;
        if count == 0 {
            return invalid("empty inventory flagged");
        }
        for _ in 0..count {
            inventory.push(decode_object(cursor, cells, depth + 1)?);
        }
    }
    Ok(ObjectRecord {
        tile,
        proto,
        layer,
        sub_layer,
        z,
        portal,
        fields,
        custom,
        inventory,
    })
}
