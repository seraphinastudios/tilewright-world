// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Single reads and listings of planes, zones, anchors and definitions, and
//! of the layers and chunk positions a plane has. Like [`Pack::chunk`], each
//! checks the shape and bounds of what it loads, and agreement between a
//! row and its own document; whether the targets a row names exist is
//! checked by [`Pack::verify`].

use super::{
    bounded_identifier, bounded_text, maximum_document_bytes, stairwell_row_within,
    stored_document, text, utf8_column,
};
use crate::{Error, ErrorCode, Pack, is_identifier};
use rusqlite::{Row, ToSql, params, types};
use std::fmt::Write as _;
use tilewright_jsonc::{Document, MAX_SAFE_INTEGER, Value};
use tilewright_source::{CHUNK_SIZE, Stairwell};

/// A plane row and its stored document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Plane {
    pub id: String,
    /// `shared`, `instanced` or `per_player`.
    pub kind: String,
    pub chunk_size: u16,
    /// The plane's source document as canonical JSON.
    pub document: String,
}

/// A zone row and its stored entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Zone {
    pub plane: String,
    pub id: String,
    /// The parent zone's ID, or `None` for a root zone.
    pub parent: Option<String>,
    /// The zone's source entry as canonical JSON.
    pub document: String,
}

/// A named position on a plane.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Anchor {
    pub plane: String,
    pub id: String,
    pub layer: i32,
    pub x: i64,
    pub y: i64,
}

/// A definition row and its stored document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Definition {
    /// The definition type, such as `proto`.
    pub kind: String,
    pub id: String,
    /// The definition's source document as canonical JSON.
    pub document: String,
}

/// SQL for `column` when it is an integer, else NULL.
pub(super) fn integer(column: &str) -> String {
    format!("CASE WHEN typeof({column}) = 'integer' THEN {column} END")
}

/// The identifier in column `index` of a row selected with
/// [`bounded_identifier`]; `code` is the rule a bad one breaks.
pub(super) fn identifier(
    row: &Row<'_>,
    index: usize,
    what: &str,
    code: ErrorCode,
) -> Result<String, Error> {
    utf8_column(row, index, code)?
        .filter(|value| is_identifier(value))
        .ok_or_else(|| Error::Rejected(code, format!("a {what} key is not an identifier")))
}

/// The 32-bit layer in column `index` of a row selected with [`integer`].
pub(super) fn layer(
    row: &Row<'_>,
    index: usize,
    what: &str,
    code: ErrorCode,
) -> Result<i32, Error> {
    let value: Option<i64> = row.get(index)?;
    value
        .and_then(|layer| i32::try_from(layer).ok())
        .ok_or_else(|| Error::Rejected(code, format!("a {what} layer is not a 32-bit integer")))
}

