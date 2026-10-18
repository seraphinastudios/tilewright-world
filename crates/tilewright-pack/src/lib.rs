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
    CHUNK_ENCODING, ChunkData, DecodeError, FieldRecord, MAXIMUM_DEPTH, MAXIMUM_RECORD_BYTES,
    ObjectRecord, PortalRecord, ZSTD_LEVEL, decode_chunk, decode_record, encode_chunk,
    encode_record,
};
pub use compile::{COMPILER, Written, compile, write_pack};
pub use read::{Pack, Summary};

use std::fmt;

/// The `meta.format` value.
pub const FORMAT: &str = "tilewright.pack/1";

/// SQLite `application_id` of a pack: `"TWP1"`.
pub const APPLICATION_ID: i32 = 0x5457_5031;

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
