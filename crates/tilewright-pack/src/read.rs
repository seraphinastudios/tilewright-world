// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Reading and verifying packs. A pack is untrusted input: it is opened
//! read-only and immutable, in SQLite's defensive mode, and its schema must
//! match this version exactly before any other query runs. No value is
//! loaded before SQL has checked its type and size, and verification streams
//! rows instead of collecting whole tables.

use crate::{
    APPLICATION_ID, Error, FORMAT, META_KEYS, SCHEMA_TABLES, USER_VERSION,
    chunk::{
        CHUNK_ENCODING, ChunkData, DecodeError, FieldRecord, MAXIMUM_CHUNK_BYTES, ObjectRecord,
        PortalRecord, Visit, decode_chunk_with,
    },
    is_field_name, is_identifier,
};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Statement, config::DbConfig, params, types,
};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write as _,
    io::Read as _,
    path::{Path, PathBuf},
};
use tilewright_jsonc::{Document, Limits, MAX_SAFE_INTEGER, Value};
use tilewright_source::{CHUNK_SIZE, MAXIMUM_GROUND_LAYERS};

/// The page size of every pack.
const PAGE_SIZE: i64 = 4096;

/// Largest `meta` value other than `profile`.
const MAXIMUM_META_BYTES: usize = 1024;

/// Largest `sqlite_schema` value loaded; every expected one is shorter.
const MAXIMUM_SCHEMA_BYTES: usize = 1024;

/// The default [`VerifyLimits::maximum_decoded_bytes`]: 8 GiB.
pub const DEFAULT_MAXIMUM_DECODED_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Bounds on the work a full verification may do.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerifyLimits {
    /// Most bytes of chunk records decoded, summed over every chunk.
    /// Verification fails once the chunks' recorded sizes pass it, before
    /// the chunk that would pass it is decompressed.
    pub maximum_decoded_bytes: u64,
}

impl Default for VerifyLimits {
    fn default() -> Self {
        Self {
            maximum_decoded_bytes: DEFAULT_MAXIMUM_DECODED_BYTES,
        }
    }
}

/// Row counts of a pack.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Summary {
    pub strings: u64,
    pub planes: u64,
    pub zones: u64,
    pub anchors: u64,
    pub chunks: u64,
    pub definitions: u64,
    /// Objects in every chunk, inventories included; counted by
    /// [`Pack::verify`] only.
    pub objects: u64,
}

pub struct Pack {
    connection: Connection,
    /// `None` for a pack the compiler is checking in memory.
    path: Option<PathBuf>,
}