/// The integer in column `index` of a row selected with [`integer`], within
/// the source format's integer range.
pub(super) fn position(
    row: &Row<'_>,
    index: usize,
    what: &str,
    code: ErrorCode,
) -> Result<i64, Error> {
    let value: Option<i64> = row.get(index)?;
    value
        .filter(|value| (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(value))
        .ok_or_else(|| {
            Error::Rejected(
                code,
                format!("a {what} position is not an integer within ±{MAX_SAFE_INTEGER}"),
            )
        })
}

/// The chunk coordinate in column `index` of a row selected with
/// [`integer`]: one whose chunk, `CHUNK_SIZE` tiles across as format 1
/// requires, keeps every tile within the format's integer range.
pub(super) fn chunk_coordinate(row: &Row<'_>, index: usize, code: ErrorCode) -> Result<i64, Error> {
    let size = i64::from(CHUNK_SIZE);
    let range = -(MAX_SAFE_INTEGER / size)..=(MAX_SAFE_INTEGER - (size - 1)) / size;
    let value: Option<i64> = row.get(index)?;
    value.filter(|value| range.contains(value)).ok_or_else(|| {
        Error::Rejected(
            code,
            format!(
                "a chunk coordinate is not an integer from {} to {}",
                range.start(),
                range.end()
            ),
        )
    })
}

/// The error for a stored document that disagrees with its row.
pub(super) fn disagrees(what: &str, code: ErrorCode) -> Error {
    Error::Rejected(
        code,
        format!("the document of {what} disagrees with its row"),
    )
}

/// The columns of a plane row: `id, kind, chunk_size, document`.
pub(super) fn plane_columns() -> String {
    format!(
        "{}, {}, {}, {}",
        bounded_identifier("id"),
        bounded_identifier("kind"),
        integer("chunk_size"),
        bounded_text("document")
    )
}

/// A row of `planes` selected with [`plane_columns`], and its stored
/// document. As verification checks, the kind is one format 1 defines, the
/// chunk size is [`CHUNK_SIZE`], and the document agrees with the row on its
/// format, ID, kind and chunk size, and has a string `default_zone`, if any;
/// whether that zone exists is checked by verification.
pub(super) fn plane_row(row: &Row<'_>) -> Result<(String, Document), Error> {
    let id = identifier(row, 0, "plane", ErrorCode::Plane)?;
    let kind = utf8_column(row, 1, ErrorCode::Plane)?;
    let chunk_size: Option<i64> = row.get(2)?;
    let stored = stored_document(
        utf8_column(row, 3, ErrorCode::Plane)?,
        "a plane document",
        ErrorCode::Plane,
    )?;
    if !kind
        .as_deref()
        .is_some_and(|kind| ["shared", "instanced", "per_player"].contains(&kind))
        || chunk_size != Some(i64::from(CHUNK_SIZE))
    {
        return Err(Error::Rejected(
            ErrorCode::Plane,
            format!("plane {id} has an invalid kind or a chunk size other than {CHUNK_SIZE}"),
        ));
    }
    if text(&stored, "format") != Some("tilewright.plane/1")
        || text(&stored, "id") != Some(&id)
        || text(&stored, "kind") != kind.as_deref()
        || stored.root().get("chunk_size").and_then(Value::as_i64) != chunk_size
        || stored
            .root()
            .get("default_zone")
            .is_some_and(|zone| zone.as_str().is_none())
    {
        return Err(disagrees(&format!("plane {id}"), ErrorCode::Plane));
    }
    Ok((id, stored))
}

/// A [`Plane`] from a row selected with [`plane_columns`].
fn plane(row: &Row<'_>) -> Result<Plane, Error> {
    let (id, stored) = plane_row(row)?;
    Ok(Plane {
        id,
        kind: text(&stored, "kind").unwrap_or_default().to_owned(),
        chunk_size: CHUNK_SIZE,
        document: stored.source().to_owned(),
    })
}

/// The columns of a zone row: `plane, id, parent IS NULL, parent, document`.
pub(super) fn zone_columns() -> String {
    format!(
        "{}, {}, parent IS NULL, {}, {}",
        bounded_identifier("plane"),
        bounded_identifier("id"),
        bounded_identifier("parent"),
        bounded_text("document")
    )
}

/// A row of `zones` selected with [`zone_columns`], and its stored document,
/// which agrees with the row on its ID and parent, as verification checks.
pub(super) fn zone_row(row: &Row<'_>) -> Result<(String, String, Document), Error> {
    let (plane, id) = (
        identifier(row, 0, "zone", ErrorCode::Zone)?,
        identifier(row, 1, "zone", ErrorCode::Zone)?,
    );
    let parent = if row.get::<_, bool>(2)? {
        None
    } else {
        Some(identifier(row, 3, "zone parent", ErrorCode::Zone)?)
    };
    let stored = stored_document(
        utf8_column(row, 4, ErrorCode::Zone)?,
        "a zone document",
        ErrorCode::Zone,
    )?;
    if text(&stored, "id") != Some(&id)
        || stored.root().get("parent").map(Value::as_str) != parent.as_deref().map(Some)
    {
        return Err(disagrees(&format!("zone {plane}/{id}"), ErrorCode::Zone));
    }
    Ok((plane, id, stored))
}

/// A [`Zone`] from a row selected with [`zone_columns`].
fn zone(row: &Row<'_>) -> Result<Zone, Error> {
    let (plane, id, stored) = zone_row(row)?;
    Ok(Zone {
        plane,
        id,
        parent: text(&stored, "parent").map(str::to_owned),
        document: stored.source().to_owned(),
    })
}

/// The columns of an anchor row: `plane, id, layer, x, y`.
pub(super) fn anchor_columns() -> String {
    format!(
        "{}, {}, {}, {}, {}",
        bounded_identifier("plane"),
        bounded_identifier("id"),
        integer("layer"),
        integer("x"),
        integer("y")
    )
}

/// An anchor row selected with [`anchor_columns`]: identifier keys, a
/// 32-bit layer and a position within the source format's integer range.
pub(super) fn anchor_row(row: &Row<'_>) -> Result<Anchor, Error> {
    let code = ErrorCode::Anchor;
    Ok(Anchor {
        plane: identifier(row, 0, "anchor", code)?,
        id: identifier(row, 1, "anchor", code)?,
        layer: layer(row, 2, "anchor", code)?,
        x: position(row, 3, "anchor", code)?,
        y: position(row, 4, "anchor", code)?,
    })
}

/// The columns of a definition row: `type, id, document`.
pub(super) fn definition_columns() -> String {
    format!(
        "{}, {}, {}",
        bounded_identifier("type"),
        bounded_identifier("id"),
        bounded_text("document")
    )
}

/// A row of `definitions` selected with [`definition_columns`], whose
/// document agrees with it on its format, type and ID.
pub(super) fn definition_row(row: &Row<'_>) -> Result<(String, String, Document), Error> {
    let (kind, id) = (
        identifier(row, 0, "definition", ErrorCode::Definition)?,
        identifier(row, 1, "definition", ErrorCode::Definition)?,
    );
    let stored = stored_document(
        utf8_column(row, 2, ErrorCode::Definition)?,
        "a definition document",
        ErrorCode::Definition,
    )?;
    if text(&stored, "format") != Some("tilewright.def/1")
        || text(&stored, "type") != Some(&kind)
        || text(&stored, "id") != Some(&id)
    {
        return Err(disagrees(
            &format!("definition {kind}/{id}"),
            ErrorCode::Definition,
        ));
    }
    Ok((kind, id, stored))
}

/// A [`Definition`] from a row selected with [`definition_columns`].
fn definition(row: &Row<'_>) -> Result<Definition, Error> {
    let (kind, id, stored) = definition_row(row)?;
    Ok(Definition {
        kind,
        id,
        document: stored.source().to_owned(),
    })
}

/// Which rows of a listing to visit: those after the row whose key is
/// `after`, in the listing's key order, at most `limit` of them, and no
/// more once their text reaches `bytes`. A listing reads a page with one
/// query that starts at `after` in the table's primary key, so a caller can
/// walk a table a page at a time, holding one page, and resume from the
/// last key it saw.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Page<K> {
    /// The key of the last row already seen, or `None` to start at the
    /// first row.
    pub after: Option<K>,
    /// The most rows to visit.
    pub limit: usize,
    /// The text to visit: the page stops after the row that brings the
    /// bytes of its rows' keys and documents to this, so it always visits a
    /// row when there is one, and holds less than this and one row.
    pub bytes: usize,
}

