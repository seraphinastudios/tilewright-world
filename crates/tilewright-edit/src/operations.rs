// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Carries out the operations of a transaction on a workspace.

use crate::{
    engine::{Failure, Outcome, Workspace},
    render::{Node, json_string},
    splice::{self, Insertion, Layout, Step},
    terrain,
    transaction::{
        Action, Content, Destination, Expected, ObjectAddress, Operation, Segment, Target, Tile,
        Transaction, chunk_of,
    },
};
use std::collections::BTreeSet;
use tilewright_jsonc::{Document, Limits, Span, Value};
use tilewright_source::{CHUNK_SIZE, is_identifier};

/// Failures of one operation, each at a span of the transaction.
pub(crate) type Failures = Vec<(Span, Failure)>;

/// A short form of a value for messages. A large value is described, not
/// rendered, so a message costs little whatever the value's size.
pub(crate) fn shown(document: &Document, value: &Value) -> String {
    if value.span().len() > 4_096 {
        return format!("{} of {} bytes", value.kind(), value.span().len());
    }
    let mut text = document.canonical(value);
    if text.len() > 120 {
        let mut end = 117;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("...");
    }
    text
}

/// Where a tile's chunk lives and the tile's place in it.
pub(crate) struct Site {
    pub path: String,
    pub chunk: [i64; 2],
    pub local: [u16; 2],
}

pub(crate) fn chunk_path(plane: &str, layer: i32, chunk: [i64; 2]) -> String {
    format!(
        "planes/{plane}/layers/{layer}/{}_{}.jsonc",
        chunk[0], chunk[1]
    )
}

/// The plane, layer and chunk of a chunk path, if it is one: only the
/// paths [`chunk_path`] gives for chunks a plane can hold.
pub(crate) fn chunk_site(path: &str) -> Option<(String, i32, [i64; 2])> {
    let rest = path.strip_prefix("planes/")?;
    let (plane, rest) = rest.split_once('/')?;
    let (layer, name) = rest.strip_prefix("layers/")?.split_once('/')?;
    let (cx, cy) = name.strip_suffix(".jsonc")?.split_once('_')?;
    let layer: i32 = layer.parse().ok()?;
    let chunk: [i64; 2] = [cx.parse().ok()?, cy.parse().ok()?];
    let size = i64::from(CHUNK_SIZE);
    let held = chunk.iter().all(|coordinate| {
        coordinate
            .checked_mul(size)
            .and_then(chunk_of)
            .is_some_and(|(found, _)| found == *coordinate)
    });
    (held && is_identifier(plane) && chunk_path(plane, layer, chunk) == path)
        .then(|| (plane.to_owned(), layer, chunk))
}

pub(crate) fn site(plane: &str, layer: i32, at: [i64; 2]) -> Site {
    let (cx, lx) = chunk_of(at[0]).unwrap_or_default();
    let (cy, ly) = chunk_of(at[1]).unwrap_or_default();
    Site {
        path: chunk_path(plane, layer, [cx, cy]),
        chunk: [cx, cy],
        local: [lx, ly],
    }
}

/// Creates the chunk at `site` if it is missing, holding only its header.
pub(crate) fn ensure_chunk(
    workspace: &mut Workspace<'_>,
    plane: &str,
    layer: i32,
    site: &Site,
) -> Outcome<()> {
    if workspace.exists(&site.path)? {
        return Ok(());
    }
    let header = Node::Object(vec![
        ("format".to_owned(), Node::string("tilewright.chunk/1")),
        ("plane".to_owned(), Node::string(plane)),
        ("layer".to_owned(), Node::integer(i64::from(layer))),
        (
            "chunk".to_owned(),
            Node::Array(vec![
                Node::integer(site.chunk[0]),
                Node::integer(site.chunk[1]),
            ]),
        ),
    ]);
    workspace.create(&site.path, header.file())
}