/// Percent-encodes a path for an SQLite URI. On Unix the path's bytes are
/// encoded as they are, so paths that are not UTF-8 open too.
#[cfg_attr(
    unix,
    expect(
        clippy::unnecessary_wraps,
        reason = "only platforms whose paths must be UTF-8 can fail"
    )
)]
fn uri(absolute: &Path) -> Result<String, Error> {
    #[cfg(unix)]
    let bytes = std::os::unix::ffi::OsStrExt::as_bytes(absolute.as_os_str());
    #[cfg(not(unix))]
    let bytes = absolute
        .to_str()
        .ok_or_else(|| Error::Invalid("pack path is not UTF-8".into()))?
        .as_bytes();
    let mut encoded = String::from("file:");
    for byte in bytes {
        if byte.is_ascii_alphanumeric() || b"/-_.~".contains(byte) {
            encoded.push(char::from(*byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded.push_str("?immutable=1");
    Ok(encoded)
}

/// A size bound as an SQL parameter.
fn bound(bytes: usize) -> i64 {
    i64::try_from(bytes).unwrap_or(i64::MAX)
}

/// The largest stored document: the source format's input limit.
fn maximum_document_bytes() -> i64 {
    bound(Limits::default().max_input_bytes)
}

/// The largest interned string: the source format's string limit.
fn maximum_string_bytes() -> i64 {
    bound(Limits::default().max_string_bytes)
}

/// SQL that is true when `column` is not an identifier
/// (`^[a-z0-9][a-z0-9_.-]{0,127}$`). The length is checked first, without
/// loading the value.
fn not_identifier(column: &str) -> String {
    // GLOB stops at a NUL byte, so NUL is rejected separately.
    format!(
        "(typeof({column}) != 'text' OR octet_length({column}) NOT BETWEEN 1 AND 128 \
         OR instr({column}, char(0)) != 0 \
         OR {column} NOT GLOB '[a-z0-9]*' OR {column} GLOB '*[^a-z0-9_.-]*')"
    )
}

/// SQL for a `chunks c` row's data: the blob when the row's encoding is this
/// one and the blob is within [`MAXIMUM_CHUNK_BYTES`], else 0 for another
/// encoding and 1 for other data. Nothing larger is loaded.
fn chunk_data_sql() -> String {
    format!(
        "CASE WHEN octet_length(c.encoding) = {length} AND c.encoding = '{CHUNK_ENCODING}' \
         THEN CASE WHEN typeof(c.data) = 'blob' AND octet_length(c.data) <= {MAXIMUM_CHUNK_BYTES} \
         THEN c.data ELSE 1 END ELSE 0 END",
        length = CHUNK_ENCODING.len()
    )
}

/// The decoded size a chunk's zstd frame records, or 0 when it has none
/// (such a chunk fails to decode).
fn recorded_size(value: &types::Value) -> u64 {
    match value {
        types::Value::Blob(data) => zstd::zstd_safe::get_frame_content_size(data)
            .ok()
            .flatten()
            .unwrap_or(0),
        _ => 0,
    }
}

/// Decodes the value of [`chunk_data_sql`].
fn chunk_data(value: types::Value) -> Result<ChunkData, Error> {
    chunk_data_with(value, None)
}

/// [`chunk_data`], streaming objects to `visit` when it is given.
fn chunk_data_with(value: types::Value, visit: Option<Visit<'_>>) -> Result<ChunkData, Error> {
    match value {
        types::Value::Blob(data) => decode_chunk_with(&data, visit).map_err(Error::Chunk),
        types::Value::Integer(0) => Err(Error::Invalid(format!(
            "a chunk's encoding is not {CHUNK_ENCODING}"
        ))),
        _ => Err(Error::Invalid(format!(
            "a chunk's data is not a blob of at most {MAXIMUM_CHUNK_BYTES} bytes"
        ))),
    }
}

/// SQL for `column` when it is text of at most `?1` bytes, else NULL.
fn bounded_text(column: &str) -> String {
    format!(
        "CASE WHEN typeof({column}) = 'text' AND octet_length({column}) <= ?1 THEN {column} END"
    )
}

/// Parses a stored document, which must be canonical JSON of an object.
fn stored_document(text: Option<String>, what: &str) -> Result<Document, Error> {
    let invalid = || Error::Invalid(format!("{what} is not canonical JSON of an object"));
    let document = Document::parse(text.ok_or_else(invalid)?, &crate::canonical_limits())
        .map_err(|_| invalid())?;
    if document.root().as_object().is_none()
        || document.canonical(document.root()) != document.source()
    {
        return Err(invalid());
    }
    Ok(document)
}

/// A string member of a document's root object.
fn text<'d>(document: &'d Document, key: &str) -> Option<&'d str> {
    document.root().get(key).and_then(Value::as_str)
}

impl Pack {
    /// Opens a pack and checks its identity and schema.
    ///
    /// # Errors
    /// Returns an error if the file is not a pack of this format.
    pub fn open(path: &Path) -> Result<Self, Error> {
        // The same absolute path opens the database and is hashed later, so
        // a change of working directory cannot point them at different files.
        let path = std::path::absolute(path)?;
        if !std::fs::metadata(&path)?.is_file() {
            return Err(Error::Invalid(format!("{} is not a file", path.display())));
        }
        let connection = Connection::open_with_flags(
            uri(&path)?,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
        connection.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?;
        connection.execute_batch("PRAGMA query_only = ON")?;
        check_identity(&connection)?;
        Ok(Self {
            connection,
            path: Some(path),
        })
    }

    /// A pack the compiler has just built, checked like an opened one.
    pub(crate) fn compiled(connection: Connection) -> Result<Self, Error> {
        check_identity(&connection)?;
        Ok(Self {
            connection,
            path: None,
        })
    }

    pub(crate) fn into_connection(self) -> Connection {
        self.connection
    }

    /// A `meta` value.
    ///
    /// # Errors
    /// Returns an error if SQLite fails.
    pub fn meta(&self, key: &str) -> Result<Option<String>, Error> {
        Ok(self
            .connection
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()?)
    }

    /// An interned string.
    ///
    /// # Errors
    /// Returns an error if SQLite fails or the stored value is not text of
    /// at most 1 MiB.
    pub fn string(&self, id: u32) -> Result<Option<String>, Error> {
        let value: Option<Option<String>> = self
            .connection
            .query_row(
                &format!(
                    "SELECT {} FROM strings WHERE id = ?2",
                    bounded_text("value")
                ),
                params![maximum_string_bytes(), id],
                |row| row.get(0),
            )
            .optional()?;
        match value {
            None => Ok(None),
            Some(Some(text)) => Ok(Some(text)),
            Some(None) => Err(Error::Invalid(
                "a string is not text of at most 1 MiB".into(),
            )),
        }
    }

    /// Decodes one chunk into its full model; see [`decode_chunk`].
    ///
    /// [`decode_chunk`]: crate::decode_chunk
    ///
    /// # Errors
    /// Returns an error if SQLite fails or the stored record is invalid.
    pub fn chunk(
        &self,
        plane: &str,
        layer: i32,
        cx: i64,
        cy: i64,
    ) -> Result<Option<ChunkData>, Error> {
        let value: Option<types::Value> = self
            .connection
            .query_row(
                &format!(
                    "SELECT {} FROM chunks c WHERE c.plane = ?1 AND c.layer = ?2 AND c.cx = ?3 AND c.cy = ?4",
                    chunk_data_sql()
                ),
                params![plane, layer, cx, cy],
                |row| row.get(0),
            )
            .optional()?;
        value.map(chunk_data).transpose()
    }

    /// Row counts.
    ///
    /// # Errors
    /// Returns an error if SQLite fails.
    pub fn summary(&self) -> Result<Summary, Error> {
        let count = |table: &str| -> Result<u64, Error> {
            let value: i64 =
                self.connection
                    .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                        row.get(0)
                    })?;
            Ok(u64::try_from(value).unwrap_or(0))
        };
        Ok(Summary {
            strings: count("strings")?,
            planes: count("planes")?,
            zones: count("zones")?,
            anchors: count("anchors")?,
            chunks: count("chunks")?,
            definitions: count("definitions")?,
            objects: 0,
        })
    }

    /// The SHA-256 of the pack file, read from its path when called.
    ///
    /// This hashes whatever file is at the pack's path now, not necessarily
    /// the file SQLite opened: the digest binds to the path. Packs are
    /// content-addressed files opened from directories the caller controls;
    /// a directory that others can write to is not supported.
    ///
    /// # Errors
    /// Returns an error if the file cannot be read.
    pub fn file_digest(&self) -> Result<[u8; 32], Error> {
        let path = self
            .path
            .as_ref()
            .ok_or_else(|| Error::Invalid("the pack is not a file".into()))?;
        let mut file = std::fs::File::open(path)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0; 64 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(hasher.finalize().into())
    }

    /// Checks the whole pack with [`VerifyLimits::default`]; see
    /// [`Pack::verify_with`].
    ///
    /// # Errors
    /// Returns the first problem found.
    pub fn verify(&self) -> Result<Summary, Error> {
        self.verify_with(&VerifyLimits::default())
    }

    /// Checks the whole pack: SQLite integrity, the file name digest when the
    /// name is one, string, plane, zone, anchor and definition rows, every
    /// stored document, every chunk record, and every string, layer, ground
    /// count and portal target a record references.
    ///
    /// Decoding every chunk is linear in the pack's decoded size, which
    /// compression can make far larger than the file, so it stops and fails
    /// once `limits.maximum_decoded_bytes` would be passed. The file name
    /// check hashes the file at the pack's path, so it binds to the path; see
    /// [`Pack::file_digest`].
    ///
    /// # Errors
    /// Returns the first problem found, including an exceeded limit.
    pub fn verify_with(&self, limits: &VerifyLimits) -> Result<Summary, Error> {
        let integrity: String = self
            .connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
        if integrity != "ok" {
            return Err(Error::Invalid(format!(
                "integrity check failed: {integrity}"
            )));
        }
        if let Some(stem) = self
            .path
            .as_ref()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".twpack"))
            .filter(|stem| stem.len() == 64 && stem.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            // The name must be the lowercase digest; other cases do not match.
            let digest = crate::compile::hex(&self.file_digest()?);
            if digest != stem {
                return Err(Error::Invalid(format!(
                    "file digest {digest} does not match its name"
                )));
            }
        }
        self.verify_contents(limits.maximum_decoded_bytes)
    }

    /// Everything [`Pack::verify`] checks except SQLite integrity and the
    /// file name: what the compiler checks of every pack it builds.
    pub(crate) fn verify_contents(&self, maximum_decoded_bytes: u64) -> Result<Summary, Error> {
        let mut summary = self.summary()?;
        let (minimum, maximum): (Option<i64>, Option<i64>) =
            self.connection
                .query_row("SELECT min(id), max(id) FROM strings", [], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })?;
        let contiguous = match (minimum, maximum) {
            (None, None) => summary.strings == 0,
            (Some(1), Some(maximum)) => u64::try_from(maximum).ok() == Some(summary.strings),
            _ => false,
        };
        if !contiguous {
            return Err(Error::Invalid("string IDs are not 1..=count".into()));
        }
        self.check_rows()?;
        self.check_string_encoding()?;
        self.check_documents()?;
        let (render_layers, ground_layers) = self.profile_layers()?;
        let mut targets = Targets {
            strings: u32::try_from(summary.strings)
                .map_err(|_| Error::Invalid("too many strings".into()))?,
            render_layers,
            is_plane: self.connection.prepare(
                "SELECT EXISTS (SELECT 1 FROM strings s JOIN planes p ON p.id = s.value WHERE s.id = ?1)",
            )?,
            is_proto: self.connection.prepare(
                "SELECT EXISTS (SELECT 1 FROM strings s JOIN definitions d ON d.type = 'proto' AND d.id = s.value WHERE s.id = ?1)",
            )?,
            is_zone: self.connection.prepare(
                "SELECT EXISTS (SELECT 1 FROM strings s JOIN zones z ON z.plane = ?2 AND z.id = s.value WHERE s.id = ?1)",
            )?,
            has_anchor: self.connection.prepare(
                "SELECT EXISTS (SELECT 1 FROM strings n JOIN anchors a ON a.id = n.value \
                 WHERE n.id = ?3 AND a.plane = CASE WHEN ?1 = 0 THEN ?2 ELSE (SELECT value FROM strings WHERE id = ?1) END)",
            )?,
            value: self.connection.prepare("SELECT value FROM strings WHERE id = ?1")?,
            planes: BTreeMap::new(),
            protos: BTreeMap::new(),
            field_names: BTreeMap::new(),
            anchors: BTreeMap::new(),
            current: String::new(),
            zones: BTreeMap::new(),
            own_anchors: BTreeMap::new(),
        };
        let mut statement = self.connection.prepare(&format!(
            "SELECT p.id, p.chunk_size, {} FROM chunks c JOIN planes p ON p.id = c.plane ORDER BY c.plane, c.layer, c.cx, c.cy",
            chunk_data_sql()
        ))?;
        let mut rows = statement.query([])?;
        let mut checked = 0;
        let mut decoded: u64 = 0;
        while let Some(row) = rows.next()? {
            let plane: String = row.get(0)?;
            let chunk_size: u16 = row.get(1)?;
            let data: types::Value = row.get(2)?;
            // The frame records its decoded size, so the budget is checked
            // before the chunk is decompressed.
            decoded = decoded.saturating_add(recorded_size(&data));
            if decoded > maximum_decoded_bytes {
                return Err(Error::Invalid(format!(
                    "the chunks decode to more than {maximum_decoded_bytes} bytes, the verification limit"
                )));
            }
            // Objects are checked as they decode rather than kept, so memory
            // stays bounded by the record, not by the objects it holds.
            targets.enter(&plane);
            let mut failure = None;
            let mut visit = |object: &ObjectRecord| {
                summary.objects += 1;
                targets.check(object).map_err(|error| {
                    failure = Some(error);
                    DecodeError("an object failed verification".into())
                })
            };
            let decoded_chunk = chunk_data_with(data, Some(&mut visit));
            if let Some(error) = failure {
                return Err(error);
            }
            let chunk = decoded_chunk?;
            if chunk.chunk_size != chunk_size || chunk.ground.len() != ground_layers {
                return Err(Error::Invalid(
                    "a chunk disagrees with its plane or profile".into(),
                ));
            }
            targets.check_palette(&chunk)?;
            checked += 1;
        }
        if checked != summary.chunks {
            return Err(Error::Invalid("a chunk names an unknown plane".into()));
        }
        Ok(summary)
    }

    /// Checks string, plane, zone, anchor and definition rows on their own
    /// and against each other, without loading them.
    fn check_rows(&self) -> Result<(), Error> {
        let checks = [
            (
                format!(
                    "SELECT count(*) FROM strings WHERE typeof(value) != 'text' OR octet_length(value) > {} OR instr(value, char(0)) != 0",
                    maximum_string_bytes()
                ),
                "a string is not text of at most 1 MiB without NUL",
            ),
            (
                "SELECT count(*) FROM strings a JOIN strings b ON b.id = a.id + 1 WHERE b.value <= a.value".to_owned(),
                "string IDs are not in byte order of their values",
            ),
            (
                format!(
                    "SELECT count(*) FROM planes WHERE {} OR kind NOT IN ('shared', 'instanced', 'per_player') OR typeof(chunk_size) != 'integer' OR chunk_size != {CHUNK_SIZE}",
                    not_identifier("id")
                ),
                "a plane has an invalid ID or kind, or a chunk size other than 32",
            ),
            (
                format!(
                    "SELECT count(*) FROM zones z WHERE {} OR NOT EXISTS (SELECT 1 FROM planes p WHERE p.id = z.plane) OR (z.parent IS NOT NULL AND NOT EXISTS (SELECT 1 FROM zones q WHERE q.plane = z.plane AND q.id = z.parent))",
                    not_identifier("z.id")
                ),
                "a zone has an invalid ID or names an unknown plane or parent",
            ),
            (
                // Zones reached from a root by following children: with one
                // parent each, a zone is reached once or, on or below a
                // cycle, never.
                "WITH RECURSIVE children(plane, parent, id) AS MATERIALIZED \
                 (SELECT plane, parent, id FROM zones WHERE parent IS NOT NULL), \
                 rooted(plane, id) AS (SELECT plane, id FROM zones WHERE parent IS NULL \
                 UNION ALL SELECT c.plane, c.id FROM rooted r JOIN children c ON c.plane = r.plane AND c.parent = r.id) \
                 SELECT (SELECT count(*) FROM zones) - (SELECT count(*) FROM rooted)"
                    .to_owned(),
                "zone parents form a cycle",
            ),
            (
                format!(
                    "SELECT count(*) FROM anchors a WHERE {} OR NOT EXISTS (SELECT 1 FROM planes p WHERE p.id = a.plane) OR typeof(a.layer) != 'integer' OR a.layer NOT BETWEEN -2147483648 AND 2147483647 OR typeof(a.x) != 'integer' OR a.x NOT BETWEEN -{MAX_SAFE_INTEGER} AND {MAX_SAFE_INTEGER} OR typeof(a.y) != 'integer' OR a.y NOT BETWEEN -{MAX_SAFE_INTEGER} AND {MAX_SAFE_INTEGER}",
                    not_identifier("a.id")
                ),
                "an anchor has an invalid ID, layer or position, or names an unknown plane",
            ),
            (
                // Chunk `c` covers tiles `c * size ..= c * size + size - 1`.
                format!(
                    "SELECT count(*) FROM chunks c LEFT JOIN planes p ON p.id = c.plane WHERE p.id IS NULL \
                     OR typeof(c.layer) != 'integer' OR c.layer NOT BETWEEN -2147483648 AND 2147483647 \
                     OR typeof(c.cx) != 'integer' OR typeof(c.cy) != 'integer' \
                     OR c.cx NOT BETWEEN -({MAX_SAFE_INTEGER} / p.chunk_size) AND ({MAX_SAFE_INTEGER} - p.chunk_size + 1) / p.chunk_size \
                     OR c.cy NOT BETWEEN -({MAX_SAFE_INTEGER} / p.chunk_size) AND ({MAX_SAFE_INTEGER} - p.chunk_size + 1) / p.chunk_size"
                ),
                "a chunk names an unknown plane or has an invalid layer or position",
            ),
            (
                format!(
                    "SELECT count(*) FROM definitions WHERE {} OR {}",
                    not_identifier("type"),
                    not_identifier("id")
                ),
                "a definition type or ID is not an identifier",
            ),
        ];
        for (query, problem) in checks {
            let count: i64 = self.connection.query_row(&query, [], |row| row.get(0))?;
            if count != 0 {
                return Err(Error::Invalid(problem.into()));
            }
        }
        Ok(())
    }

    /// Checks that every string is UTF-8, one bounded row at a time; SQLite
    /// accepts any bytes as text.
    fn check_string_encoding(&self) -> Result<(), Error> {
        let mut statement = self
            .connection
            .prepare("SELECT value FROM strings ORDER BY id")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            match row.get_ref(0)? {
                types::ValueRef::Text(bytes) if std::str::from_utf8(bytes).is_ok() => {}
                _ => return Err(Error::Invalid("a string is not UTF-8 text".into())),
            }
        }
        Ok(())
    }

    /// Checks that every `document` column holds canonical JSON of an
    /// object whose identity keys agree with its row.
    fn check_documents(&self) -> Result<(), Error> {
        let limit = maximum_document_bytes();
        let document = bounded_text("document");
        let mut planes = self.connection.prepare(&format!(
            "SELECT id, kind, chunk_size, {document} FROM planes ORDER BY id"
        ))?;
        let mut has_zone = self
            .connection
            .prepare("SELECT EXISTS (SELECT 1 FROM zones WHERE plane = ?1 AND id = ?2)")?;
        let mut rows = planes.query([limit])?;
        while let Some(row) = rows.next()? {
            let (id, kind, chunk_size): (String, String, i64) =
                (row.get(0)?, row.get(1)?, row.get(2)?);
            let stored = stored_document(row.get(3)?, "a plane document")?;
            let default_zone_ok = match stored.root().get("default_zone") {
                None => true,
                Some(zone) => match zone.as_str() {
                    Some(zone) => has_zone.query_row(params![id, zone], |row| row.get(0))?,
                    None => false,
                },
            };
            if text(&stored, "format") != Some("tilewright.plane/1")
                || text(&stored, "id") != Some(&id)
                || text(&stored, "kind") != Some(&kind)
                || stored.root().get("chunk_size").and_then(Value::as_i64) != Some(chunk_size)
                || !default_zone_ok
            {
                return Err(Error::Invalid(format!(
                    "the document of plane {id} disagrees with its row"
                )));
            }
        }
        let mut zones = self.connection.prepare(&format!(
            "SELECT plane, id, parent, {document} FROM zones ORDER BY plane, id"
        ))?;
        let mut rows = zones.query([limit])?;
        while let Some(row) = rows.next()? {
            let (plane, id, parent): (String, String, Option<String>) =
                (row.get(0)?, row.get(1)?, row.get(2)?);
            let stored = stored_document(row.get(3)?, "a zone document")?;
            if text(&stored, "id") != Some(&id)
                || stored.root().get("parent").map(Value::as_str) != parent.as_deref().map(Some)
            {
                return Err(Error::Invalid(format!(
                    "the document of zone {plane}/{id} disagrees with its row"
                )));
            }
        }
        let mut definitions = self.connection.prepare(&format!(
            "SELECT type, id, {document} FROM definitions ORDER BY type, id"
        ))?;
        let mut rows = definitions.query([limit])?;
        while let Some(row) = rows.next()? {
            let (kind, id): (String, String) = (row.get(0)?, row.get(1)?);
            let stored = stored_document(row.get(2)?, "a definition document")?;
            if text(&stored, "format") != Some("tilewright.def/1")
                || text(&stored, "type") != Some(&kind)
                || text(&stored, "id") != Some(&id)
            {
                return Err(Error::Invalid(format!(
                    "the document of definition {kind}/{id} disagrees with its row"
                )));
            }
        }
        Ok(())
    }

    /// Render and ground layer counts from the stored profile, which must be
    /// canonical and agree with `profile_id` and `profile_version`.
    fn profile_layers(&self) -> Result<(usize, usize), Error> {
        let profile = stored_document(self.meta("profile")?, "the stored profile")?;
        // The identity must be one a source profile can have: an identifier
        // and a version of at most 64 characters.
        let identity_ok = text(&profile, "id").is_some_and(is_identifier)
            && text(&profile, "version").is_some_and(|version| version.chars().count() <= 64);
        if text(&profile, "format") != Some("tilewright.profile/1")
            || !identity_ok
            || text(&profile, "id") != self.meta("profile_id")?.as_deref()
            || text(&profile, "version") != self.meta("profile_version")?.as_deref()
        {
            return Err(Error::Invalid(
                "the stored profile disagrees with profile_id or profile_version".into(),
            ));
        }
        // Each list holds unique identifiers within its count, and every
        // ground layer is a render layer other than `height` or `zone`.
        let layers = |key: &str, range: std::ops::RangeInclusive<usize>| {
            let names: Option<Vec<&str>> = profile
                .root()
                .get(key)
                .and_then(Value::as_array)
                .filter(|items| range.contains(&items.len()))
                .and_then(|items| items.iter().map(Value::as_str).collect());
            names
                .filter(|names| {
                    let unique: BTreeSet<&str> = names.iter().copied().collect();
                    unique.len() == names.len() && names.iter().all(|name| is_identifier(name))
                })
                .ok_or_else(|| {
                    Error::Invalid(format!(
                        "stored profile does not list {}..={} unique {key}",
                        range.start(),
                        range.end()
                    ))
                })
        };
        let render = layers("render_layers", 1..=usize::from(u16::MAX - 1))?;
        let ground = layers("ground_layers", 0..=MAXIMUM_GROUND_LAYERS)?;
        let known: BTreeSet<&str> = render.iter().copied().collect();
        if ground
            .iter()
            .any(|name| !known.contains(name) || ["height", "zone"].contains(name))
        {
            return Err(Error::Invalid(
                "a stored ground layer is not a render layer other than height or zone".into(),
            ));
        }
        Ok((render.len(), ground.len()))
    }
}