impl<K> Page<K> {
    /// Every row, from the first.
    #[must_use]
    pub const fn all() -> Self {
        Self {
            after: None,
            limit: usize::MAX,
            bytes: usize::MAX,
        }
    }
}

/// A listing's query: `SELECT ... FROM ...` and its conditions, with its
/// parameters in the order they bind.
struct Listing {
    sql: String,
    conditions: Vec<String>,
    values: Vec<types::Value>,
}

impl Listing {
    /// Selects `columns` from `table`. Columns that load a document bound
    /// it by `?1`, the largest stored document, so `documents` binds it.
    fn new(columns: &str, table: &str, documents: bool) -> Self {
        Self {
            sql: format!("SELECT {columns} FROM {table}"),
            conditions: Vec::new(),
            values: if documents {
                vec![types::Value::Integer(maximum_document_bytes())]
            } else {
                Vec::new()
            },
        }
    }

    /// The next parameter, `?N`, bound to `value`.
    fn bind(&mut self, value: impl Into<types::Value>) -> String {
        self.values.push(value.into());
        format!("?{}", self.values.len())
    }

    /// Adds a condition that takes the next parameters.
    fn condition(&mut self, condition: impl FnOnce(&mut Self) -> String) -> &mut Self {
        let condition = condition(self);
        self.conditions.push(condition);
        self
    }

    /// The query, ending in `order`, its `ORDER BY` clause and any
    /// grouping before it, and limited to `limit` rows; a limit past
    /// SQLite's is none.
    fn finish(mut self, order: &str, limit: usize) -> (String, Vec<types::Value>) {
        let limit = self.bind(i64::try_from(limit).unwrap_or(-1));
        let mut sql = self.sql;
        if !self.conditions.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&self.conditions.join(" AND "));
        }
        let _ = write!(sql, " {order} LIMIT {limit}");
        (sql, self.values)
    }
}