/// The `objects` of a chunk: the array positions of the objects on tile
/// `local`, in order, and the array's length (`None` when the chunk has
/// no `objects`).
fn tile_objects(
    document: &Document,
    path: &str,
    local: [u16; 2],
) -> Outcome<(Vec<usize>, Option<usize>)> {
    let Some(objects) = document.root().get("objects") else {
        return Ok((Vec::new(), None));
    };
    let items = objects
        .as_array()
        .ok_or_else(|| Failure::conflict(format!("{path}: objects is not an array")))?;
    let mut positions = Vec::new();
    for (position, item) in items.iter().enumerate() {
        let at = item.get("at").and_then(Value::as_array);
        let at = match at {
            Some([x, y]) => (x.as_i64(), y.as_i64()),
            _ => (None, None),
        };
        let (Some(x), Some(y)) = at else {
            return Err(Failure::conflict(format!(
                "{path}: object {position} has no valid \"at\"; the chunk cannot be edited"
            )));
        };
        if x == i64::from(local[0]) && y == i64::from(local[1]) {
            positions.push(position);
        }
    }
    Ok((positions, Some(items.len())))
}

fn tile_name(tile: &Tile) -> String {
    format!(
        "tile ({}, {}) of layer {} of plane \"{}\"",
        tile.at[0], tile.at[1], tile.layer, tile.plane
    )
}

/// An addressed object: its chunk, the steps to it there, and, for an
/// object on a tile rather than in an inventory, the tile and its index
/// among the tile's objects.
struct Located {
    path: String,
    pointer: Vec<Step>,
    place: Option<(Tile, usize)>,
}

/// Finds an addressed object: by its place on a tile, or by its uid
/// wherever it is now.
fn locate(workspace: &mut Workspace<'_>, address: &ObjectAddress) -> Outcome<Located> {
    let (tile, index) = match address {
        ObjectAddress::Place { tile, index } => (tile, *index),
        ObjectAddress::Uid(uid) => {
            let (path, pointer) = workspace.find_uid(uid)?;
            let place = match pointer.as_slice() {
                [_, Step::Index(position)] => Some(place_of(workspace, &path, *position)?),
                _ => None,
            };
            return Ok(Located {
                path,
                pointer,
                place,
            });
        }
    };
    let site = site(&tile.plane, tile.layer, tile.at);
    let missing = || {
        Failure::conflict(format!(
            "{} has no object at index {index}",
            tile_name(tile)
        ))
    };
    if !workspace.exists(&site.path)? {
        return Err(missing());
    }
    let document = workspace.document(&site.path)?;
    let (positions, _) = tile_objects(document, &site.path, site.local)?;
    let position = *positions.get(index).ok_or_else(missing)?;
    Ok(Located {
        path: site.path,
        pointer: vec![Step::Key("objects".to_owned()), Step::Index(position)],
        place: Some((tile.clone(), index)),
    })
}

/// The tile of the object at `position` of the `objects` of the chunk at
/// `path`, and the object's index among the tile's objects.
fn place_of(workspace: &mut Workspace<'_>, path: &str, position: usize) -> Outcome<(Tile, usize)> {
    let invalid = || {
        Failure::conflict(format!(
            "{path}: object {position} has no valid \"at\"; the chunk cannot be edited"
        ))
    };
    // The uid index holds chunk paths only.
    let (plane, layer, chunk) =
        chunk_site(path).ok_or_else(|| Failure::conflict(format!("{path} is not a chunk")))?;
    let document = workspace.document(path)?;
    let at = document
        .root()
        .get("objects")
        .and_then(Value::as_array)
        .and_then(|objects| objects.get(position))
        .and_then(|object| object.get("at"))
        .and_then(Value::as_array);
    let size = i64::from(CHUNK_SIZE);
    let local = |value: &Value| {
        value
            .as_i64()
            .filter(|local| (0..size).contains(local))
            .and_then(|local| u16::try_from(local).ok())
    };
    let Some([Some(x), Some(y)]) = at.and_then(|at| match at {
        [x, y] => Some([local(x), local(y)]),
        _ => None,
    }) else {
        return Err(invalid());
    };
    let (positions, _) = tile_objects(document, path, [x, y])?;
    let index = positions
        .iter()
        .position(|found| *found == position)
        .ok_or_else(invalid)?;
    // The chunk is one a plane can hold, so its tiles are in range.
    let at = [
        chunk[0] * size + i64::from(x),
        chunk[1] * size + i64::from(y),
    ];
    Ok((Tile { plane, layer, at }, index))
}

/// How messages name an addressed object.
fn object_name(address: &ObjectAddress) -> String {
    match address {
        ObjectAddress::Place { tile, index } => format!("object {index} on {}", tile_name(tile)),
        ObjectAddress::Uid(uid) => format!("object {}", crate::diagnostic::quoted(uid)),
    }
}

