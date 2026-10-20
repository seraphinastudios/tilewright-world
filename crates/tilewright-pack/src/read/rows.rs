// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Single reads and listings of planes, zones, anchors and definitions, and
//! of the layers and chunk positions a plane has. Like [`Pack::chunk`], each
//! checks the shape and bounds of what it loads, and agreement between a
//! row and its own document; whether the targets a row names exist is
//! checked by [`Pack::verify`].

use super::{
    bounded_identifier, bounded_text, maximum_document_bytes, stored_document, text, utf8_column,
};
use crate::{Error, ErrorCode, Pack, is_identifier};
use rusqlite::{Row, ToSql, params};
use tilewright_jsonc::{Document, MAX_SAFE_INTEGER, Value};
use tilewright_source::CHUNK_SIZE;

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

impl Pack {
    /// Calls `each` with every row of `sql`, bound to `parameters`.
    fn rows_of(
        &self,
        sql: &str,
        parameters: &[&dyn ToSql],
        each: &mut dyn FnMut(&Row<'_>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let mut statement = self.connection.prepare(sql)?;
        let mut rows = statement.query(parameters)?;
        while let Some(row) = rows.next()? {
            each(row)?;
        }
        Ok(())
    }

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

    /// Calls `visit` with each plane, in ID order. One stored document is
    /// held at a time.
    ///
    /// # Errors
    /// Returns an error if SQLite fails, a stored plane is invalid, or
    /// `visit` fails.
    pub fn visit_planes(
        &self,
        visit: &mut dyn FnMut(Plane) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.rows_of(
            &format!("SELECT {} FROM planes ORDER BY id", plane_columns()),
            params![maximum_document_bytes()],
            &mut |row| visit(plane(row)?),
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

    /// Calls `visit` with each zone of a plane, in ID order.
    ///
    /// # Errors
    /// Returns an error if SQLite fails, a stored zone is invalid, or
    /// `visit` fails.
    pub fn visit_zones(
        &self,
        plane: &str,
        visit: &mut dyn FnMut(Zone) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.rows_of(
            &format!(
                "SELECT {} FROM zones WHERE plane = ?2 ORDER BY id",
                zone_columns()
            ),
            params![maximum_document_bytes(), plane],
            &mut |row| visit(zone(row)?),
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

    /// Calls `visit` with each anchor of a plane, in ID order.
    ///
    /// # Errors
    /// Returns an error if SQLite fails, a stored anchor is invalid, or
    /// `visit` fails.
    pub fn visit_anchors(
        &self,
        plane: &str,
        visit: &mut dyn FnMut(Anchor) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.rows_of(
            &format!(
                "SELECT {} FROM anchors WHERE plane = ?1 ORDER BY id",
                anchor_columns()
            ),
            params![plane],
            &mut |row| visit(anchor_row(row)?),
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

    /// Calls `visit` with each definition of type `kind`, in ID order, or
    /// with every definition, by type and then ID, when `kind` is `None`.
    /// One stored document is held at a time.
    ///
    /// # Errors
    /// Returns an error if SQLite fails, a stored definition is invalid, or
    /// `visit` fails.
    pub fn visit_definitions(
        &self,
        kind: Option<&str>,
        visit: &mut dyn FnMut(Definition) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let columns = definition_columns();
        let mut each = |row: &Row<'_>| visit(definition(row)?);
        match kind {
            Some(kind) => self.rows_of(
                &format!("SELECT {columns} FROM definitions WHERE type = ?2 ORDER BY id"),
                params![maximum_document_bytes(), kind],
                &mut each,
            ),
            None => self.rows_of(
                &format!("SELECT {columns} FROM definitions ORDER BY type, id"),
                params![maximum_document_bytes()],
                &mut each,
            ),
        }
    }

    /// Calls `visit` with each layer of a plane that has chunks, in order.
    ///
    /// # Errors
    /// Returns an error if SQLite fails, a stored layer is not a 32-bit
    /// integer, or `visit` fails.
    pub fn visit_layers(
        &self,
        plane: &str,
        visit: &mut dyn FnMut(i32) -> Result<(), Error>,
    ) -> Result<(), Error> {
        // Grouping by the stored layer lets SQLite read the primary key in
        // order instead of collecting the layers first.
        self.rows_of(
            &format!(
                "SELECT {} FROM chunks WHERE plane = ?1 GROUP BY layer ORDER BY layer",
                integer("layer")
            ),
            params![plane],
            &mut |row| visit(layer(row, 0, "chunk", ErrorCode::Chunk)?),
        )
    }

    /// Calls `visit` with the chunk coordinates `(cx, cy)` of each chunk of
    /// a plane's layer, by `cx` and then `cy`. Each keeps every tile of its
    /// chunk within the source format's integer range.
    ///
    /// # Errors
    /// Returns an error if SQLite fails, a stored position is out of range,
    /// or `visit` fails.
    pub fn visit_chunk_keys(
        &self,
        plane: &str,
        layer: i32,
        visit: &mut dyn FnMut(i64, i64) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.rows_of(
            &format!(
                "SELECT {}, {} FROM chunks WHERE plane = ?1 AND layer = ?2 ORDER BY cx, cy",
                integer("cx"),
                integer("cy")
            ),
            params![plane, layer],
            &mut |row| {
                visit(
                    chunk_coordinate(row, 0, ErrorCode::Chunk)?,
                    chunk_coordinate(row, 1, ErrorCode::Chunk)?,
                )
            },
        )
    }
}