impl Pack {
    /// The first row of `sql`, bound to `parameters`, read with `read`.
    fn row_of<T>(
        &self,
        sql: &str,
        parameters: &[&dyn ToSql],
        read: fn(&Row<'_>) -> Result<T, Error>,
    ) -> Result<Option<T>, Error> {
        let mut statement = self.connection.prepare(sql)?;
        let mut rows = statement.query(parameters)?;
        rows.next()?.map(read).transpose()
    }

    /// Calls `visit` with each row of a listing, read with `read`, until the
    /// sizes `keyed` gives the rows reach `bytes`. Keys, which `keyed` also
    /// gives, must rise strictly from `after`, as a table's primary key
    /// does; a page that does not move forward is an invalid table,
    /// `code`'s error, so walking a table page by page always ends.
    #[expect(
        clippy::too_many_arguments,
        reason = "each listing gives its query, page, row reader, key and code"
    )]
    fn walk<T, K: Ord>(
        &self,
        (sql, values): (String, Vec<types::Value>),
        after: Option<K>,
        bytes: usize,
        read: impl Fn(&Row<'_>) -> Result<T, Error>,
        keyed: impl Fn(&T) -> (K, usize),
        code: ErrorCode,
        visit: &mut dyn FnMut(T) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let mut statement = self.connection.prepare(&sql)?;
        let mut rows = statement.query(rusqlite::params_from_iter(values))?;
        let mut last = after;
        let mut total: usize = 0;
        while let Some(row) = rows.next()? {
            let value = read(row)?;
            let (next, size) = keyed(&value);
            if last.as_ref().is_some_and(|last| *last >= next) {
                return Err(Error::Rejected(
                    code,
                    "a table's rows are not in the order of its primary key".into(),
                ));
            }
            last = Some(next);
            visit(value)?;
            total = total.saturating_add(size);
            if total >= bytes {
                break;
            }
        }
        Ok(())
    }

    /// The stored profile as canonical JSON, checked as a full
    /// verification checks it; see [`Pack::layer_height`].
    ///
    /// # Errors
    /// Returns an error if SQLite fails or the stored profile is invalid.
    pub fn profile(&self) -> Result<String, Error> {
        self.profile_layers()?;
        self.meta("profile")?
            .ok_or_else(|| Error::Rejected(ErrorCode::Meta, "the pack has no profile".into()))
    }

    /// A plane, read from its row and stored document.
    ///
    /// # Errors
    /// Returns an error if SQLite fails or the stored plane is invalid.
    pub fn plane(&self, id: &str) -> Result<Option<Plane>, Error> {
        self.row_of(
            &format!("SELECT {} FROM planes WHERE id = ?2", plane_columns()),
            params![maximum_document_bytes(), id],
            plane,
        )
    }

    /// Calls `visit` with the planes of `page`, in ID order. One stored
    /// document is held at a time.
    ///
    /// # Errors
    /// Returns an error if SQLite fails, a stored plane is invalid, or
    /// `visit` fails.
    pub fn visit_planes(
        &self,
        page: Page<&str>,
        visit: &mut dyn FnMut(Plane) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.walk(
            planes_listing(page),
            page.after.map(str::to_owned),
            page.bytes,
            plane,
            |plane| {
                let size = plane.id.len() + plane.kind.len() + plane.document.len();
                (plane.id.clone(), size)
            },
            ErrorCode::Plane,
            visit,
        )
    }

    /// A zone of a plane. Whether the plane and parent exist is checked by
    /// [`Pack::verify`].
    ///
    /// # Errors
    /// Returns an error if SQLite fails or the stored zone is invalid.
    pub fn zone(&self, plane: &str, id: &str) -> Result<Option<Zone>, Error> {
        self.row_of(
            &format!(
                "SELECT {} FROM zones WHERE plane = ?2 AND id = ?3",
                zone_columns()
            ),
            params![maximum_document_bytes(), plane, id],
            zone,
        )
    }

    /// Calls `visit` with the zones of a plane in `page`, in ID order.
    ///
    /// # Errors
    /// Returns an error if SQLite fails, a stored zone is invalid, or
    /// `visit` fails.
    pub fn visit_zones(
        &self,
        plane: &str,
        page: Page<&str>,
        visit: &mut dyn FnMut(Zone) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.walk(
            in_plane(&zone_columns(), "zones", true, plane, page),
            page.after.map(str::to_owned),
            page.bytes,
            zone,
            |zone| {
                let parent = zone.parent.as_ref().map_or(0, String::len);
                let size = zone.plane.len() + zone.id.len() + parent + zone.document.len();
                (zone.id.clone(), size)
            },
            ErrorCode::Zone,
            visit,
        )
    }

    /// An anchor of a plane. Whether the plane exists is checked by
    /// [`Pack::verify`].
    ///
    /// # Errors
    /// Returns an error if SQLite fails or the stored anchor is invalid.
    pub fn anchor(&self, plane: &str, id: &str) -> Result<Option<Anchor>, Error> {
        self.row_of(
            &format!(
                "SELECT {} FROM anchors WHERE plane = ?1 AND id = ?2",
                anchor_columns()
            ),
            params![plane, id],
            anchor_row,
        )
    }

    /// Calls `visit` with the anchors of a plane in `page`, in ID order.
    ///
    /// # Errors
    /// Returns an error if SQLite fails, a stored anchor is invalid, or
    /// `visit` fails.
    pub fn visit_anchors(
        &self,
        plane: &str,
        page: Page<&str>,
        visit: &mut dyn FnMut(Anchor) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.walk(
            in_plane(&anchor_columns(), "anchors", false, plane, page),
            page.after.map(str::to_owned),
            page.bytes,
            anchor_row,
            |anchor| (anchor.id.clone(), anchor.plane.len() + anchor.id.len()),
            ErrorCode::Anchor,
            visit,
        )
    }

    /// Calls `visit` with the stairwells of a plane in `page`, in ID order,
    /// read from their stored documents as [`Pack::stairwell`] reads one.
    /// One stored document is held at a time.
    ///
    /// # Errors
    /// Returns an error if SQLite fails, a stored stairwell is invalid, or
    /// `visit` fails.
    pub fn visit_stairwells(
        &self,
        plane: &str,
        page: Page<&str>,
        visit: &mut dyn FnMut(Stairwell) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.visit_stairwells_within(plane, page, u64::MAX, visit)
    }

    /// [`Pack::visit_stairwells`], with a budget of `maximum_steps` steps
    /// for each stairwell; see [`Pack::stairwell_within`].
    ///
    /// # Errors
    /// Returns an error if SQLite fails, a stored stairwell is invalid or
    /// lists more steps than the budget, or `visit` fails.
    pub fn visit_stairwells_within(
        &self,
        plane: &str,
        page: Page<&str>,
        maximum_steps: u64,
        visit: &mut dyn FnMut(Stairwell) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let columns = format!("{}, {}", bounded_identifier("id"), bounded_text("document"));
        self.walk(
            in_plane(&columns, "stairwells", true, plane, page),
            page.after.map(str::to_owned),
            page.bytes,
            |row| stairwell_row_within(row, maximum_steps),
            |stairwell| {
                let steps: usize = stairwell.steps.iter().map(|step| step.proto.len()).sum();
                let custom = stairwell.custom.as_ref().map_or(0, String::len);
                let size = stairwell.id.len() + steps + custom + stairwell.canonical.len();
                (stairwell.id.clone(), size)
            },
            ErrorCode::Stairwell,
            visit,
        )
    }

    /// A definition, by type and ID.
    ///
    /// # Errors
    /// Returns an error if SQLite fails or the stored definition is invalid.
    pub fn definition(&self, kind: &str, id: &str) -> Result<Option<Definition>, Error> {
        self.row_of(
            &format!(
                "SELECT {} FROM definitions WHERE type = ?2 AND id = ?3",
                definition_columns()
            ),
            params![maximum_document_bytes(), kind, id],
            definition,
        )
    }

    /// Calls `visit` with the definitions of `page`, by type and then ID:
    /// those of type `kind`, or every definition when `kind` is `None`.
    /// A page's key is a definition's type and ID. One stored document is
    /// held at a time.
    ///
    /// # Errors
    /// Returns an error if SQLite fails, a stored definition is invalid, or
    /// `visit` fails.
    pub fn visit_definitions(
        &self,
        kind: Option<&str>,
        page: Page<(&str, &str)>,
        visit: &mut dyn FnMut(Definition) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let Some(query) = definitions_listing(kind, page) else {
            return Ok(());
        };
        self.walk(
            query,
            page.after
                .map(|(kind, id)| (kind.to_owned(), id.to_owned())),
            page.bytes,
            definition,
            |definition| {
                let size = definition.kind.len() + definition.id.len() + definition.document.len();
                ((definition.kind.clone(), definition.id.clone()), size)
            },
            ErrorCode::Definition,
            visit,
        )
    }

    /// Calls `visit` with the layers of a plane that have chunks, in
    /// `page`, in order.
    ///
    /// # Errors
    /// Returns an error if SQLite fails, a stored layer is not a 32-bit
    /// integer, or `visit` fails.
    pub fn visit_layers(
        &self,
        plane: &str,
        page: Page<i32>,
        visit: &mut dyn FnMut(i32) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.walk(
            layers_listing(plane, page),
            page.after,
            page.bytes,
            |row| layer(row, 0, "chunk", ErrorCode::Chunk),
            |layer| (*layer, 0),
            ErrorCode::Chunk,
            visit,
        )
    }

    /// Calls `visit` with the chunk coordinates `(cx, cy)` of the chunks of
    /// a plane's layer in `page`, by `cx` and then `cy`. Each keeps every
    /// tile of its chunk within the source format's integer range.
    ///
    /// # Errors
    /// Returns an error if SQLite fails, a stored position is out of range,
    /// or `visit` fails.
    pub fn visit_chunk_keys(
        &self,
        plane: &str,
        layer: i32,
        page: Page<(i64, i64)>,
        visit: &mut dyn FnMut(i64, i64) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.walk(
            chunk_keys_listing(plane, layer, page),
            page.after,
            page.bytes,
            |row| {
                Ok((
                    chunk_coordinate(row, 0, ErrorCode::Chunk)?,
                    chunk_coordinate(row, 1, ErrorCode::Chunk)?,
                ))
            },
            |key| (*key, 0),
            ErrorCode::Chunk,
            &mut |(cx, cy)| visit(cx, cy),
        )
    }
}