/// Checks the schema, application ID, user version and `meta` of a pack
/// before any other query runs.
fn check_identity(connection: &Connection) -> Result<(), Error> {
    // At most one row more than expected, each value bounded, is loaded.
    let mut schema = connection.prepare(&format!(
        "SELECT {}, {}, {} FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY name LIMIT {}",
        bounded_text("type"),
        bounded_text("name"),
        bounded_text("sql"),
        SCHEMA_TABLES.len() + 1
    ))?;
    let found: Vec<(Option<String>, Option<String>, Option<String>)> = schema
        .query_map([bound(MAXIMUM_SCHEMA_BYTES)], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<Result<_, _>>()?;
    drop(schema);
    let mut expected: Vec<(Option<String>, Option<String>, Option<String>)> = SCHEMA_TABLES
        .iter()
        .map(|(name, sql)| {
            (
                Some("table".to_owned()),
                Some((*name).to_owned()),
                Some((*sql).to_owned()),
            )
        })
        .collect();
    expected.sort_by(|left, right| left.1.cmp(&right.1));
    if found != expected {
        return Err(Error::Invalid(
            "the pack schema does not match this format".into(),
        ));
    }
    let application: i32 = connection.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    let version: i32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let page_size: i64 = connection.query_row("PRAGMA page_size", [], |row| row.get(0))?;
    if application != APPLICATION_ID || version != USER_VERSION || page_size != PAGE_SIZE {
        return Err(Error::Invalid(format!(
            "not a Tilewright pack of user_version {USER_VERSION} with {PAGE_SIZE}-byte pages"
        )));
    }
    let meta = read_meta(connection)?;
    let value = |key: &str| {
        meta.iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    };
    let digest_ok = value("source_digest").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    });
    if value("format") != Some(FORMAT)
        || value("chunk_encoding") != Some(CHUNK_ENCODING)
        || !digest_ok
    {
        return Err(Error::Invalid(format!(
            "the pack's metadata is not that of {FORMAT}"
        )));
    }
    Ok(())
}