/// Checks an object against `expect`: every member it names is present
/// with an equal value.
fn check_object(
    document: &Document,
    object: &Value,
    transaction: &Transaction,
    expect: Span,
    address: &ObjectAddress,
) -> Outcome<()> {
    for member in transaction.value(expect).as_object().unwrap_or_default() {
        let found = object.get(&member.key);
        if !found.is_some_and(|value| equal(document, value, &transaction.document, &member.value))
        {
            return Err(Failure::conflict(format!(
                "{} has {} {}, expected {}",
                object_name(address),
                crate::diagnostic::quoted(&member.key),
                found.map_or_else(|| "absent".to_owned(), |value| shown(document, value)),
                shown(&transaction.document, &member.value)
            )));
        }
    }
    Ok(())
}

/// `object`, an object's text, with its `at` set to `local`.
fn with_tile(object: &str, local: [u16; 2]) -> Outcome<String> {
    let invalid = || Failure::conflict("the object is not valid JSONC");
    let document = Document::parse(object, &Limits::default()).map_err(|_| invalid())?;
    let at = format!("[{}, {}]", local[0], local[1]);
    let root = document.root();
    if let Some(current) = root.get("at") {
        return Ok(splice::splice(object, current.span(), &at));
    }
    let insertion = Insertion {
        key: Some(("at", "\"at\"")),
        value: &at,
        layout: None,
        colon: None,
    };
    splice::insert(object, root, 0, &insertion)
        .map(|inserted| inserted.text)
        .map_err(Failure::conflict)
}

/// Inserts the object text `object`, whose `at` is already the tile's, at
/// `index` among the objects of `tile`.
fn insert_object(
    workspace: &mut Workspace<'_>,
    tile: &Tile,
    index: Option<usize>,
    object: &str,
) -> Outcome<()> {
    let site = site(&tile.plane, tile.layer, tile.at);
    ensure_chunk(workspace, &tile.plane, tile.layer, &site)?;
    let document = workspace.document(&site.path)?;
    let (positions, length) = tile_objects(document, &site.path, site.local)?;
    let index = index.unwrap_or(positions.len());
    if index > positions.len() {
        return Err(Failure::conflict(format!(
            "{} has {} object(s); index {index} is past the end",
            tile_name(tile),
            positions.len()
        )));
    }
    let Some(length) = length else {
        let members = document.root().as_object().map_or(0, <[_]>::len);
        let value = format!("[{object}]");
        let insertion = Insertion {
            key: Some(("objects", "\"objects\"")),
            value: &value,
            layout: None,
            colon: None,
        };
        return workspace.insert(&site.path, &[], members, &insertion);
    };
    let position = match (positions.get(index), positions.last()) {
        (Some(position), _) => *position,
        (None, Some(last)) => last + 1,
        (None, None) => length,
    };
    let insertion = Insertion {
        key: None,
        value: object,
        layout: None,
        colon: None,
    };
    workspace.insert(
        &site.path,
        &[Step::Key("objects".to_owned())],
        position,
        &insertion,
    )
}

/// The steps of a pointer in `document`, resolving `{"id": ...}`
/// selectors. Every step but the last must exist.
fn steps(document: &Document, path: &str, pointer: &[Segment]) -> Outcome<Vec<Step>> {
    let mut value = Some(document.root());
    let mut steps = Vec::with_capacity(pointer.len());
    for (position, segment) in pointer.iter().enumerate() {
        let Some(current) = value else {
            return Err(Failure::conflict(format!(
                "{path} has no value at step {position} of the pointer"
            )));
        };
        let step = match segment {
            Segment::Key(key) => Step::Key(key.clone()),
            Segment::Index(index) => Step::Index(*index),
            Segment::Id(id) => {
                let items = current.as_array().unwrap_or_default();
                let mut matches = items.iter().enumerate().filter(|(_, item)| {
                    item.get("id").and_then(Value::as_str) == Some(id.as_str())
                });
                match (matches.next(), matches.next()) {
                    (Some((index, _)), None) => Step::Index(index),
                    (None, _) => {
                        return Err(Failure::conflict(format!(
                            "{path} has no entry with id \"{id}\" there"
                        )));
                    }
                    (Some(_), Some(_)) => {
                        return Err(Failure::conflict(format!(
                            "{path} has more than one entry with id \"{id}\" there"
                        )));
                    }
                }
            }
        };
        value = splice::find(current, std::slice::from_ref(&step));
        steps.push(step);
    }
    Ok(steps)
}

