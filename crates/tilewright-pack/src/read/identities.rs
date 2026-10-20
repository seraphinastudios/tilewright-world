// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Listing the identities a pack holds, for tools that compare IDs: its
//! render layers, planes, zones, anchors, stairwells and definitions, and
//! the addresses of its chunks.

use super::{
    bounded_identifier, bounded_text, maximum_document_bytes, stored_document, stored_stairwell,
    text,
};
use crate::{Error, Pack, is_identifier};
use rusqlite::Row;
use tilewright_jsonc::{Document, MAX_SAFE_INTEGER, Value};
use tilewright_source::CHUNK_SIZE;

/// One identity of a pack, as [`Pack::visit_identities`] lists it.
/// Documents are the stored canonical JSON: a plane's or definition's
/// document, or a zone's or stairwell's entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Identity<'a> {
    /// A render layer of the profile and its position in `render_layers`.
    RenderLayer {
        name: &'a str,
        position: usize,
    },
    Plane {
        id: &'a str,
        document: &'a str,
    },
    Zone {
        plane: &'a str,
        id: &'a str,
        document: &'a str,
    },
    Anchor {
        plane: &'a str,
        id: &'a str,
        layer: i32,
        at: [i64; 2],
    },
    Stairwell {
        plane: &'a str,
        id: &'a str,
        document: &'a str,
    },
    Definition {
        kind: &'a str,
        id: &'a str,
        document: &'a str,
    },
    /// The address of a chunk: its plane, layer and chunk coordinates.
    Chunk {
        plane: &'a str,
        layer: i32,
        cx: i64,
        cy: i64,
    },
}

/// SQL for `column` when it is an integer, else NULL.
fn integer(column: &str) -> String {
    format!("CASE WHEN typeof({column}) = 'integer' THEN {column} END")
}

/// The identifier in column `index` of a row selected with
/// [`bounded_identifier`].
fn identifier(row: &Row<'_>, index: usize, what: &str) -> Result<String, Error> {
    let value: Option<String> = row.get(index)?;
    value
        .filter(|value| is_identifier(value))
        .ok_or_else(|| Error::Invalid(format!("a {what} key is not an identifier")))
}

/// The 32-bit layer in column `index` of a row selected with [`integer`].
fn layer(row: &Row<'_>, index: usize, what: &str) -> Result<i32, Error> {
    let value: Option<i64> = row.get(index)?;
    value
        .and_then(|layer| i32::try_from(layer).ok())
        .ok_or_else(|| Error::Invalid(format!("a {what} layer is not a 32-bit integer")))
}