/// Reads `meta` after checking, without loading any value, that it holds
/// exactly the format's keys with text values of bounded size.
fn read_meta(connection: &Connection) -> Result<Vec<(String, String)>, Error> {
    let keys = META_KEYS
        .iter()
        .map(|key| format!("'{key}'"))
        .collect::<Vec<_>>()
        .join(", ");
    let (total, known, oversized): (i64, i64, i64) = connection.query_row(
        &format!(
            "SELECT count(*), count(*) FILTER (WHERE key IN ({keys})), \
             count(*) FILTER (WHERE typeof(value) != 'text' OR octet_length(value) > CASE key WHEN 'profile' THEN ?1 ELSE ?2 END) \
             FROM meta"
        ),
        params![maximum_document_bytes(), bound(MAXIMUM_META_BYTES)],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let expected = i64::try_from(META_KEYS.len()).unwrap_or(i64::MAX);
    if total != expected || known != expected || oversized != 0 {
        return Err(Error::Invalid(format!(
            "the pack's metadata is not that of {FORMAT}"
        )));
    }
    let mut statement = connection.prepare("SELECT key, value FROM meta ORDER BY key")?;
    let meta = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<Result<_, _>>()?;
    Ok(meta)
}

/// What chunk records may reference. Strings are checked by ID, since
/// verification has proven IDs are `1..=strings`. Every other lookup is
/// cached per distinct ID, so checking an object costs map lookups rather
/// than queries, and verification work stays proportional to the decoded
/// bytes its budget bounds.
struct Targets<'c> {
    strings: u32,
    render_layers: usize,
    /// Whether string `?1` names a plane.
    is_plane: Statement<'c>,
    /// Whether string `?1` names a `proto` definition.
    is_proto: Statement<'c>,
    /// Whether string `?1` names a zone of plane `?2`.
    is_zone: Statement<'c>,
    /// Whether plane `?1` (a string ID, or 0 for plane `?2`) has the anchor
    /// named by string `?3`.
    has_anchor: Statement<'c>,
    /// The value of string `?1`.
    value: Statement<'c>,
    planes: BTreeMap<u32, bool>,
    protos: BTreeMap<u32, bool>,
    field_names: BTreeMap<u32, bool>,
    /// Anchors by `(plane, anchor)` for portals that name their plane.
    anchors: BTreeMap<(u32, u32), bool>,
    /// The plane of the chunks being checked, and the lookups relative to
    /// it: its zones and its own anchors.
    current: String,
    zones: BTreeMap<u32, bool>,
    own_anchors: BTreeMap<u32, bool>,
}