// Each listing's query seeks its table's primary key to the row after the
// page's key, so a walk of a table page by page reads each row once.

/// The listing of planes in `page`.
fn planes_listing(page: Page<&str>) -> (String, Vec<types::Value>) {
    let mut listing = Listing::new(&plane_columns(), "planes", true);
    if let Some(after) = page.after {
        listing.condition(|listing| format!("id > {}", listing.bind(after.to_owned())));
    }
    listing.finish("ORDER BY id", page.limit)
}

/// The listing of a plane's rows of `table`, keyed by `id`, in `page`.
fn in_plane(
    columns: &str,
    table: &str,
    documents: bool,
    plane: &str,
    page: Page<&str>,
) -> (String, Vec<types::Value>) {
    let mut listing = Listing::new(columns, table, documents);
    listing.condition(|listing| format!("plane = {}", listing.bind(plane.to_owned())));
    if let Some(after) = page.after {
        listing.condition(|listing| format!("id > {}", listing.bind(after.to_owned())));
    }
    listing.finish("ORDER BY id", page.limit)
}

/// The listing of the definitions of `page`, of type `kind` or of every
/// type, or `None` when no row can follow the page's key. With a type, the
/// key's type decides which IDs follow, so the query bounds only the ID: a
/// bound on `(type, id)` as well as `type = ?` would make SQLite seek the
/// type and filter its rows from the first, for every page.
fn definitions_listing(
    kind: Option<&str>,
    page: Page<(&str, &str)>,
) -> Option<(String, Vec<types::Value>)> {
    let mut listing = Listing::new(&definition_columns(), "definitions", true);
    match (kind, page.after) {
        (Some(kind), after) => {
            listing.condition(|listing| format!("type = {}", listing.bind(kind.to_owned())));
            if let Some((after_kind, after_id)) = after {
                match after_kind.cmp(kind) {
                    // Every definition of the type follows the key.
                    std::cmp::Ordering::Less => {}
                    std::cmp::Ordering::Equal => {
                        listing.condition(|listing| {
                            format!("id > {}", listing.bind(after_id.to_owned()))
                        });
                    }
                    std::cmp::Ordering::Greater => return None,
                }
            }
        }
        (None, Some((after_kind, after_id))) => {
            listing.condition(|listing| {
                format!(
                    "(type, id) > ({}, {})",
                    listing.bind(after_kind.to_owned()),
                    listing.bind(after_id.to_owned())
                )
            });
        }
        (None, None) => {}
    }
    Some(listing.finish("ORDER BY type, id", page.limit))
}

