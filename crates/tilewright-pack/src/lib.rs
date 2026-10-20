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
    MAXIMUM_RECORD_BYTES, ObjectRecord, PortalRecord, RECORD_VERSION, ZSTD_LEVEL, decode_chunk,
    decode_record, encode_chunk, encode_record,
};
pub use compile::{COMPILER, Written, compile, write_pack};
pub use read::{
    Anchor, DEFAULT_MAXIMUM_DECODED_BYTES, DEFAULT_MAXIMUM_PACK_BYTES, Definition, Identity,
    MAXIMUM_OBJECT_INDEX, Pack, Page, Plane, StairwellTile, Summary, UidLocation, VerifyLimits,
    Zone,
};

use std::fmt;

/// The `meta.format` value.
pub const FORMAT: &str = "tilewright.pack/1";

/// SQLite `application_id` of a pack: `"TWP1"`.
pub const APPLICATION_ID: i32 = 0x5457_5031;

/// SQLite `user_version` of a pack of this format.
pub const USER_VERSION: i32 = 2;

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
pub const SCHEMA_TABLES: [(&str, &str); 10] = [
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
        "stairwells",
        "CREATE TABLE stairwells (plane TEXT NOT NULL, id TEXT NOT NULL, document TEXT NOT NULL, PRIMARY KEY (plane, id)) WITHOUT ROWID",
    ),
    (
        "stairwell_steps",
        "CREATE TABLE stairwell_steps (plane TEXT NOT NULL, layer INTEGER NOT NULL, x INTEGER NOT NULL, y INTEGER NOT NULL, stairwell TEXT NOT NULL, step INTEGER NOT NULL, proto TEXT NOT NULL, PRIMARY KEY (plane, layer, x, y)) WITHOUT ROWID",
    ),
    (
        "chunks",
        "CREATE TABLE chunks (plane TEXT NOT NULL, layer INTEGER NOT NULL, cx INTEGER NOT NULL, cy INTEGER NOT NULL, encoding TEXT NOT NULL, data BLOB NOT NULL, PRIMARY KEY (plane, layer, cx, cy)) WITHOUT ROWID",
    ),
    (
        "definitions",
        "CREATE TABLE definitions (type TEXT NOT NULL, id TEXT NOT NULL, document TEXT NOT NULL, PRIMARY KEY (type, id)) WITHOUT ROWID",
    ),
    (
        "object_uids",
        "CREATE TABLE object_uids (uid TEXT PRIMARY KEY, plane TEXT NOT NULL, layer INTEGER NOT NULL, cx INTEGER NOT NULL, cy INTEGER NOT NULL, object INTEGER NOT NULL) WITHOUT ROWID",
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

/// Whether `text` is a domain-qualified namespace for `custom` data, such as
/// `example.org` or `example.org/tool`, as the source format requires.
pub(crate) fn is_namespace(text: &str) -> bool {
    let (domain, rest) = text.split_once('/').unwrap_or((text, ""));
    domain.contains('.')
        && is_identifier(domain)
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && (rest.is_empty() || is_identifier(rest))
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

/// Stable codes for why a pack failed to open, read or verify, one per rule
/// of `docs/spec/PACK.md` section 3. The Go reader reports the same codes,
/// and the conformance fixtures in `fixtures/packs` pin them.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum ErrorCode {
    /// The file could not be read.
    Io,
    /// SQLite failed, for example on a file that is not a database.
    Sqlite,
    /// The path is not a regular file.
    NotAFile,
    /// The file is longer than the pack size limit.
    TooLarge,
    /// The file is not exactly its pages.
    FileLength,
    /// The schema is not exactly the format's.
    Schema,
    /// The application ID, user version or page size is not the format's.
    Identity,
    /// The pack has free pages.
    FreePages,
    /// `meta` does not hold exactly the format's keys and values.
    Meta,
    /// The stored profile is invalid or disagrees with `meta`.
    Profile,
    /// A string is not UTF-8 text of at most 1 MiB without NUL.
    String,
    /// String IDs are not `1..=count` in byte order of their values.
    StringIds,
    /// A plane row or document is invalid.
    Plane,
    /// A zone row or document is invalid, or zone parents form a cycle.
    Zone,
    /// An anchor row is invalid.
    Anchor,
    /// A stairwell row or document is invalid or disagrees with its steps.
    Stairwell,
    /// A `stairwell_steps` row is invalid or listed by no stairwell.
    StairwellStep,
    /// A definition row or document is invalid.
    Definition,
    /// A chunk row's key is invalid, or its record disagrees with its plane
    /// or the profile.
    Chunk,
    /// A chunk's `encoding` is not `twc1+zstd`.
    ChunkEncoding,
    /// A chunk's `data` is not a blob within the encoding's bound.
    ChunkData,
    /// A chunk's data is not a valid `twc1+zstd` record.
    ChunkRecord,
    /// A chunk record names a target that does not exist or is out of range.
    Reference,
    /// An `object_uids` row is invalid, or the table does not index exactly
    /// the uids of the chunk records.
    ObjectUid,
    /// SQLite's integrity check failed.
    Integrity,
    /// The file's SHA-256 does not match its name, or the SHA-256 of a pack
    /// opened from bytes is not the digest its caller expects.
    Digest,
    /// The chunks decode to more than the verification budget.
    DecodeBudget,
    /// The compiler cannot encode a world.
    Invalid,
}

impl ErrorCode {
    /// Every code, in declaration order.
    pub const ALL: [Self; 28] = [
        Self::Io,
        Self::Sqlite,
        Self::NotAFile,
        Self::TooLarge,
        Self::FileLength,
        Self::Schema,
        Self::Identity,
        Self::FreePages,
        Self::Meta,
        Self::Profile,
        Self::String,
        Self::StringIds,
        Self::Plane,
        Self::Zone,
        Self::Anchor,
        Self::Stairwell,
        Self::StairwellStep,
        Self::Definition,
        Self::Chunk,
        Self::ChunkEncoding,
        Self::ChunkData,
        Self::ChunkRecord,
        Self::Reference,
        Self::ObjectUid,
        Self::Integrity,
        Self::Digest,
        Self::DecodeBudget,
        Self::Invalid,
    ];

    /// The stable string form, such as `pack.free_pages`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Io => "pack.io",
            Self::Sqlite => "pack.sqlite",
            Self::NotAFile => "pack.not_a_file",
            Self::TooLarge => "pack.too_large",
            Self::FileLength => "pack.file_length",
            Self::Schema => "pack.schema",
            Self::Identity => "pack.identity",
            Self::FreePages => "pack.free_pages",
            Self::Meta => "pack.meta",
            Self::Profile => "pack.profile",
            Self::String => "pack.string",
            Self::StringIds => "pack.string_ids",
            Self::Plane => "pack.plane",
            Self::Zone => "pack.zone",
            Self::Anchor => "pack.anchor",
            Self::Stairwell => "pack.stairwell",
            Self::StairwellStep => "pack.stairwell_step",
            Self::Definition => "pack.definition",
            Self::Chunk => "pack.chunk",
            Self::ChunkEncoding => "pack.chunk_encoding",
            Self::ChunkData => "pack.chunk_data",
            Self::ChunkRecord => "pack.chunk_record",
            Self::Reference => "pack.reference",
            Self::ObjectUid => "pack.object_uid",
            Self::Integrity => "pack.integrity",
            Self::Digest => "pack.digest",
            Self::DecodeBudget => "pack.decode_budget",
            Self::Invalid => "pack.invalid",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Sqlite(rusqlite::Error),
    Chunk(DecodeError),
    /// A pack the reader rejects, with the code of the rule it breaks.
    Rejected(ErrorCode, String),
    /// A world the compiler cannot encode.
    Invalid(String),
}

impl Error {
    /// The stable code of this error.
    #[must_use]
    pub const fn code(&self) -> ErrorCode {
        match self {
            Self::Io(_) => ErrorCode::Io,
            Self::Sqlite(_) => ErrorCode::Sqlite,
            Self::Chunk(_) => ErrorCode::ChunkRecord,
            Self::Rejected(code, _) => *code,
            Self::Invalid(_) => ErrorCode::Invalid,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "I/O error: {error}"),
            Self::Sqlite(error) => write!(formatter, "SQLite error: {error}"),
            Self::Chunk(error) => error.fmt(formatter),
            Self::Rejected(_, message) | Self::Invalid(message) => formatter.write_str(message),
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