fn describe(steps: &[Step]) -> String {
    let parts: Vec<String> = steps
        .iter()
        .map(|step| match step {
            Step::Key(key) => crate::diagnostic::quoted(key),
            Step::Index(index) => index.to_string(),
        })
        .collect();
    format!("[{}]", parts.join(", "))
}

/// The file and pointer prefix of a target.
fn target(workspace: &mut Workspace<'_>, target: &Target) -> Outcome<(String, Vec<Segment>)> {
    match target {
        Target::Path(path) => Ok((path.clone(), Vec::new())),
        Target::Object(address) => {
            let located = locate(workspace, address)?;
            let prefix = located
                .pointer
                .into_iter()
                .map(|step| match step {
                    Step::Key(key) => Segment::Key(key),
                    Step::Index(index) => Segment::Index(index),
                })
                .collect();
            Ok((located.path, prefix))
        }
    }
}

/// Whether two values have equal canonical JSON. Values of different kinds
/// differ without rendering either.
fn equal(document: &Document, value: &Value, other_document: &Document, other: &Value) -> bool {
    value.kind() == other.kind() && document.canonical(value) == other_document.canonical(other)
}

/// Checks that the value at `steps` equals the transaction's `expect`.
fn check_value(
    workspace: &mut Workspace<'_>,
    path: &str,
    steps: &[Step],
    transaction: &Transaction,
    expect: Span,
) -> Outcome<()> {
    let wanted = transaction.value(expect);
    let document = workspace.document(path)?;
    match splice::find(document.root(), steps) {
        Some(found) if equal(document, found, &transaction.document, wanted) => Ok(()),
        found => Err(Failure::conflict(format!(
            "{path} {} is {}, expected {}",
            describe(steps),
            found.map_or_else(|| "absent".to_owned(), |value| shown(document, value)),
            shown(&transaction.document, wanted)
        ))),
    }
}

/// The file of a target, and its steps with `pointer` after the target's
/// own prefix.
fn resolved(
    workspace: &mut Workspace<'_>,
    aim: &Target,
    pointer: &[Segment],
) -> Outcome<(String, Vec<Step>)> {
    let (path, mut full) = target(workspace, aim)?;
    full.extend(pointer.iter().cloned());
    let document = workspace.document(&path)?;
    let steps = steps(document, &path, &full)?;
    Ok((path, steps))
}

/// `set`: replaces a value, or adds a missing member at the end of its
/// object.
fn set_value(
    workspace: &mut Workspace<'_>,
    transaction: &Transaction,
    aim: &Target,
    pointer: &[Segment],
    key_text: Option<Span>,
    value: Span,
    expect: Option<Span>,
) -> Outcome<()> {
    let (path, steps) = resolved(workspace, aim, pointer)?;
    let value = transaction.document.text(value);
    if let Some(expect) = expect {
        check_value(workspace, &path, &steps, transaction, expect)?;
    }
    let document = workspace.document(&path)?;
    if splice::find(document.root(), &steps).is_some() {
        return workspace.set(&path, &steps, value);
    }
    let Some((Step::Key(key), parent)) = steps.split_last() else {
        return Err(Failure::conflict(format!(
            "{path} has no value at {}",
            describe(&steps)
        )));
    };
    let members = splice::find(document.root(), parent)
        .and_then(Value::as_object)
        .ok_or_else(|| Failure::conflict(format!("{path} has no object at {}", describe(parent))))?
        .len();
    // The key is written as the transaction spells it.
    let key_text = key_text.map_or_else(
        || json_string(key),
        |span| transaction.document.text(span).to_owned(),
    );
    let insertion = Insertion {
        key: Some((key, &key_text)),
        value,
        layout: None,
        colon: None,
    };
    workspace.insert(&path, parent, members, &insertion)
}

#[allow(clippy::too_many_arguments)]
fn insert_value(
    workspace: &mut Workspace<'_>,
    transaction: &Transaction,
    aim: &Target,
    pointer: &[Segment],
    index: usize,
    key: Option<Span>,
    value: Span,
    layout: Option<&(String, String)>,
    colon: Option<&str>,
) -> Outcome<()> {
    let (path, steps) = resolved(workspace, aim, pointer)?;
    let decoded = key.map(|span| {
        transaction
            .value(span)
            .as_str()
            .unwrap_or_default()
            .to_owned()
    });
    let insertion = Insertion {
        key: decoded
            .as_deref()
            .zip(key.map(|span| transaction.document.text(span))),
        value: transaction.document.text(value),
        layout: layout.map(|(before, after)| Layout {
            before: before.clone(),
            after: after.clone(),
        }),
        colon,
    };
    workspace.insert(&path, &steps, index, &insertion)
}

