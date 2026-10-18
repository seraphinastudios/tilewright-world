// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Deterministic compilation of a validated world into a pack.

use crate::{
    APPLICATION_ID, Error, FORMAT, Pack, SCHEMA, USER_VERSION,
    chunk::{
        CHUNK_ENCODING, ChunkData, FieldRecord, MAXIMUM_DEPTH, MAXIMUM_RECORD_BYTES, ObjectRecord,
        PortalRecord, encode_chunk,
    },
    is_field_name, is_identifier,
};
use rusqlite::{Connection, MAIN_DB, params};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read as _, Write as _},
    path::{Path, PathBuf},
};
use tilewright_jsonc::{Limits, MAX_SAFE_INTEGER};
use tilewright_source::{CHUNK_SIZE, Chunk, FieldValue, Object, Portal, Profile, World};

/// The `compiler` meta value.
pub const COMPILER: &str = concat!("tilewright-pack ", env!("CARGO_PKG_VERSION"));

/// Interned strings: IDs start at 1 in byte order of the values; 0 means
/// "none" wherever an ID is optional.
/// The strings are borrowed from the world, and each is checked against its
/// role's limits before it is kept: the model is public.
pub(crate) struct Strings<'w>(BTreeMap<&'w str, u32>);

/// `value` if it is an identifier.
pub(crate) fn identifier(value: &str) -> Result<&str, Error> {
    if is_identifier(value) {
        Ok(value)
    } else {
        Err(Error::Invalid(format!(
            "{:?} is not an identifier",
            value.chars().take(128).collect::<String>()
        )))
    }
}

/// `value` if it is a field name.
pub(crate) fn field_name(value: &str) -> Result<&str, Error> {
    if is_field_name(value) {
        Ok(value)
    } else {
        Err(Error::Invalid("a field name is not valid".into()))
    }
}

/// `value` if it fits a source string: at most 1 MiB, without NUL.
pub(crate) fn text(value: &str) -> Result<&str, Error> {
    if value.len() <= Limits::default().max_string_bytes && !value.contains('\0') {
        Ok(value)
    } else {
        Err(Error::Invalid(
            "a string value is longer than 1 MiB or contains NUL".into(),
        ))
    }
}

/// `value` if it fits a source document: at most 64 MiB.
pub(crate) fn document(value: &str) -> Result<&str, Error> {
    if value.len() <= Limits::default().max_input_bytes {
        Ok(value)
    } else {
        Err(Error::Invalid(
            "a stored document is longer than 64 MiB".into(),
        ))
    }
}

