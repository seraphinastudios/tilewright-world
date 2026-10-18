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
        CHUNK_ENCODING, ChunkData, FieldRecord, MAXIMUM_CHUNK_BYTES, ObjectRecord, PortalRecord,
        decode_chunk,
    },
};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Statement, config::DbConfig, params, types,
};
use sha2::{Digest, Sha256};
use std::{
    fmt::Write as _,
    io::Read as _,
    path::{Path, PathBuf},
};
use tilewright_jsonc::{Document, Limits, Value};

/// Largest `meta` value other than `profile`.
const MAXIMUM_META_BYTES: usize = 1024;

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
    path: PathBuf,
}

/// Percent-encodes a path for an SQLite URI. On Unix the path's bytes are
/// encoded as they are, so paths that are not UTF-8 open too.
fn uri(path: &Path) -> Result<String, Error> {
    let absolute = std::path::absolute(path)?;
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
    format!(
        "(typeof({column}) != 'text' OR octet_length({column}) NOT BETWEEN 1 AND 128 \
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

/// Decodes the value of [`chunk_data_sql`].
fn chunk_data(value: types::Value) -> Result<ChunkData, Error> {
    match value {
        types::Value::Blob(data) => decode_chunk(&data).map_err(Error::Chunk),
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
        if !std::fs::metadata(path)?.is_file() {
            return Err(Error::Invalid(format!("{} is not a file", path.display())));
        }
        let connection = Connection::open_with_flags(
            uri(path)?,
            OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
        connection.set_db_config(DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA, false)?;
        connection.execute_batch("PRAGMA query_only = ON")?;
        let mut schema = connection.prepare(
            "SELECT type, name, sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY name",
        )?;
        let found: Vec<(String, String, String)> = schema
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<Result<_, _>>()?;
        drop(schema);
        let mut expected: Vec<(String, String, String)> = SCHEMA_TABLES
            .iter()
            .map(|(name, sql)| ("table".to_owned(), (*name).to_owned(), (*sql).to_owned()))
            .collect();
        expected.sort_by(|left, right| left.1.cmp(&right.1));
        if found != expected {
            return Err(Error::Invalid(
                "the pack schema does not match this format".into(),
            ));
        }
        let application: i32 =
            connection.query_row("PRAGMA application_id", [], |row| row.get(0))?;
        let version: i32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if application != APPLICATION_ID || version != USER_VERSION {
            return Err(Error::Invalid(format!(
                "not a Tilewright pack of user_version {USER_VERSION}"
            )));
        }
        let meta = read_meta(&connection)?;
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
        Ok(Self {
            connection,
            path: path.to_path_buf(),
        })
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

    /// Decodes one chunk.
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

    /// The SHA-256 of the pack file.
    ///
    /// # Errors
    /// Returns an error if the file cannot be read.
    pub fn file_digest(&self) -> Result<[u8; 32], Error> {
        let mut file = std::fs::File::open(&self.path)?;
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

    /// Checks the whole pack: SQLite integrity, the file name digest when the
    /// name is one, string, plane, zone, anchor and definition rows, every
    /// stored document, every chunk record, and every string, layer, ground
    /// count and portal target a record references.
    ///
    /// # Errors
    /// Returns the first problem found.
    pub fn verify(&self) -> Result<Summary, Error> {
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
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".twpack"))
            .filter(|stem| stem.len() == 64 && stem.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            let digest = crate::compile::hex(&self.file_digest()?);
            if digest != stem.to_ascii_lowercase() {
                return Err(Error::Invalid(format!(
                    "file digest {digest} does not match its name"
                )));
            }
        }
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
        self.check_documents()?;
        let (render_layers, ground_layers) = self.profile_layers()?;
        let mut targets = Targets {
            strings: u32::try_from(summary.strings)
                .map_err(|_| Error::Invalid("too many strings".into()))?,
            render_layers,
            is_plane: self.connection.prepare(
                "SELECT EXISTS (SELECT 1 FROM strings s JOIN planes p ON p.id = s.value WHERE s.id = ?1)",
            )?,
            has_anchor: self.connection.prepare(
                "SELECT EXISTS (SELECT 1 FROM strings n JOIN anchors a ON a.id = n.value \
                 WHERE n.id = ?3 AND a.plane = CASE WHEN ?1 = 0 THEN ?2 ELSE (SELECT value FROM strings WHERE id = ?1) END)",
            )?,
        };
        let mut statement = self.connection.prepare(&format!(
            "SELECT p.id, p.chunk_size, {} FROM chunks c JOIN planes p ON p.id = c.plane ORDER BY c.plane, c.layer, c.cx, c.cy",
            chunk_data_sql()
        ))?;
        let mut rows = statement.query([])?;
        let mut checked = 0;
        while let Some(row) = rows.next()? {
            let plane: String = row.get(0)?;
            let chunk_size: u16 = row.get(1)?;
            let chunk = chunk_data(row.get(2)?)?;
            if chunk.chunk_size != chunk_size || chunk.ground.len() != ground_layers {
                return Err(Error::Invalid(
                    "a chunk disagrees with its plane or profile".into(),
                ));
            }
            if chunk
                .palette
                .iter()
                .any(|id| *id == 0 || *id > targets.strings)
            {
                return Err(Error::Invalid("a palette entry is not a string".into()));
            }
            let mut stack: Vec<&ObjectRecord> = chunk.objects.iter().collect();
            while let Some(object) = stack.pop() {
                summary.objects += 1;
                targets.check(object, &plane)?;
                stack.extend(&object.inventory);
            }
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
                    "SELECT count(*) FROM strings WHERE typeof(value) != 'text' OR octet_length(value) > {}",
                    maximum_string_bytes()
                ),
                "a string is not text of at most 1 MiB",
            ),
            (
                format!(
                    "SELECT count(*) FROM planes WHERE {} OR kind NOT IN ('shared', 'instanced', 'per_player') OR typeof(chunk_size) != 'integer' OR chunk_size NOT BETWEEN 1 AND 255",
                    not_identifier("id")
                ),
                "a plane has an invalid ID, kind or chunk size",
            ),
            (
                format!(
                    "SELECT count(*) FROM zones z WHERE {} OR NOT EXISTS (SELECT 1 FROM planes p WHERE p.id = z.plane) OR (z.parent IS NOT NULL AND NOT EXISTS (SELECT 1 FROM zones q WHERE q.plane = z.plane AND q.id = z.parent))",
                    not_identifier("z.id")
                ),
                "a zone has an invalid ID or names an unknown plane or parent",
            ),
            (
                format!(
                    "SELECT count(*) FROM anchors a WHERE {} OR NOT EXISTS (SELECT 1 FROM planes p WHERE p.id = a.plane) OR typeof(a.layer) != 'integer' OR a.layer NOT BETWEEN -2147483648 AND 2147483647",
                    not_identifier("a.id")
                ),
                "an anchor has an invalid ID or layer or names an unknown plane",
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

    /// Checks that every `document` column holds canonical JSON of an
    /// object whose identity keys agree with its row.
    fn check_documents(&self) -> Result<(), Error> {
        let limit = maximum_document_bytes();
        let document = bounded_text("document");
        let mut planes = self.connection.prepare(&format!(
            "SELECT id, kind, chunk_size, {document} FROM planes ORDER BY id"
        ))?;
        let mut rows = planes.query([limit])?;
        while let Some(row) = rows.next()? {
            let (id, kind, chunk_size): (String, String, i64) =
                (row.get(0)?, row.get(1)?, row.get(2)?);
            let stored = stored_document(row.get(3)?, "a plane document")?;
            if text(&stored, "format") != Some("tilewright.plane/1")
                || text(&stored, "id") != Some(&id)
                || text(&stored, "kind") != Some(&kind)
                || stored.root().get("chunk_size").and_then(Value::as_i64) != Some(chunk_size)
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
        if text(&profile, "format") != Some("tilewright.profile/1")
            || text(&profile, "id") != self.meta("profile_id")?.as_deref()
            || text(&profile, "version") != self.meta("profile_version")?.as_deref()
        {
            return Err(Error::Invalid(
                "the stored profile disagrees with profile_id or profile_version".into(),
            ));
        }
        let count = |key: &str| {
            profile
                .root()
                .get(key)
                .and_then(Value::as_array)
                .map(<[_]>::len)
                .ok_or_else(|| Error::Invalid(format!("stored profile has no {key}")))
        };
        Ok((count("render_layers")?, count("ground_layers")?))
    }
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
/// verification has proven IDs are `1..=strings`; planes and anchors are
/// looked up per portal.
struct Targets<'c> {
    strings: u32,
    render_layers: usize,
    /// Whether string `?1` names a plane.
    is_plane: Statement<'c>,
    /// Whether plane `?1` (a string ID, or 0 for plane `?2`) has the anchor
    /// named by string `?3`.
    has_anchor: Statement<'c>,
}

impl Targets<'_> {
    fn string(&self, id: u32) -> bool {
        (1..=self.strings).contains(&id)
    }

    fn check(&mut self, object: &ObjectRecord, current: &str) -> Result<(), Error> {
        let mut ok = self.string(object.proto) && usize::from(object.layer) <= self.render_layers;
        ok &= match &object.portal {
            Some(PortalRecord::Location { plane: 0, .. }) | None => true,
            Some(PortalRecord::Location { plane, .. }) => self
                .is_plane
                .query_row([plane], |row| row.get::<_, bool>(0))?,
            Some(PortalRecord::Anchor { plane, anchor }) => {
                (*plane == 0 || self.string(*plane))
                    && self.string(*anchor)
                    && self
                        .has_anchor
                        .query_row(params![plane, current, anchor], |row| row.get::<_, bool>(0))?
            }
        };
        for (name, value) in &object.fields {
            ok &= self.string(*name);
            if let FieldRecord::String(id) = value {
                ok &= self.string(*id);
            }
        }
        if ok {
            Ok(())
        } else {
            Err(Error::Invalid(
                "an object references a missing string, layer, plane or anchor".into(),
            ))
        }
    }
}
