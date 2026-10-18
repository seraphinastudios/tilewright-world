// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Deterministic compilation of a validated world into a pack.

use crate::{
    APPLICATION_ID, Error, FORMAT, SCHEMA,
    chunk::{CHUNK_ENCODING, ChunkData, FieldRecord, ObjectRecord, PortalRecord, encode_chunk},
};
use rusqlite::{Connection, MAIN_DB, params};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write as _,
    path::{Path, PathBuf},
};
use tilewright_source::{Chunk, FieldValue, Object, Portal, Profile, World};

/// The `compiler` meta value.
pub const COMPILER: &str = concat!("tilewright-pack ", env!("CARGO_PKG_VERSION"));

/// Interned strings: IDs start at 1 in byte order of the values; 0 means
/// "none" wherever an ID is optional.
struct Strings(BTreeMap<String, u32>);

impl Strings {
    fn collect(world: &World) -> Result<Self, Error> {
        let mut values = BTreeSet::new();
        for plane in world.planes.values() {
            for chunk in plane.chunks.values() {
                for grid in chunk.ground.iter().chain([&chunk.zones]) {
                    let used: BTreeSet<u16> = grid.cells.iter().copied().collect();
                    for cell in used {
                        if let Some(index) = usize::from(cell).checked_sub(1) {
                            values.insert(grid.palette[index].clone());
                        }
                    }
                }
                let mut stack: Vec<&Object> = chunk.objects.iter().collect();
                while let Some(object) = stack.pop() {
                    values.insert(object.proto.clone());
                    for (name, value) in &object.fields {
                        values.insert(name.clone());
                        if let FieldValue::String(text) = value {
                            values.insert(text.clone());
                        }
                    }
                    match &object.portal {
                        Some(Portal::Location { plane, .. }) => values.extend(plane.clone()),
                        Some(Portal::Anchor { plane, anchor }) => {
                            values.extend(plane.clone());
                            values.insert(anchor.clone());
                        }
                        None => {}
                    }
                    stack.extend(&object.inventory);
                }
            }
        }
        let mut ids = BTreeMap::new();
        for (index, value) in values.into_iter().enumerate() {
            let id =
                u32::try_from(index + 1).map_err(|_| Error::Invalid("too many strings".into()))?;
            ids.insert(value, id);
        }
        Ok(Self(ids))
    }

    fn id(&self, value: &str) -> Result<u32, Error> {
        self.0
            .get(value)
            .copied()
            .ok_or_else(|| Error::Invalid(format!("string \"{value}\" was not interned")))
    }

    fn optional(&self, value: Option<&String>) -> Result<u32, Error> {
        value.map_or(Ok(0), |value| self.id(value))
    }
}

fn object_record(
    object: &Object,
    profile: &Profile,
    strings: &Strings,
    chunk_size: u16,
) -> Result<ObjectRecord, Error> {
    let tile = object.at.map(|[x, y]| y * chunk_size + x);
    let layer = match &object.layer {
        Some(name) => profile
            .render_layer_index(name)
            .and_then(|index| u16::try_from(index + 1).ok())
            .ok_or_else(|| Error::Invalid(format!("unknown render layer \"{name}\"")))?,
        None => 0,
    };
    let portal = match &object.portal {
        Some(Portal::Location { plane, layer, at }) => Some(PortalRecord::Location {
            plane: strings.optional(plane.as_ref())?,
            layer: *layer,
            x: at[0],
            y: at[1],
        }),
        Some(Portal::Anchor { plane, anchor }) => Some(PortalRecord::Anchor {
            plane: strings.optional(plane.as_ref())?,
            anchor: strings.id(anchor)?,
        }),
        None => None,
    };
    let mut fields = Vec::with_capacity(object.fields.len());
    for (name, value) in &object.fields {
        let record = match value {
            FieldValue::Null => FieldRecord::Null,
            FieldValue::Bool(flag) => FieldRecord::Bool(*flag),
            FieldValue::Integer(number) => FieldRecord::Integer(*number),
            FieldValue::Number(number) => FieldRecord::Number(*number),
            FieldValue::String(text) => FieldRecord::String(strings.id(text)?),
            FieldValue::Json(text) => FieldRecord::Json(text.clone()),
        };
        fields.push((strings.id(name)?, record));
    }
    let inventory = object
        .inventory
        .iter()
        .map(|item| object_record(item, profile, strings, chunk_size))
        .collect::<Result<_, _>>()?;
    Ok(ObjectRecord {
        tile,
        proto: strings.id(&object.proto)?,
        layer,
        sub_layer: object.sub_layer,
        z: object.z,
        portal,
        fields,
        custom: object.custom.clone(),
        inventory,
    })
}

/// Builds a chunk record. The chunk palette holds the string IDs its grids
/// use, in ascending order.
fn chunk_record(
    chunk: &Chunk,
    profile: &Profile,
    strings: &Strings,
    chunk_size: u16,
) -> Result<ChunkData, Error> {
    let mut used = BTreeSet::new();
    for grid in chunk.ground.iter().chain([&chunk.zones]) {
        for cell in &grid.cells {
            if let Some(index) = usize::from(*cell).checked_sub(1) {
                used.insert(strings.id(&grid.palette[index])?);
            }
        }
    }
    let palette: Vec<u32> = used.into_iter().collect();
    let remap = |grid: &tilewright_source::Grid| -> Result<Vec<u16>, Error> {
        let local: Vec<u16> = grid
            .palette
            .iter()
            .map(|value| {
                let id = strings.id(value)?;
                Ok(palette
                    .binary_search(&id)
                    .ok()
                    .and_then(|index| u16::try_from(index + 1).ok())
                    .unwrap_or(0))
            })
            .collect::<Result<_, Error>>()?;
        Ok(grid
            .cells
            .iter()
            .map(|cell| {
                usize::from(*cell)
                    .checked_sub(1)
                    .map_or(0, |index| local[index])
            })
            .collect())
    };
    Ok(ChunkData {
        chunk_size,
        ground: chunk.ground.iter().map(remap).collect::<Result<_, _>>()?,
        heights: chunk.heights.clone(),
        zones: remap(&chunk.zones)?,
        objects: chunk
            .objects
            .iter()
            .map(|object| object_record(object, profile, strings, chunk_size))
            .collect::<Result<_, _>>()?,
        palette,
    })
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut output, byte| {
        let _ = write!(output, "{byte:02x}");
        output
    })
}

