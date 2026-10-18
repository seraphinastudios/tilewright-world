// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Reading and verifying packs. A pack is untrusted input: it is opened
//! read-only and immutable, in SQLite's defensive mode, and its schema must
//! match this version exactly before any other query runs.

use crate::{
    APPLICATION_ID, Error, FORMAT, SCHEMA_TABLES,
    chunk::{CHUNK_ENCODING, ChunkData, FieldRecord, ObjectRecord, PortalRecord, decode_chunk},
};
use rusqlite::{Connection, OpenFlags, OptionalExtension, config::DbConfig, params};
use sha2::{Digest, Sha256};
use std::{
    fmt::Write as _,
    path::{Path, PathBuf},
};
use tilewright_jsonc::{Document, Limits};

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

/// Percent-encodes a path for an SQLite URI.
fn uri(path: &Path) -> Result<String, Error> {
    let absolute = std::path::absolute(path)?;
    let text = absolute
        .to_str()
        .ok_or_else(|| Error::Invalid("pack path is not UTF-8".into()))?;
    let mut encoded = String::from("file:");
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-_.~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded.push_str("?immutable=1");
    Ok(encoded)
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
        if application != APPLICATION_ID {
            return Err(Error::Invalid("not a Tilewright pack".into()));
        }
        let pack = Self {
            connection,
            path: path.to_path_buf(),
        };
        if pack.meta("format")?.as_deref() != Some(FORMAT)
            || pack.meta("chunk_encoding")?.as_deref() != Some(CHUNK_ENCODING)
        {
            return Err(Error::Invalid(format!("the pack is not {FORMAT}")));
        }
        Ok(pack)
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
    /// Returns an error if SQLite fails.
    pub fn string(&self, id: u32) -> Result<Option<String>, Error> {
        Ok(self
            .connection
            .query_row("SELECT value FROM strings WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .optional()?)
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
        let data: Option<Vec<u8>> = self
            .connection
            .query_row(
                "SELECT data FROM chunks WHERE plane = ?1 AND layer = ?2 AND cx = ?3 AND cy = ?4",
                params![plane, layer, cx, cy],
                |row| row.get(0),
            )
            .optional()?;
        data.map(|data| decode_chunk(&data).map_err(Error::Chunk))
            .transpose()
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
        Ok(Sha256::digest(std::fs::read(&self.path)?).into())
    }

    /// Checks the whole pack: SQLite integrity, the file name digest when the
    /// name is one, every chunk record, and every string, layer and ground
    /// count a record references.
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
        let strings = u32::try_from(summary.strings)
            .map_err(|_| Error::Invalid("too many strings".into()))?;
        let (render_layers, ground_layers) = self.profile_layers()?;
        let mut statement = self
            .connection
            .prepare("SELECT c.encoding, c.data, p.chunk_size FROM chunks c JOIN planes p ON p.id = c.plane ORDER BY c.plane, c.layer, c.cx, c.cy")?;
        let mut rows = statement.query([])?;
        let mut checked = 0;
        while let Some(row) = rows.next()? {
            let encoding: String = row.get(0)?;
            let data: Vec<u8> = row.get(1)?;
            let chunk_size: u16 = row.get(2)?;
            if encoding != CHUNK_ENCODING {
                return Err(Error::Invalid(format!("unknown chunk encoding {encoding}")));
            }
            let chunk = decode_chunk(&data).map_err(Error::Chunk)?;
            if chunk.chunk_size != chunk_size || chunk.ground.len() != ground_layers {
                return Err(Error::Invalid(
                    "a chunk disagrees with its plane or profile".into(),
                ));
            }
            if chunk.palette.iter().any(|id| *id == 0 || *id > strings) {
                return Err(Error::Invalid("a palette entry is not a string".into()));
            }
            let mut stack: Vec<&ObjectRecord> = chunk.objects.iter().collect();
            while let Some(object) = stack.pop() {
                summary.objects += 1;
                check_object(object, strings, render_layers)?;
                stack.extend(&object.inventory);
            }
            checked += 1;
        }
        if checked != summary.chunks {
            return Err(Error::Invalid("a chunk names an unknown plane".into()));
        }
        Ok(summary)
    }

    /// Render and ground layer counts from the stored profile.
    fn profile_layers(&self) -> Result<(usize, usize), Error> {
        let profile = self
            .meta("profile")?
            .ok_or_else(|| Error::Invalid("the pack has no profile".into()))?;
        let document = Document::parse(profile, &Limits::default())
            .map_err(|error| Error::Invalid(format!("stored profile: {error}")))?;
        let count = |key: &str| {
            document
                .root()
                .get(key)
                .and_then(tilewright_jsonc::Value::as_array)
                .map(<[_]>::len)
                .ok_or_else(|| Error::Invalid(format!("stored profile has no {key}")))
        };
        Ok((count("render_layers")?, count("ground_layers")?))
    }
}

fn check_object(object: &ObjectRecord, strings: u32, render_layers: usize) -> Result<(), Error> {
    let string = |id: u32| id >= 1 && id <= strings;
    let optional = |id: u32| id <= strings;
    let mut ok = string(object.proto) && usize::from(object.layer) <= render_layers;
    ok &= match &object.portal {
        Some(PortalRecord::Location { plane, .. }) => optional(*plane),
        Some(PortalRecord::Anchor { plane, anchor }) => optional(*plane) && string(*anchor),
        None => true,
    };
    for (name, value) in &object.fields {
        ok &= string(*name);
        if let FieldRecord::String(id) = value {
            ok &= string(*id);
        }
    }
    if ok {
        Ok(())
    } else {
        Err(Error::Invalid(
            "an object references a missing string or layer".into(),
        ))
    }
}