fn remove_value(
    workspace: &mut Workspace<'_>,
    transaction: &Transaction,
    aim: &Target,
    pointer: &[Segment],
    expect: Option<Span>,
    separator: Option<&str>,
) -> Outcome<()> {
    let (path, steps) = resolved(workspace, aim, pointer)?;
    if let Some(expect) = expect {
        check_value(workspace, &path, &steps, transaction, expect)?;
    }
    workspace.remove(&path, &steps, separator)
}

fn test_value(
    workspace: &mut Workspace<'_>,
    transaction: &Transaction,
    aim: &Target,
    pointer: &[Segment],
    expected: &Expected,
) -> Outcome<()> {
    let (path, prefix) = target(workspace, aim)?;
    if !workspace.exists(&path)? {
        return match (expected, prefix.is_empty() && pointer.is_empty()) {
            (Expected::Absent, true) => Ok(()),
            _ => Err(Failure::conflict(format!("{path} does not exist"))),
        };
    }
    // An absent test may end in an `{"id": ...}` selector that matches no
    // entry: then there is none, as the test expects.
    if let (Expected::Absent, Some((Segment::Id(id), parent))) = (expected, pointer.split_last()) {
        let (path, steps) = resolved(workspace, aim, parent)?;
        let document = workspace.document(&path)?;
        let matching = splice::find(document.root(), &steps)
            .and_then(Value::as_array)
            .ok_or_else(|| {
                Failure::conflict(format!("{path} has no array at {}", describe(&steps)))
            })?
            .iter()
            .filter(|item| item.get("id").and_then(Value::as_str) == Some(id.as_str()))
            .count();
        return if matching == 0 {
            Ok(())
        } else {
            Err(Failure::conflict(format!(
                "{path} {} has an entry with id \"{id}\", expected none",
                describe(&steps)
            )))
        };
    }
    let (path, steps) = resolved(workspace, aim, pointer)?;
    if let Expected::Value(expect) = expected {
        return check_value(workspace, &path, &steps, transaction, *expect);
    }
    let document = workspace.document(&path)?;
    match expected {
        Expected::Value(_) => Ok(()),
        Expected::Absent => match splice::find(document.root(), &steps) {
            None => Ok(()),
            Some(found) => Err(Failure::conflict(format!(
                "{path} {} is {}, expected it to be absent",
                describe(&steps),
                shown(document, found)
            ))),
        },
    }
}

/// The addressed object, checked against `expect`, and its exact text.
fn expected_object(
    workspace: &mut Workspace<'_>,
    transaction: &Transaction,
    address: &ObjectAddress,
    expect: Span,
) -> Outcome<(Located, String)> {
    let located = locate(workspace, address)?;
    let document = workspace.document(&located.path)?;
    let object = splice::find(document.root(), &located.pointer)
        .ok_or_else(|| Failure::conflict("the object is missing"))?;
    check_object(document, object, transaction, expect, address)?;
    let text = document.text(object.span()).to_owned();
    Ok((located, text))
}

/// `object.insert`, `object.remove` and `object.move`. The chunks of the
/// tiles they edit are added to `chunks`.
fn object_operation(
    workspace: &mut Workspace<'_>,
    transaction: &Transaction,
    action: &Action,
    chunks: &mut BTreeSet<String>,
) -> Outcome<()> {
    match action {
        Action::ObjectInsert {
            tile,
            index,
            object,
        } => {
            let site = site(&tile.plane, tile.layer, tile.at);
            chunks.insert(site.path);
            let object = with_tile(transaction.document.text(*object), site.local)?;
            insert_object(workspace, tile, *index, &object)
        }
        Action::ObjectRemove { address, expect } => {
            let (located, _) = expected_object(workspace, transaction, address, *expect)?;
            chunks.insert(located.path.clone());
            workspace.remove(&located.path, &located.pointer, None)
        }
        Action::ObjectMove {
            address,
            expect,
            to,
            index,
        } => move_object(workspace, transaction, address, *expect, to, *index, chunks),
        _ => Ok(()),
    }
}