/// The listing of the layers of a plane that have chunks, in `page`.
fn layers_listing(plane: &str, page: Page<i32>) -> (String, Vec<types::Value>) {
    let mut listing = Listing::new(&integer("layer"), "chunks", false);
    listing.condition(|listing| format!("plane = {}", listing.bind(plane.to_owned())));
    if let Some(after) = page.after {
        listing.condition(|listing| format!("layer > {}", listing.bind(after)));
    }
    // Grouping by the stored layer lets SQLite read the primary key in
    // order instead of collecting the layers first.
    listing.finish("GROUP BY layer ORDER BY layer", page.limit)
}

/// The listing of the chunk keys of a plane's layer, in `page`.
fn chunk_keys_listing(
    plane: &str,
    layer: i32,
    page: Page<(i64, i64)>,
) -> (String, Vec<types::Value>) {
    let mut listing = Listing::new(
        &format!("{}, {}", integer("cx"), integer("cy")),
        "chunks",
        false,
    );
    listing.condition(|listing| format!("plane = {}", listing.bind(plane.to_owned())));
    listing.condition(|listing| format!("layer = {}", listing.bind(layer)));
    if let Some((cx, cy)) = page.after {
        listing.condition(|listing| {
            format!("(cx, cy) > ({}, {})", listing.bind(cx), listing.bind(cy))
        });
    }
    listing.finish("ORDER BY cx, cy", page.limit)
}

#[cfg(test)]
mod tests {
    use super::{
        Listing, Page, chunk_keys_listing, definitions_listing, in_plane, layers_listing, plane,
        plane_columns, planes_listing, types,
    };
    use crate::{Error, ErrorCode, Pack, VerifyLimits};
    use std::path::Path;

    fn pack() -> Pack {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(
            "../../fixtures/packs/5830e26e449a1f0d0c786b32801b7ed68e351777db5f586532478cb4384844b2.twpack",
        );
        Pack::open_with(&path, &VerifyLimits::default()).unwrap()
    }

    /// The rows of a listing read all at once, and read one page of one row
    /// at a time from the last row's key, which must be the same. `list`
    /// reads the page after a key, of at most a number of rows.
    fn paged<T: PartialEq + std::fmt::Debug, K>(
        list: impl Fn(Option<&K>, usize, &mut dyn FnMut(T) -> Result<(), Error>) -> Result<(), Error>,
        key: impl Fn(&T) -> K,
    ) -> Vec<T> {
        let read = |after: Option<&K>, limit| {
            let mut rows = Vec::new();
            list(after, limit, &mut |row| {
                rows.push(row);
                Ok(())
            })
            .unwrap();
            rows
        };
        let all = read(None, usize::MAX);
        let mut one_by_one: Vec<T> = Vec::new();
        loop {
            let after = one_by_one.last().map(&key);
            let mut page = read(after.as_ref(), 1);
            assert!(page.len() <= 1);
            let Some(row) = page.pop() else { break };
            one_by_one.push(row);
        }
        assert_eq!(one_by_one, all);
        all
    }