/// The integer in column `index` of a row selected with [`integer`], within
/// the source format's integer range.
fn position(row: &Row<'_>, index: usize, what: &str) -> Result<i64, Error> {
    let value: Option<i64> = row.get(index)?;
    value
        .filter(|value| (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(value))
        .ok_or_else(|| {
            Error::Invalid(format!(
                "a {what} position is not an integer within ±{MAX_SAFE_INTEGER}"
            ))
        })
}

/// The chunk coordinate in column `index` of a row selected with
/// [`integer`]: one whose chunk, `CHUNK_SIZE` tiles across as format 1
/// requires, keeps every tile within the format's integer range.
fn chunk_coordinate(row: &Row<'_>, index: usize) -> Result<i64, Error> {
    let size = i64::from(CHUNK_SIZE);
    let range = -(MAX_SAFE_INTEGER / size)..=(MAX_SAFE_INTEGER - (size - 1)) / size;
    let value: Option<i64> = row.get(index)?;
    value.filter(|value| range.contains(value)).ok_or_else(|| {
        Error::Invalid(format!(
            "a chunk coordinate is not an integer from {} to {}",
            range.start(),
            range.end()
        ))
    })
}

/// The error for a stored document that disagrees with its row.
fn disagrees(what: &str) -> Error {
    Error::Invalid(format!("the document of {what} disagrees with its row"))
}

/// A row of `planes` selected as `id, kind, chunk_size, document`, and its
/// stored document. As verification checks, the kind is one format 1
/// defines, the chunk size is [`CHUNK_SIZE`], and the document agrees with
/// the row on its format, ID, kind and chunk size.
fn plane_row(row: &Row<'_>) -> Result<(String, Document), Error> {
    let id = identifier(row, 0, "plane")?;
    let kind: Option<String> = row.get(1)?;
    let chunk_size: Option<i64> = row.get(2)?;
    let stored = stored_document(row.get(3)?, "a plane document")?;
    if !kind
        .as_deref()
        .is_some_and(|kind| ["shared", "instanced", "per_player"].contains(&kind))
        || chunk_size != Some(i64::from(CHUNK_SIZE))
    {
        return Err(Error::Invalid(format!(
            "plane {id} has an invalid kind or a chunk size other than {CHUNK_SIZE}"
        )));
    }
    if text(&stored, "format") != Some("tilewright.plane/1")
        || text(&stored, "id") != Some(&id)
        || text(&stored, "kind") != kind.as_deref()
        || stored.root().get("chunk_size").and_then(Value::as_i64) != chunk_size
    {
        return Err(disagrees(&format!("plane {id}")));
    }
    Ok((id, stored))
}

/// A row of `zones` selected as `plane, id, parent IS NULL, parent,
/// document`, and its stored document, which agrees with the row on its ID
/// and parent, as verification checks.
fn zone_row(row: &Row<'_>) -> Result<(String, String, Document), Error> {
    let (plane, id) = (identifier(row, 0, "zone")?, identifier(row, 1, "zone")?);
    let parent = if row.get::<_, bool>(2)? {
        None
    } else {
        Some(identifier(row, 3, "zone parent")?)
    };
    let stored = stored_document(row.get(4)?, "a zone document")?;
    if text(&stored, "id") != Some(&id)
        || stored.root().get("parent").map(Value::as_str) != parent.as_deref().map(Some)
    {
        return Err(disagrees(&format!("zone {plane}/{id}")));
    }
    Ok((plane, id, stored))
}

impl Pack {
    /// Calls `each` with every row of `sql`, whose `?1`, if any, is the
    /// largest stored document.
    fn each_row(
        &self,
        sql: &str,
        each: &mut dyn FnMut(&Row<'_>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let mut statement = self.connection.prepare(sql)?;
        let mut rows = if statement.parameter_count() == 0 {
            statement.query([])?
        } else {
            statement.query([maximum_document_bytes()])?
        };
        while let Some(row) = rows.next()? {
            each(row)?;
        }
        Ok(())
    }

    /// Verifies the whole pack, as [`Pack::verify`] does, and then calls
    /// `visit` with every render layer of the profile, in the profile's
    /// order, then with every plane, zone, stairwell, definition and
    /// anchor, and then with the address of every chunk: table by table in
    /// that order, each table in primary-key order. Every identity listed
    /// is therefore one a valid pack holds. One stored document is held at
    /// a time.
    ///
    /// # Errors
    /// Returns the first problem verification finds, an error if SQLite
    /// fails or a row is invalid, or the error `visit` returns.
    pub fn visit_identities(
        &self,
        visit: &mut dyn FnMut(Identity<'_>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.verify()?;
        self.visit_rows(visit)
    }

    /// The listing of [`Pack::visit_identities`], without verifying the
    /// pack first. Each value is read within the bounds a single read uses,
    /// and checked against what its row or document repeats, so that a
    /// file that changed after verification still cannot yield an
    /// unbounded or inconsistent identity: the stored profile is checked as
    /// verification checks it, keys are identifiers, documents are
    /// canonical JSON of objects that agree with their rows, stairwells
    /// have the source format's shape, layers and positions are integers
    /// within the format's range, and chunk coordinates keep every tile of
    /// a format 1 chunk within it.
    fn visit_rows(
        &self,
        visit: &mut dyn FnMut(Identity<'_>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let document = bounded_text("document");
        let key = bounded_identifier;

        // The whole profile is checked, so its render layers are 1 to
        // 65,534 unique identifiers.
        self.profile_layers()?;
        let profile = stored_document(self.meta("profile")?, "the stored profile")?;
        let layers = profile
            .root()
            .get("render_layers")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Invalid("the stored profile lists no render layers".into()))?;
        for (position, layer) in layers.iter().enumerate() {
            let name = layer
                .as_str()
                .ok_or_else(|| Error::Invalid("a render layer is not a string".into()))?;
            visit(Identity::RenderLayer { name, position })?;
        }
        drop(profile);

        self.each_row(
            &format!(
                "SELECT {}, {}, {}, {document} FROM planes ORDER BY id",
                key("id"),
                key("kind"),
                integer("chunk_size")
            ),
            &mut |row| {
                let (id, stored) = plane_row(row)?;
                visit(Identity::Plane {
                    id: &id,
                    document: stored.source(),
                })
            },
        )?;
        self.each_row(
            &format!(
                "SELECT {}, {}, parent IS NULL, {}, {document} FROM zones ORDER BY plane, id",
                key("plane"),
                key("id"),
                key("parent")
            ),
            &mut |row| {
                let (plane, id, stored) = zone_row(row)?;
                visit(Identity::Zone {
                    plane: &plane,
                    id: &id,
                    document: stored.source(),
                })
            },
        )?;
        self.each_row(
            &format!(
                "SELECT {}, {}, {document} FROM stairwells ORDER BY plane, id",
                key("plane"),
                key("id")
            ),
            &mut |row| {
                let (plane, id) = (
                    identifier(row, 0, "stairwell")?,
                    identifier(row, 1, "stairwell")?,
                );
                let stairwell =
                    stored_stairwell(&stored_document(row.get(2)?, "a stairwell document")?)?;
                if stairwell.id != id {
                    return Err(disagrees(&format!("stairwell {plane}/{id}")));
                }
                visit(Identity::Stairwell {
                    plane: &plane,
                    id: &id,
                    document: &stairwell.canonical,
                })
            },
        )?;
        self.each_row(
            &format!(
                "SELECT {}, {}, {document} FROM definitions ORDER BY type, id",
                key("type"),
                key("id")
            ),
            &mut |row| {
                let (kind, id) = (
                    identifier(row, 0, "definition")?,
                    identifier(row, 1, "definition")?,
                );
                let stored = stored_document(row.get(2)?, "a definition document")?;
                if text(&stored, "format") != Some("tilewright.def/1")
                    || text(&stored, "type") != Some(&kind)
                    || text(&stored, "id") != Some(&id)
                {
                    return Err(disagrees(&format!("definition {kind}/{id}")));
                }
                visit(Identity::Definition {
                    kind: &kind,
                    id: &id,
                    document: stored.source(),
                })
            },
        )?;
        self.visit_positions(visit)
    }

    /// The anchors and chunk addresses of [`Pack::visit_rows`].
    fn visit_positions(
        &self,
        visit: &mut dyn FnMut(Identity<'_>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let key = bounded_identifier;
        self.each_row(
            &format!(
                "SELECT {}, {}, {}, {}, {} FROM anchors ORDER BY plane, id",
                key("plane"),
                key("id"),
                integer("layer"),
                integer("x"),
                integer("y")
            ),
            &mut |row| {
                visit(Identity::Anchor {
                    plane: &identifier(row, 0, "anchor")?,
                    id: &identifier(row, 1, "anchor")?,
                    layer: layer(row, 2, "anchor")?,
                    at: [position(row, 3, "anchor")?, position(row, 4, "anchor")?],
                })
            },
        )?;
        self.each_row(
            &format!(
                "SELECT {}, {}, {}, {} FROM chunks ORDER BY plane, layer, cx, cy",
                key("plane"),
                integer("layer"),
                integer("cx"),
                integer("cy")
            ),
            &mut |row| {
                visit(Identity::Chunk {
                    plane: &identifier(row, 0, "chunk")?,
                    layer: layer(row, 1, "chunk")?,
                    cx: chunk_coordinate(row, 2)?,
                    cy: chunk_coordinate(row, 3)?,
                })
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::Identity;
    use crate::{Error, Pack, write_pack};
    use rusqlite::Connection;
    use std::path::{Path, PathBuf};

    fn world() -> tilewright_source::World {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/basic");
        tilewright_source::load(&fixture).expect("the fixture is valid")
    }

    /// A pack of the fixture, changed by `sql` unless it is empty.
    fn pack(sql: &str) -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let written = write_pack(&world(), directory.path()).unwrap();
        let copy = directory.path().join("copy.twpack");
        std::fs::copy(&written.path, &copy).unwrap();
        if !sql.is_empty() {
            Connection::open(&copy)
                .unwrap()
                .execute_batch(&format!("{sql}; VACUUM;"))
                .unwrap();
        }
        (directory, copy)
    }

    /// Every identity [`Pack::visit_rows`] lists, without verifying the
    /// pack first.
    fn visit(path: &Path) -> Result<Vec<String>, Error> {
        let mut listed = Vec::new();
        Pack::open(path)?.visit_rows(&mut |identity| {
            listed.push(match identity {
                Identity::RenderLayer { name, position } => {
                    format!("render_layer {name} {position}")
                }
                Identity::Plane { id, document } => format!("plane {id} {document}"),
                Identity::Zone {
                    plane,
                    id,
                    document,
                } => format!("zone {plane}/{id} {document}"),
                Identity::Anchor {
                    plane,
                    id,
                    layer,
                    at,
                } => format!("anchor {plane}/{id} {layer} {at:?}"),
                Identity::Stairwell {
                    plane,
                    id,
                    document,
                } => format!("stairwell {plane}/{id} {document}"),
                Identity::Definition { kind, id, document } => {
                    format!("definition {kind}/{id} {document}")
                }
                Identity::Chunk {
                    plane,
                    layer,
                    cx,
                    cy,
                } => format!("chunk {plane} {layer} {cx} {cy}"),
            });
            Ok(())
        })?;
        Ok(listed)
    }

    #[test]
    fn identities_list_every_row_in_order_with_its_source_document() {
        let world = world();
        let mut expected = Vec::new();
        for (position, name) in world.profile().render_layers.iter().enumerate() {
            expected.push(format!("render_layer {name} {position}"));
        }
        for (id, plane) in world.planes() {
            expected.push(format!("plane {id} {}", plane.canonical));
        }
        for (id, plane) in world.planes() {
            for (zone, entry) in &plane.zones {
                expected.push(format!("zone {id}/{zone} {}", entry.canonical));
            }
        }
        for (id, plane) in world.planes() {
            for (stairwell, entry) in &plane.stairwells {
                expected.push(format!("stairwell {id}/{stairwell} {}", entry.canonical));
            }
        }
        for ((kind, id), definition) in world.definitions() {
            expected.push(format!("definition {kind}/{id} {}", definition.canonical));
        }
        for (id, plane) in world.planes() {
            for (anchor, entry) in &plane.anchors {
                expected.push(format!(
                    "anchor {id}/{anchor} {} {:?}",
                    entry.layer, entry.at
                ));
            }
        }
        for (id, plane) in world.planes() {
            for (layer, cx, cy) in plane.chunks.keys() {
                expected.push(format!("chunk {id} {layer} {cx} {cy}"));
            }
        }
        let (_directory, path) = pack("");
        let listed = visit(&path).unwrap();
        assert_eq!(listed, expected);
        assert_eq!(
            listed
                .iter()
                .filter(|line| line.starts_with("chunk "))
                .count(),
            5
        );

        // A failing visit stops the listing and returns its error.
        let mut seen = 0;
        let error = Pack::open(&path)
            .unwrap()
            .visit_identities(&mut |_| {
                seen += 1;
                Err(Error::Invalid("stop".into()))
            })
            .unwrap_err();
        assert_eq!((seen, error.to_string()), (1, "stop".to_owned()));

        // The verified listing is the same.
        let mut verified = 0;
        Pack::open(&path)
            .unwrap()
            .visit_identities(&mut |_| {
                verified += 1;
                Ok(())
            })
            .unwrap();
        assert_eq!(verified, expected.len());
    }

    #[test]
    fn identities_are_listed_only_from_packs_that_verify() {
        // Each change passes the row checks but not verification, which
        // relates rows to each other.
        for sql in [
            "UPDATE stairwell_steps SET proto = 'stairs_up' WHERE stairwell = 'cave.chute' AND step = 0",
            "INSERT INTO stairwell_steps VALUES ('overworld', 5, 1, 1, 'cave.chute', 7, 'coin')",
            "UPDATE planes SET document = replace(document, '\"wilderness\"', '\"nowhere\"') WHERE id = 'overworld'",
            "UPDATE zones SET plane = 'ghost' WHERE id = 'village'",
            "UPDATE zones SET parent = 'village', document = replace(document, '\"name\"', '\"parent\":\"village\",\"name\"') WHERE id = 'wilderness'",
            "UPDATE anchors SET plane = 'ghost' WHERE id = 'village.well'",
            "UPDATE chunks SET plane = 'ghost' WHERE plane = 'vault'",
            "UPDATE stairwells SET plane = 'ghost' WHERE id = 'cave.chute'",
        ] {
            let (_directory, path) = pack(sql);
            assert!(visit(&path).is_ok(), "{sql}: the row checks accept it");
            let mut seen = 0;
            let result = Pack::open(&path).unwrap().visit_identities(&mut |_| {
                seen += 1;
                Ok(())
            });
            assert!(result.is_err(), "{sql}");
            assert_eq!(seen, 0, "{sql}");
        }
    }

    /// Changes to a pack, each with what the error for it says.
    const MALFORMED: &[(&str, &str)] = &[
        (
            "UPDATE planes SET document = replace(document, '\"vault\"', '\"other\"') WHERE id = 'vault'",
            "disagrees",
        ),
        (
            "UPDATE zones SET document = '{\"id\":\"elsewhere\"}' WHERE id = 'village'",
            "disagrees",
        ),
        (
            "UPDATE zones SET document = ' {}' WHERE id = 'village'",
            "canonical",
        ),
        (
            "UPDATE zones SET id = 'Village' WHERE id = 'village'",
            "identifier",
        ),
        (
            "UPDATE zones SET parent = 'wilderness' WHERE id = 'village'",
            "disagrees",
        ),
        (
            "UPDATE zones SET parent = NULL WHERE id = 'village'",
            "disagrees",
        ),
        (
            "UPDATE zones SET parent = 'x' || char(10) WHERE id = 'village'",
            "identifier",
        ),
        (
            "UPDATE planes SET kind = 'shared' WHERE id = 'vault'",
            "disagrees",
        ),
        (
            "UPDATE planes SET kind = 'odd', document = replace(document, '\"instanced\"', '\"odd\"') WHERE id = 'vault'",
            "invalid kind",
        ),
        (
            "UPDATE planes SET chunk_size = 16 WHERE id = 'vault'",
            "chunk size",
        ),
        (
            "UPDATE planes SET document = replace(document, 'plane/1', 'plane/2') WHERE id = 'vault'",
            "disagrees",
        ),
        (
            "UPDATE anchors SET layer = 4294967296 WHERE id = 'village.well'",
            "32-bit",
        ),
        (
            "UPDATE anchors SET x = 'one' WHERE id = 'village.well'",
            "position",
        ),
        (
            "UPDATE anchors SET y = 9007199254740992 WHERE id = 'village.well'",
            "position",
        ),
        (
            "UPDATE stairwells SET document = replace(document, '\"down\"', '\"sideways\"') WHERE id = 'cave.chute'",
            "shape",
        ),
        (
            "UPDATE stairwells SET id = 'cave.slide' WHERE id = 'cave.chute'",
            "disagrees",
        ),
        (
            "UPDATE definitions SET type = 'loot' WHERE type = 'recipe'",
            "disagrees",
        ),
        (
            "UPDATE definitions SET document = zeroblob(10) WHERE id = 'coin'",
            "canonical",
        ),
        ("UPDATE chunks SET layer = 'x' WHERE layer = 1", "32-bit"),
        (
            "UPDATE meta SET value = '{\"render_layers\":[]}' WHERE key = 'profile'",
            "profile",
        ),
        (
            "UPDATE meta SET value = replace(value, '\"effect\"]', '\"effect\",\"floor\"]') WHERE key = 'profile'",
            "render_layers",
        ),
        (
            "UPDATE meta SET value = '2.0.0' WHERE key = 'profile_version'",
            "profile_version",
        ),
        (
            "UPDATE chunks SET cx = 281474976710656 WHERE plane = 'vault'",
            "chunk coordinate",
        ),
        (
            "UPDATE chunks SET cy = -281474976710656 WHERE plane = 'vault'",
            "chunk coordinate",
        ),
        (
            "UPDATE chunks SET plane = x'00' WHERE plane = 'vault'",
            "identifier",
        ),
    ];

    #[test]
    fn identities_reject_rows_of_the_wrong_shape() {
        for (sql, problem) in MALFORMED {
            let (_directory, path) = pack(sql);
            let error = visit(&path).expect_err(sql).to_string();
            assert!(error.contains(problem), "{sql}: {error}");
        }
        // The outermost chunks whose tiles stay within range are listed.
        let (_directory, path) = pack(
            "UPDATE chunks SET cx = 281474976710655, cy = -281474976710655 WHERE plane = 'vault'",
        );
        assert!(
            visit(&path)
                .unwrap()
                .contains(&"chunk vault 0 281474976710655 -281474976710655".to_owned())
        );
    }
}