/// `object.move`: the destination's parts not given are the object's own.
fn move_object(
    workspace: &mut Workspace<'_>,
    transaction: &Transaction,
    address: &ObjectAddress,
    expect: Span,
    to: &Destination,
    index: Option<usize>,
    chunks: &mut BTreeSet<String>,
) -> Outcome<()> {
    let (located, mut moved) = expected_object(workspace, transaction, address, expect)?;
    let Some((from, own_index)) = located.place else {
        return Err(Failure::conflict(format!(
            "{} is in an inventory; move it by removing it there and inserting it elsewhere",
            object_name(address)
        )));
    };
    let to = Tile {
        plane: to.plane.clone().unwrap_or_else(|| from.plane.clone()),
        layer: to.layer.unwrap_or(from.layer),
        at: to.at.unwrap_or(from.at),
    };
    let source = site(&from.plane, from.layer, from.at);
    let destination = site(&to.plane, to.layer, to.at);
    chunks.insert(source.path.clone());
    chunks.insert(destination.path.clone());
    // A move to where the object already is changes nothing, not even its
    // separators.
    if to == from {
        let document = workspace.document(&located.path)?;
        let count = tile_objects(document, &located.path, source.local)?.0.len();
        if index.unwrap_or(count - 1) == own_index {
            return Ok(());
        }
    }
    workspace.remove(&located.path, &located.pointer, None)?;
    if destination.local != source.local {
        moved = with_tile(&moved, destination.local)?;
    }
    insert_object(workspace, &to, index, &moved)
}

fn document_operation(
    workspace: &mut Workspace<'_>,
    transaction: &Transaction,
    action: &Action,
) -> Outcome<()> {
    match action {
        Action::Set {
            target,
            pointer,
            key,
            value,
            expect,
        } => set_value(
            workspace,
            transaction,
            target,
            pointer,
            *key,
            *value,
            *expect,
        ),
        Action::Insert {
            target,
            pointer,
            index,
            key,
            value,
            layout,
            colon,
        } => insert_value(
            workspace,
            transaction,
            target,
            pointer,
            *index,
            *key,
            *value,
            layout.as_ref(),
            colon.as_deref(),
        ),
        Action::Remove {
            target,
            pointer,
            expect,
            separator,
        } => remove_value(
            workspace,
            transaction,
            target,
            pointer,
            *expect,
            separator.as_deref(),
        ),
        Action::Test {
            target,
            pointer,
            expected,
        } => test_value(workspace, transaction, target, pointer, expected),
        Action::Create { path, content } => {
            let text = match content {
                Content::Document(span) => {
                    Node::from_value(&transaction.document, transaction.value(*span)).file()
                }
                Content::Text(text) => text.clone(),
            };
            workspace.create(path, text)
        }
        Action::Delete { path } => workspace.delete(path).map(|_| ()),
        Action::Rename { path, to } => {
            if workspace.exists(to)? {
                return Err(Failure::conflict(format!("{to} already exists")));
            }
            let text = workspace.delete(path)?;
            workspace.create(to, text)
        }
        Action::ObjectInsert { .. }
        | Action::ObjectRemove { .. }
        | Action::ObjectMove { .. }
        | Action::TerrainSet { .. } => Ok(()),
    }
}

/// Carries out one operation. Chunks that tile operations touch are added
/// to `chunks`.
pub(crate) fn run(
    workspace: &mut Workspace<'_>,
    transaction: &Transaction,
    operation: &Operation,
    chunks: &mut BTreeSet<String>,
) -> Result<(), Failures> {
    let at = |failure| vec![(operation.span, failure)];
    match &operation.action {
        Action::TerrainSet {
            plane,
            layer,
            grid,
            cells,
        } => {
            terrain::set_cells(workspace, plane, *layer, grid, cells, chunks).map_err(|failures| {
                failures
                    .into_iter()
                    .map(|(span, failure)| (span.unwrap_or(operation.span), failure))
                    .collect()
            })
        }
        Action::ObjectInsert { .. } | Action::ObjectRemove { .. } | Action::ObjectMove { .. } => {
            object_operation(workspace, transaction, &operation.action, chunks).map_err(at)
        }
        action => document_operation(workspace, transaction, action).map_err(at),
    }
}