    fn text(after: Option<&String>) -> Option<&str> {
        after.map(String::as_str)
    }

    fn pair(after: Option<&(String, String)>) -> Option<(&str, &str)> {
        after.map(|(kind, id)| (kind.as_str(), id.as_str()))
    }

    #[test]
    fn plane_listings_page_by_their_keys() {
        let pack = pack();
        let planes = paged(
            |after, limit, visit| {
                pack.visit_planes(
                    Page {
                        after: text(after),
                        limit,
                        bytes: usize::MAX,
                    },
                    visit,
                )
            },
            |plane| plane.id.clone(),
        );
        assert_eq!(planes.len(), 2);
        let zones = paged(
            |after, limit, visit| {
                pack.visit_zones(
                    "overworld",
                    Page {
                        after: text(after),
                        limit,
                        bytes: usize::MAX,
                    },
                    visit,
                )
            },
            |zone| zone.id.clone(),
        );
        assert_eq!(zones.len(), 3);
        let anchors = paged(
            |after, limit, visit| {
                pack.visit_anchors(
                    "overworld",
                    Page {
                        after: text(after),
                        limit,
                        bytes: usize::MAX,
                    },
                    visit,
                )
            },
            |anchor| anchor.id.clone(),
        );
        assert_eq!(anchors.len(), 2);
        let stairwells = paged(
            |after, limit, visit| {
                pack.visit_stairwells(
                    "overworld",
                    Page {
                        after: text(after),
                        limit,
                        bytes: usize::MAX,
                    },
                    visit,
                )
            },
            |stairwell| stairwell.id.clone(),
        );
        assert_eq!(stairwells.len(), 3);
    }

    #[test]
    fn definitions_layers_and_chunk_keys_page_by_their_keys() {
        let pack = pack();
        let definitions = paged(
            |after, limit, visit| {
                pack.visit_definitions(
                    None,
                    Page {
                        after: pair(after),
                        limit,
                        bytes: usize::MAX,
                    },
                    visit,
                )
            },
            |definition| (definition.kind.clone(), definition.id.clone()),
        );
        assert_eq!(definitions.len(), 12);
        let protos = paged(
            |after, limit, visit| {
                pack.visit_definitions(
                    Some("proto"),
                    Page {
                        after: pair(after),
                        limit,
                        bytes: usize::MAX,
                    },
                    visit,
                )
            },
            |definition| (definition.kind.clone(), definition.id.clone()),
        );
        assert!(!protos.is_empty() && protos.iter().all(|found| found.kind == "proto"));
        let layers = paged(
            |after, limit, visit| {
                pack.visit_layers(
                    "overworld",
                    Page {
                        after: after.copied(),
                        limit,
                        bytes: usize::MAX,
                    },
                    visit,
                )
            },
            |layer| *layer,
        );
        assert_eq!(layers, [-1, 0, 1]);
        let keys = paged(
            |after, limit, visit| {
                pack.visit_chunk_keys(
                    "overworld",
                    0,
                    Page {
                        after: after.copied(),
                        limit,
                        bytes: usize::MAX,
                    },
                    &mut |cx, cy| visit((cx, cy)),
                )
            },
            |key| *key,
        );
        assert_eq!(keys, [(-1, 0), (0, 0)]);
        // A page past the last row, and an empty page, visit nothing.
        for page in [
            Page {
                after: Some("~"),
                limit: 5,
                bytes: usize::MAX,
            },
            Page {
                after: None,
                limit: 0,
                bytes: usize::MAX,
            },
        ] {
            pack.visit_planes(page, &mut |_| panic!("{page:?} is empty"))
                .unwrap();
        }
    }

    /// A page of up to 64 rows after `key`.
    fn after<K>(key: K) -> Page<K> {
        Page {
            after: Some(key),
            limit: 64,
            bytes: usize::MAX,
        }
    }