impl<'w> Strings<'w> {
    pub(crate) fn collect(world: &'w World) -> Result<Self, Error> {
        let mut values: BTreeSet<&'w str> = BTreeSet::new();
        for (id, plane) in world.planes() {
            if plane.chunk_size != CHUNK_SIZE {
                return Err(Error::Invalid(format!(
                    "plane {id} has chunk size {}, not {CHUNK_SIZE}",
                    plane.chunk_size
                )));
            }
            for chunk in plane.chunks.values() {
                check_chunk_shape(chunk, world.profile().ground_layers.len())?;
                for grid in chunk.ground.iter().chain([&chunk.zones]) {
                    let used: BTreeSet<u16> = grid.cells.iter().copied().collect();
                    for cell in used {
                        if let Some(index) = usize::from(cell).checked_sub(1) {
                            values.insert(identifier(palette_value(grid, index)?)?);
                        }
                    }
                }
                let mut stack: Vec<&Object> = chunk.objects.iter().collect();
                while let Some(object) = stack.pop() {
                    values.insert(identifier(&object.proto)?);
                    for (name, value) in &object.fields {
                        values.insert(field_name(name)?);
                        if let FieldValue::String(value) = value {
                            values.insert(text(value)?);
                        }
                    }
                    match &object.portal {
                        Some(Portal::Location {
                            plane: Some(plane), ..
                        }) => {
                            values.insert(identifier(plane)?);
                        }
                        Some(Portal::Anchor { plane, anchor }) => {
                            if let Some(plane) = plane {
                                values.insert(identifier(plane)?);
                            }
                            values.insert(identifier(anchor)?);
                        }
                        Some(Portal::Location { plane: None, .. }) | None => {}
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

/// The palette value of a grid cell's index.
pub(crate) fn palette_value(grid: &tilewright_source::Grid, index: usize) -> Result<&str, Error> {
    grid.palette
        .get(index)
        .map(String::as_str)
        .ok_or_else(|| Error::Invalid("a grid cell has no palette entry".into()))
}

/// Render layer names to their record value, index plus one.
pub(crate) type Layers<'p> = BTreeMap<&'p str, u16>;

pub(crate) fn render_layers(profile: &Profile) -> Result<Layers<'_>, Error> {
    profile
        .render_layers
        .iter()
        .enumerate()
        .map(|(index, name)| {
            u16::try_from(index + 1)
                .map(|value| (name.as_str(), value))
                .map_err(|_| Error::Invalid("too many render layers".into()))
        })
        .collect()
}

/// Checks that the stored profile document lists the same render and ground
/// layers, in the same order, and the same layer height as the typed
/// profile the records are built from: the model is public, and readers
/// interpret indexes and elevations by the document.
pub(crate) fn check_profile_document(profile: &Profile) -> Result<(), Error> {
    let document =
        tilewright_jsonc::Document::parse(profile.canonical.as_str(), &crate::canonical_limits())
            .map_err(|error| Error::Invalid(format!("the profile document: {error}")))?;
    let names = |key: &str| -> Option<Vec<&str>> {
        document
            .root()
            .get(key)?
            .as_array()?
            .iter()
            .map(tilewright_jsonc::Value::as_str)
            .collect()
    };
    let same = |key: &str, typed: &[String]| {
        names(key).is_some_and(|names| names.iter().copied().eq(typed.iter().map(String::as_str)))
    };
    let layer_height = document
        .root()
        .get("layer_height")
        .map(tilewright_jsonc::Value::as_i64);
    if same("render_layers", &profile.render_layers)
        && same("ground_layers", &profile.ground_layers)
        && layer_height == profile.layer_height.map(|height| Some(i64::from(height)))
    {
        Ok(())
    } else {
        Err(Error::Invalid(
            "the profile document's layers or layer height differ from the typed profile".into(),
        ))
    }
}

/// Checks a public model's chunk against format 1 before anything is
/// copied from it.
pub(crate) fn check_chunk_shape(chunk: &Chunk, ground_layers: usize) -> Result<(), Error> {
    let cells = usize::from(CHUNK_SIZE) * usize::from(CHUNK_SIZE);
    let grid_ok = |grid: &tilewright_source::Grid| {
        grid.cells.len() == cells && grid.palette.len() < usize::from(u16::MAX)
    };
    if chunk.ground.len() == ground_layers
        && chunk.ground.iter().all(grid_ok)
        && grid_ok(&chunk.zones)
        && chunk.heights.len() == cells
    {
        Ok(())
    } else {
        Err(Error::Invalid(format!(
            "chunk {} does not have the grids of a format-1 chunk",
            chunk.path
        )))
    }
}

/// The bytes an object's own part of a record takes, inventory excluded.
fn encoded_size(object: &Object) -> usize {
    let portal: usize = match object.portal {
        Some(Portal::Location { .. }) => 25,
        Some(Portal::Anchor { .. }) => 9,
        None => 0,
    };
    let fields = if object.fields.is_empty() {
        0
    } else {
        object.fields.iter().fold(2_usize, |total, (_, value)| {
            total.saturating_add(match value {
                FieldValue::Null | FieldValue::Bool(_) => 5,
                FieldValue::Integer(_) | FieldValue::Number(_) => 13,
                FieldValue::String(_) => 9,
                FieldValue::Json(text) => text.len().saturating_add(9),
            })
        })
    };
    let custom = object
        .custom
        .as_ref()
        .map_or(0, |text| text.len().saturating_add(4));
    let inventory = if object.inventory.is_empty() { 0 } else { 4 };
    (15 + portal + inventory)
        .saturating_add(fields)
        .saturating_add(custom)
}

/// Builds an object record. `depth` is the object's inventory depth and
/// `budget` the bytes left in the chunk's record; the model is public, so
/// its bounds are checked here rather than assumed.
pub(crate) fn object_record(
    object: &Object,
    layers: &Layers<'_>,
    strings: &Strings<'_>,
    depth: usize,
    budget: &mut usize,
) -> Result<ObjectRecord, Error> {
    if depth > MAXIMUM_DEPTH {
        return Err(Error::Invalid(format!(
            "inventories nest more than {MAXIMUM_DEPTH} deep"
        )));
    }
    *budget = budget
        .checked_sub(encoded_size(object))
        .ok_or_else(|| Error::Invalid("a chunk's record exceeds 16 MiB".into()))?;
    let chunk_size = CHUNK_SIZE;
    let outside = || {
        Error::Invalid(
            "top-level objects need a tile in their chunk; inventory entries none".into(),
        )
    };
    let tile = match (object.at, depth) {
        (Some([x, y]), 0) if x < chunk_size && y < chunk_size => Some(
            y.checked_mul(chunk_size)
                .and_then(|row| row.checked_add(x))
                .ok_or_else(outside)?,
        ),
        (None, 1..) => None,
        _ => return Err(outside()),
    };
    let layer = match &object.layer {
        Some(name) => layers
            .get(name.as_str())
            .copied()
            .ok_or_else(|| Error::Invalid(format!("unknown render layer \"{name}\"")))?,
        None => 0,
    };
    let portal = match &object.portal {
        Some(Portal::Location { at, .. })
            if at
                .iter()
                .any(|value| !(-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(value)) =>
        {
            return Err(Error::Invalid(
                "a portal leads outside the format's integer range".into(),
            ));
        }
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
        .map(|item| object_record(item, layers, strings, depth + 1, budget))
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
    layers: &Layers<'_>,
    strings: &Strings<'_>,
) -> Result<ChunkData, Error> {
    let chunk_size = CHUNK_SIZE;
    let mut used = BTreeSet::new();
    for grid in chunk.ground.iter().chain([&chunk.zones]) {
        for cell in &grid.cells {
            if let Some(index) = usize::from(*cell).checked_sub(1) {
                used.insert(strings.id(palette_value(grid, index)?)?);
            }
        }
    }
    let palette: Vec<u32> = used.into_iter().collect();
    // Objects may use what the header (14 bytes through the palette length),
    // palette, grids and object count (4 bytes) leave of the record.
    let cells = usize::from(chunk_size) * usize::from(chunk_size);
    let mut budget = MAXIMUM_RECORD_BYTES
        .saturating_sub(18 + 4 * palette.len() + cells * (2 * chunk.ground.len() + 6));
    // Declared palette entries that no cell uses are not interned; they map
    // to 0 and are never looked up.
    let remap = |grid: &tilewright_source::Grid| -> Result<Vec<u16>, Error> {
        let local: Vec<u16> = grid
            .palette
            .iter()
            .map(|value| {
                strings
                    .0
                    .get(value.as_str())
                    .and_then(|id| palette.binary_search(id).ok())
                    .and_then(|index| u16::try_from(index + 1).ok())
                    .unwrap_or(0)
            })
            .collect();
        grid.cells
            .iter()
            .map(|cell| match usize::from(*cell).checked_sub(1) {
                None => Ok(0),
                Some(index) => match local.get(index) {
                    Some(0) | None => {
                        Err(Error::Invalid("a grid cell has no palette entry".into()))
                    }
                    Some(mapped) => Ok(*mapped),
                },
            })
            .collect()
    };
    Ok(ChunkData {
        chunk_size,
        ground: chunk.ground.iter().map(remap).collect::<Result<_, _>>()?,
        heights: chunk.heights.clone(),
        zones: remap(&chunk.zones)?,
        objects: chunk
            .objects
            .iter()
            .map(|object| object_record(object, layers, strings, 0, &mut budget))
            .collect::<Result<_, _>>()?,
        palette,
    })
}

/// A stairwell step's row: its stairwell, its index in the stairwell, and
/// its prototype.
pub(crate) type TileStep<'w> = (&'w str, i64, &'w str);

/// The steps of a plane's stairwells by tile, the `stairwell_steps` key, so
/// they are inserted in primary-key order.
pub(crate) fn stairwell_tiles(
    plane: &tilewright_source::Plane,
) -> Result<BTreeMap<(i32, i64, i64), TileStep<'_>>, Error> {
    let mut tiles = BTreeMap::new();
    for (id, stairwell) in &plane.stairwells {
        for (index, step) in stairwell.steps.iter().enumerate() {
            let index = i64::try_from(index)
                .map_err(|_| Error::Invalid("a stairwell has too many steps".into()))?;
            let tile = (step.layer, step.at[0], step.at[1]);
            if tiles
                .insert(tile, (id.as_str(), index, step.proto.as_str()))
                .is_some()
            {
                return Err(Error::Invalid(format!(
                    "two stairwell steps of plane {} share layer {} tile [{}, {}]",
                    plane.id, tile.0, tile.1, tile.2
                )));
            }
        }
    }
    Ok(tiles)
}

/// Inserts a plane's stairwells and their steps, each in primary-key order.
fn insert_stairwells(
    connection: &Connection,
    id: &str,
    plane: &tilewright_source::Plane,
) -> Result<(), Error> {
    let mut stairwells = connection.prepare(
        "INSERT INTO stairwells (plane, id, direction, hidden_on_other_layers, document) VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for (stairwell_id, stairwell) in &plane.stairwells {
        stairwells.execute(params![
            id,
            stairwell_id,
            stairwell.direction.as_str(),
            stairwell.hidden_on_other_layers,
            document(&stairwell.canonical)?
        ])?;
    }
    let mut steps = connection.prepare(
        "INSERT INTO stairwell_steps (plane, layer, x, y, stairwell, step, proto) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for ((layer, x, y), (stairwell, step, proto)) in stairwell_tiles(plane)? {
        steps.execute(params![id, layer, x, y, stairwell, step, proto])?;
    }
    Ok(())
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
    check_profile_document(world.profile())?;
    let layers = render_layers(world.profile())?;
    let connection = Connection::open_in_memory()?;
    let schema: &str = &SCHEMA;
    connection.execute_batch(&format!(
        "PRAGMA page_size = 4096; PRAGMA journal_mode = OFF; PRAGMA synchronous = OFF;
         PRAGMA application_id = {APPLICATION_ID}; PRAGMA user_version = {USER_VERSION}; {schema}"
    ))?;
    connection.execute_batch("BEGIN")?;
    {
        let profile = world.profile();
        let digest = hex(&world.source_digest());
        let mut meta = connection.prepare("INSERT INTO meta (key, value) VALUES (?1, ?2)")?;
        for (key, value) in [
            ("chunk_encoding", CHUNK_ENCODING),
            ("compiler", COMPILER),
            ("format", FORMAT),
            ("profile", document(&profile.canonical)?),
            ("profile_id", profile.id.as_str()),
            ("profile_version", profile.version.as_str()),
            ("source_digest", digest.as_str()),
        ] {
            meta.execute(params![key, value])?;
        }
        let mut insert = connection.prepare("INSERT INTO strings (id, value) VALUES (?1, ?2)")?;
        let mut by_id: Vec<(&u32, &&str)> =
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
        for (id, plane) in world.planes() {
            planes.execute(params![
                id,
                plane.kind.as_str(),
                plane.chunk_size,
                document(&plane.canonical)?
            ])?;
            for (zone_id, zone) in &plane.zones {
                zones.execute(params![
                    id,
                    zone_id,
                    zone.parent,
                    document(&zone.canonical)?
                ])?;
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
            insert_stairwells(&connection, id, plane)?;
            for ((layer, cx, cy), chunk) in &plane.chunks {
                let record = chunk_record(chunk, &layers, &strings)?;
                let data = encode_chunk(&record).map_err(|error| Error::Invalid(error.0))?;
                chunks.execute(params![id, layer, cx, cy, CHUNK_ENCODING, data])?;
            }
        }
        let mut definitions = connection
            .prepare("INSERT INTO definitions (type, id, document) VALUES (?1, ?2, ?3)")?;
        for ((kind, id), definition) in world.definitions() {
            definitions.execute(params![kind, id, document(&definition.canonical)?])?;
        }
    }
    connection.execute_batch("COMMIT; VACUUM;")?;
    // The model is public, so the compiled pack is verified like any other
    // before it is returned: a malformed model never yields an invalid pack.
    let pack = Pack::compiled(connection)?;
    // The compiler decodes only what it just encoded, so no budget applies.
    pack.verify_contents(u64::MAX).map_err(|error| {
        Error::Invalid(format!(
            "the world does not compile to a valid pack: {error}"
        ))
    })?;
    let connection = pack.into_connection();
    let data = connection.serialize(MAIN_DB)?;
    Ok(data.to_vec())
}

/// Whether `path` is a regular file holding exactly `data`. The length is
/// compared first and the contents in fixed-size blocks, so a large file is
/// never loaded whole.
fn same_file_contents(path: &Path, data: &[u8]) -> Result<bool, Error> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.len() != u64::try_from(data.len()).unwrap_or(u64::MAX) {
        return Ok(false);
    }
    let mut file = std::fs::File::open(path)?;
    let mut buffer = vec![0; 64 * 1024];
    let mut offset = 0;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            return Ok(offset == data.len());
        }
        match data.get(offset..offset + read) {
            Some(expected) if expected == &buffer[..read] => offset += read,
            _ => return Ok(false),
        }
    }
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

/// Compiles a world and writes it to `directory/<sha256>.twpack`, creating
/// the directory if needed. The write is atomic and never replaces a
/// different file.
///
/// # Errors
/// Returns an error if compilation or the write fails, or if a different
/// file already has the pack's name.
pub fn write_pack(world: &World, directory: &Path) -> Result<Written, Error> {
    let data = compile(world)?;
    std::fs::create_dir_all(directory)?;
    let digest: [u8; 32] = Sha256::digest(&data).into();
    let path = directory.join(format!("{}.twpack", hex(&digest)));
    let bytes = u64::try_from(data.len()).unwrap_or(u64::MAX);
    if std::fs::symlink_metadata(&path).is_ok() {
        if same_file_contents(&path, &data)? {
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
    let created = persist(temporary, &path, &data)?;
    Ok(Written {
        path,
        digest,
        bytes,
        created,
    })
}

/// Moves `temporary` to `path` without replacing anything. When another
/// writer got there first with the same bytes, that is not an error: returns
/// whether this call created the file.
pub(crate) fn persist(
    temporary: tempfile::NamedTempFile,
    path: &Path,
    data: &[u8],
) -> Result<bool, Error> {
    match temporary.persist_noclobber(path) {
        Ok(_) => Ok(true),
        Err(error)
            if error.error.kind() == std::io::ErrorKind::AlreadyExists
                && same_file_contents(path, data)? =>
        {
            Ok(false)
        }
        Err(error) => Err(Error::Io(error.error)),
    }
}