/// Compiles a world into pack bytes. The same world and compiler version
/// always produce the same bytes.
///
/// # Errors
/// Returns an error if SQLite fails or the world exceeds the pack's bounds.
pub fn compile(world: &World) -> Result<Vec<u8>, Error> {
    let strings = Strings::collect(world)?;
    let connection = Connection::open_in_memory()?;
    let schema: &str = &SCHEMA;
    connection.execute_batch(&format!(
        "PRAGMA page_size = 4096; PRAGMA journal_mode = OFF; PRAGMA synchronous = OFF;
         PRAGMA application_id = {APPLICATION_ID}; PRAGMA user_version = 1; {schema}"
    ))?;
    connection.execute_batch("BEGIN")?;
    {
        let profile = &world.profile;
        let digest = hex(&world.source_digest);
        let mut meta = connection.prepare("INSERT INTO meta (key, value) VALUES (?1, ?2)")?;
        for (key, value) in [
            ("chunk_encoding", CHUNK_ENCODING),
            ("compiler", COMPILER),
            ("format", FORMAT),
            ("profile", profile.canonical.as_str()),
            ("profile_id", profile.id.as_str()),
            ("profile_version", profile.version.as_str()),
            ("source_digest", digest.as_str()),
        ] {
            meta.execute(params![key, value])?;
        }
        let mut insert = connection.prepare("INSERT INTO strings (id, value) VALUES (?1, ?2)")?;
        let mut by_id: Vec<(&u32, &String)> =
            strings.0.iter().map(|(value, id)| (id, value)).collect();
        by_id.sort();
        for (id, value) in by_id {
            insert.execute(params![id, value])?;
        }
        let mut planes = connection.prepare(
            "INSERT INTO planes (id, kind, chunk_size, document) VALUES (?1, ?2, ?3, ?4)",
        )?;
        let mut zones = connection
            .prepare("INSERT INTO zones (plane, id, parent, document) VALUES (?1, ?2, ?3, ?4)")?;
        let mut anchors = connection
            .prepare("INSERT INTO anchors (plane, id, layer, x, y) VALUES (?1, ?2, ?3, ?4, ?5)")?;
        let mut chunks = connection.prepare(
            "INSERT INTO chunks (plane, layer, cx, cy, encoding, data) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for (id, plane) in &world.planes {
            planes.execute(params![
                id,
                plane.kind.as_str(),
                plane.chunk_size,
                plane.canonical
            ])?;
            for (zone_id, zone) in &plane.zones {
                zones.execute(params![id, zone_id, zone.parent, zone.canonical])?;
            }
            for (anchor_id, anchor) in &plane.anchors {
                anchors.execute(params![
                    id,
                    anchor_id,
                    anchor.layer,
                    anchor.at[0],
                    anchor.at[1]
                ])?;
            }
            for ((layer, cx, cy), chunk) in &plane.chunks {
                let record = chunk_record(chunk, profile, &strings, plane.chunk_size)?;
                let data = encode_chunk(&record).map_err(|error| Error::Invalid(error.0))?;
                chunks.execute(params![id, layer, cx, cy, CHUNK_ENCODING, data])?;
            }
        }
        let mut definitions = connection
            .prepare("INSERT INTO definitions (type, id, document) VALUES (?1, ?2, ?3)")?;
        for ((kind, id), definition) in &world.definitions {
            definitions.execute(params![kind, id, definition.canonical])?;
        }
    }
    connection.execute_batch("COMMIT; VACUUM;")?;
    let data = connection.serialize(MAIN_DB)?;
    Ok(data.to_vec())
}

/// A pack written to disk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Written {
    pub path: PathBuf,
    /// SHA-256 of the pack file, which is also its name.
    pub digest: [u8; 32],
    pub bytes: u64,
    /// `false` when an identical pack was already present.
    pub created: bool,
}

/// Compiles a world and writes it to `directory/<sha256>.twpack`. The write
/// is atomic and never replaces a different file.
///
/// # Errors
/// Returns an error if compilation or the write fails, or if a different
/// file already has the pack's name.
pub fn write_pack(world: &World, directory: &Path) -> Result<Written, Error> {
    let data = compile(world)?;
    let digest: [u8; 32] = Sha256::digest(&data).into();
    let path = directory.join(format!("{}.twpack", hex(&digest)));
    let bytes = u64::try_from(data.len()).unwrap_or(u64::MAX);
    if path.exists() {
        let existing = std::fs::read(&path)?;
        if existing == data {
            return Ok(Written {
                path,
                digest,
                bytes,
                created: false,
            });
        }
        return Err(Error::Invalid(format!(
            "{} exists with different contents",
            path.display()
        )));
    }
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(&data)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(&path)
        .map_err(|error| Error::Io(error.error))?;
    Ok(Written {
        path,
        digest,
        bytes,
        created: true,
    })
}