/// A cached boolean lookup.
fn cached<K: Ord + Copy>(
    cache: &mut BTreeMap<K, bool>,
    key: K,
    lookup: impl FnOnce() -> Result<bool, Error>,
) -> Result<bool, Error> {
    if let Some(known) = cache.get(&key) {
        return Ok(*known);
    }
    let found = lookup()?;
    cache.insert(key, found);
    Ok(found)
}

impl Targets<'_> {
    fn string(&self, id: u32) -> bool {
        (1..=self.strings).contains(&id)
    }

    /// Starts checking chunks of `plane`, dropping lookups relative to the
    /// previous one.
    fn enter(&mut self, plane: &str) {
        if self.current != plane {
            plane.clone_into(&mut self.current);
            self.zones.clear();
            self.own_anchors.clear();
        }
    }

    fn proto(&mut self, id: u32) -> Result<bool, Error> {
        if !self.string(id) {
            return Ok(false);
        }
        let statement = &mut self.is_proto;
        cached(&mut self.protos, id, || {
            Ok(statement.query_row([id], |row| row.get(0))?)
        })
    }

    fn field_name(&mut self, id: u32) -> Result<bool, Error> {
        if !self.string(id) {
            return Ok(false);
        }
        let statement = &mut self.value;
        cached(&mut self.field_names, id, || {
            let name: String = statement.query_row([id], |row| row.get(0))?;
            Ok(is_field_name(&name))
        })
    }

    /// Checks that every palette entry is used, that ground cells name
    /// `proto` definitions, and that zone cells name zones of the current
    /// plane.
    fn check_palette(&mut self, chunk: &ChunkData) -> Result<(), Error> {
        fn mark(cells: &[u16], used: &mut [bool]) {
            for cell in cells {
                if let Some(flag) = usize::from(*cell)
                    .checked_sub(1)
                    .and_then(|index| used.get_mut(index))
                {
                    *flag = true;
                }
            }
        }
        let mut ground = vec![false; chunk.palette.len()];
        let mut zone = vec![false; chunk.palette.len()];
        for grid in &chunk.ground {
            mark(grid, &mut ground);
        }
        mark(&chunk.zones, &mut zone);
        for ((id, ground), zone) in chunk.palette.iter().zip(ground).zip(zone) {
            let id = *id;
            let mut ok = (ground || zone) && self.string(id) && (!ground || self.proto(id)?);
            if ok && zone {
                let (statement, plane) = (&mut self.is_zone, &self.current);
                ok = cached(&mut self.zones, id, || {
                    Ok(statement.query_row(params![id, plane], |row| row.get(0))?)
                })?;
            }
            if !ok {
                return Err(Error::Invalid(
                    "a palette entry is unused or not a proto definition or zone of its plane"
                        .into(),
                ));
            }
        }
        Ok(())
    }

    fn portal(&mut self, portal: Option<&PortalRecord>) -> Result<bool, Error> {
        Ok(match portal {
            Some(PortalRecord::Location { x, y, .. })
                if [x, y]
                    .iter()
                    .any(|value| !(-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(*value)) =>
            {
                false
            }
            Some(PortalRecord::Location { plane: 0, .. }) | None => true,
            Some(PortalRecord::Location { plane, .. }) => {
                let plane = *plane;
                if !self.string(plane) {
                    return Ok(false);
                }
                let statement = &mut self.is_plane;
                cached(&mut self.planes, plane, || {
                    Ok(statement.query_row([plane], |row| row.get(0))?)
                })?
            }
            Some(PortalRecord::Anchor { plane, anchor }) => {
                let (plane, anchor) = (*plane, *anchor);
                if !(plane == 0 || self.string(plane)) || !self.string(anchor) {
                    return Ok(false);
                }
                let (statement, current) = (&mut self.has_anchor, &self.current);
                let lookup =
                    || Ok(statement.query_row(params![plane, current, anchor], |row| row.get(0))?);
                if plane == 0 {
                    cached(&mut self.own_anchors, anchor, lookup)?
                } else {
                    cached(&mut self.anchors, (plane, anchor), lookup)?
                }
            }
        })
    }

    fn check(&mut self, object: &ObjectRecord) -> Result<(), Error> {
        let mut ok = self.proto(object.proto)?
            && usize::from(object.layer) <= self.render_layers
            && self.portal(object.portal.as_ref())?;
        // Field names are field-name strings, each at most once per object.
        let mut names = BTreeSet::new();
        for (name, value) in &object.fields {
            ok &= names.insert(*name) && self.field_name(*name)?;
            match value {
                FieldRecord::String(id) => ok &= self.string(*id),
                FieldRecord::Integer(number) => {
                    ok &= (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(number);
                }
                _ => {}
            }
        }
        if ok {
            Ok(())
        } else {
            Err(Error::Invalid(
                "an object references a missing string, layer, plane or anchor, repeats or misnames a field, or has a portal position or integer field out of range".into(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Pack;
    use std::path::Path;

    #[test]
    fn packs_keep_the_absolute_path_they_opened() {
        // Unit tests run in the package directory, so this path is relative.
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
        std::fs::create_dir_all(&target).unwrap();
        let directory = tempfile::tempdir_in(&target).unwrap();
        let world = tilewright_source::load(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/basic"),
        )
        .unwrap();
        let written = crate::write_pack(&world, directory.path()).unwrap();
        let relative = Path::new("../../target")
            .join(directory.path().file_name().unwrap())
            .join(written.path.file_name().unwrap());
        let pack = Pack::open(&relative).unwrap();
        let path = pack.path.as_ref().unwrap();
        assert!(path.is_absolute(), "{}", path.display());
        pack.verify().unwrap();
    }
}
