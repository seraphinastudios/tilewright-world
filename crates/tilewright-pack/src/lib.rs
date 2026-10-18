// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Tilewright content packs (`docs/spec/PACK.md`): one SQLite file compiled
//! deterministically from a validated world, named by its SHA-256 digest.
//!
//! ```no_run
//! # use std::path::Path;
//! let world = tilewright_source::load(Path::new("content")).expect("valid source");
//! let written = tilewright_pack::write_pack(&world, Path::new("dist"))?;
//! let pack = tilewright_pack::Pack::open(&written.path)?;
//! let summary = pack.verify()?;
//! println!("{} chunks", summary.chunks);
//! # Ok::<(), tilewright_pack::Error>(())
//! ```

mod chunk;
mod compile;
mod read;

pub use chunk::{
    CHUNK_ENCODING, ChunkData, DecodeError, FieldRecord, MAXIMUM_CHUNK_BYTES, MAXIMUM_DEPTH,
    MAXIMUM_RECORD_BYTES, ObjectRecord, PortalRecord, ZSTD_LEVEL, decode_chunk, decode_record,
    encode_chunk, encode_record,
};
pub use compile::{COMPILER, Written, compile, write_pack};
pub use read::{DEFAULT_MAXIMUM_DECODED_BYTES, Pack, Summary, VerifyLimits};

use std::fmt;

/// The `meta.format` value.
pub const FORMAT: &str = "tilewright.pack/1";

/// SQLite `application_id` of a pack: `"TWP1"`.
pub const APPLICATION_ID: i32 = 0x5457_5031;

/// SQLite `user_version` of a pack of this format.
pub const USER_VERSION: i32 = 1;

/// Every `meta` key, in byte order. A pack has exactly these.
pub const META_KEYS: [&str; 7] = [
    "chunk_encoding",
    "compiler",
    "format",
    "profile",
    "profile_id",
    "profile_version",
    "source_digest",
];

/// The pack tables, by name, exactly as created.
pub const SCHEMA_TABLES: [(&str, &str); 7] = [
    (
        "meta",
        "CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) WITHOUT ROWID",
    ),
    (
        "strings",
        "CREATE TABLE strings (id INTEGER PRIMARY KEY, value TEXT NOT NULL UNIQUE)",
    ),
    (
        "planes",
        "CREATE TABLE planes (id TEXT PRIMARY KEY, kind TEXT NOT NULL, chunk_size INTEGER NOT NULL, document TEXT NOT NULL) WITHOUT ROWID",
    ),
    (
        "zones",
        "CREATE TABLE zones (plane TEXT NOT NULL, id TEXT NOT NULL, parent TEXT, document TEXT NOT NULL, PRIMARY KEY (plane, id)) WITHOUT ROWID",
    ),
    (
        "anchors",
        "CREATE TABLE anchors (plane TEXT NOT NULL, id TEXT NOT NULL, layer INTEGER NOT NULL, x INTEGER NOT NULL, y INTEGER NOT NULL, PRIMARY KEY (plane, id)) WITHOUT ROWID",
    ),
    (
        "chunks",
        "CREATE TABLE chunks (plane TEXT NOT NULL, layer INTEGER NOT NULL, cx INTEGER NOT NULL, cy INTEGER NOT NULL, encoding TEXT NOT NULL, data BLOB NOT NULL, PRIMARY KEY (plane, layer, cx, cy)) WITHOUT ROWID",
    ),
    (
        "definitions",
        "CREATE TABLE definitions (type TEXT NOT NULL, id TEXT NOT NULL, document TEXT NOT NULL, PRIMARY KEY (type, id)) WITHOUT ROWID",
    ),
];

/// Whether `text` matches `^[a-z0-9][a-z0-9_.-]{0,127}$`, the source
/// format's identifier pattern.
pub(crate) fn is_identifier(text: &str) -> bool {
    let bytes = text.as_bytes();
    (1..=128).contains(&bytes.len())
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_.-".contains(byte))
}

/// Whether `text` is a field name: `^[a-z][a-z0-9_]{0,63}$`.
pub(crate) fn is_field_name(text: &str) -> bool {
    let bytes = text.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
}

/// Limits for parsing canonical JSON stored in a pack. Canonical JSON is
/// one line, so the physical line limit is the input limit; every other
/// limit is the source format's, which the source document already met.
pub(crate) fn canonical_limits() -> tilewright_jsonc::Limits {
    let limits = tilewright_jsonc::Limits::default();
    tilewright_jsonc::Limits {
        max_line_bytes: limits.max_input_bytes,
        ..limits
    }
}

/// The schema as one SQL batch.
pub(crate) static SCHEMA: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    SCHEMA_TABLES
        .iter()
        .map(|(_, sql)| format!("{sql};"))
        .collect::<Vec<_>>()
        .join("\n")
});

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
    Chunk(DecodeError),
    Invalid(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "I/O error: {error}"),
            Self::Sqlite(error) => write!(formatter, "SQLite error: {error}"),
            Self::Chunk(error) => error.fmt(formatter),
            Self::Invalid(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<rusqlite::Error> for Error {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

#[cfg(test)]
mod tests;