    /// The steps of the query plan of `query`.
    fn plan(pack: &Pack, (sql, values): (String, Vec<types::Value>)) -> Vec<String> {
        let mut statement = pack
            .connection
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap();
        statement
            .query_map(rusqlite::params_from_iter(values), |row| row.get(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    #[test]
    fn every_listing_seeks_past_its_page_key() {
        let pack = pack();
        let stairwell_columns = format!(
            "{}, {}",
            super::bounded_identifier("id"),
            super::bounded_text("document")
        );
        for (query, seek) in [
            (
                planes_listing(after("a")),
                "SEARCH planes USING PRIMARY KEY (id>?)",
            ),
            (
                in_plane(
                    &super::zone_columns(),
                    "zones",
                    true,
                    "overworld",
                    after("a"),
                ),
                "SEARCH zones USING PRIMARY KEY (plane=? AND id>?)",
            ),
            (
                in_plane(
                    &super::anchor_columns(),
                    "anchors",
                    false,
                    "overworld",
                    after("a"),
                ),
                "SEARCH anchors USING PRIMARY KEY (plane=? AND id>?)",
            ),
            (
                in_plane(
                    &stairwell_columns,
                    "stairwells",
                    true,
                    "overworld",
                    after("a"),
                ),
                "SEARCH stairwells USING PRIMARY KEY (plane=? AND id>?)",
            ),
            (
                definitions_listing(None, after(("proto", "a"))).unwrap(),
                "SEARCH definitions USING PRIMARY KEY ((type,id)>(?,?))",
            ),
            (
                definitions_listing(Some("proto"), after(("proto", "a"))).unwrap(),
                "SEARCH definitions USING PRIMARY KEY (type=? AND id>?)",
            ),
            (
                layers_listing("overworld", after(0)),
                "SEARCH chunks USING PRIMARY KEY (plane=? AND layer>?)",
            ),
            (
                chunk_keys_listing("overworld", 0, after((0, 0))),
                "SEARCH chunks USING PRIMARY KEY (plane=? AND layer=? AND (cx,cy)>(?,?))",
            ),
        ] {
            assert_eq!(plan(&pack, query), [seek]);
        }
    }

    #[test]
    fn typed_definition_pages_follow_a_key_of_any_type() {
        let pack = pack();
        let read = |after: Option<(&str, &str)>| {
            let mut ids = Vec::new();
            let page = Page {
                after,
                limit: usize::MAX,
                bytes: usize::MAX,
            };
            pack.visit_definitions(Some("proto"), page, &mut |definition| {
                ids.push(definition.id);
                Ok(())
            })
            .unwrap();
            ids
        };
        let all = read(None);
        assert!(all.len() > 1);
        // A key of a type before the listing's is followed by all of its
        // rows; a key of a type after it, by none.
        assert_eq!(read(Some(("a", "zzz"))), all);
        assert!(read(Some(("zzz", "a"))).is_empty());
        assert_eq!(read(Some(("proto", all[0].as_str()))), all[1..]);
    }

    #[test]
    fn pages_stop_at_their_byte_budget() {
        let pack = pack();
        let read = |after: Option<&(String, String)>, bytes| {
            let mut rows = Vec::new();
            let page = Page {
                after: pair(after),
                limit: usize::MAX,
                bytes,
            };
            pack.visit_definitions(None, page, &mut |row| {
                rows.push(row);
                Ok(())
            })
            .unwrap();
            rows
        };
        let all = read(None, usize::MAX);
        // A budget of one byte reads one row a page, and the rows still
        // all come, in order.
        let mut walked: Vec<super::Definition> = Vec::new();
        loop {
            let after = walked
                .last()
                .map(|last| (last.kind.clone(), last.id.clone()));
            let page = read(after.as_ref(), 1);
            assert!(page.len() <= 1);
            if page.is_empty() {
                break;
            }
            walked.extend(page);
        }
        assert_eq!(walked, all);
        // A page stops after the row whose text reaches the budget.
        let size = |row: &super::Definition| row.kind.len() + row.id.len() + row.document.len();
        let two = size(&all[0]) + size(&all[1]);
        assert_eq!(read(None, two).len(), 2);
        assert_eq!(read(None, two + 1).len(), 3);
    }

    #[test]
    fn listings_that_do_not_move_forward_fail() {
        let pack = pack();
        // Rows that come back out of key order, as a damaged table's could,
        // are an error rather than a page that could repeat.
        let mut listing = Listing::new(&plane_columns(), "planes", true);
        listing.conditions.clear();
        let descending = listing.finish("ORDER BY id DESC", usize::MAX);
        let error = pack
            .walk(
                descending,
                None,
                usize::MAX,
                plane,
                |plane| (plane.id.clone(), 0),
                ErrorCode::Plane,
                &mut |_| Ok(()),
            )
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::Plane);
        // So is a row at or before the key a page starts after.
        let listing = Listing::new(&plane_columns(), "planes", true);
        let error = pack
            .walk(
                listing.finish("ORDER BY id", usize::MAX),
                Some("vault".to_owned()),
                usize::MAX,
                plane,
                |plane| (plane.id.clone(), 0),
                ErrorCode::Plane,
                &mut |_| Ok(()),
            )
            .unwrap_err();
        assert_eq!(error.code(), ErrorCode::Plane);
    }
}
