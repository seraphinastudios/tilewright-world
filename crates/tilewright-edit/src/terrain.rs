// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Terrain cell edits. A grid is edited in site: changed rows or heights
//! are replaced, palette entries are added and dropped as values appear and
//! disappear, a `fill` grid becomes `rows` when its cells differ, and a
//! grid whose cells are all empty is removed.

use crate::{
    engine::{Failure, Outcome, Primitive, Workspace},
    limits,
    operations::{Site, ensure_chunk, site},
    render::{Node, json_string},
    splice::{self, Insertion, Step},
    transaction::{Cell, CellValue},
};
use std::collections::{BTreeMap, BTreeSet};
use tilewright_jsonc::{Span, Value};
use tilewright_source::CHUNK_SIZE;

const SIDE: usize = CHUNK_SIZE as usize;
const CELLS: usize = SIDE * SIDE;

/// Characters palette keys are allocated from, in order: visible ASCII
/// without `.`, `"` and `\`.
const KEY_ALPHABET: &[u8] =
    b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789!#$%&'()*+,-/:;<=>?@[]^_`{|}~";

/// A character grid: palette keys of `width` characters, and per cell 0 for
/// empty or a palette index plus one.
#[derive(Clone, Debug)]
struct Characters {
    width: usize,
    palette: Vec<(String, String)>,
    cells: Vec<usize>,
    fill: bool,
    /// The keys in use, and the first entry of each value, so that a cell
    /// costs a lookup however large the palette is.
    keys: BTreeSet<String>,
    values: BTreeMap<String, usize>,
}

#[derive(Clone, Debug)]
struct Heights {
    cells: Vec<i32>,
    fill: bool,
}

fn read_characters(value: &Value) -> Option<Characters> {
    let width = match value.get("cell_width").map(Value::as_i64) {
        None | Some(Some(1)) => 1,
        Some(Some(2)) => 2,
        Some(_) => return None,
    };
    let palette: Vec<(String, String)> = value
        .get("palette")?
        .as_object()?
        .iter()
        .map(|member| Some((member.key.clone(), member.value.as_str()?.to_owned())))
        .collect::<Option<_>>()?;
    let index: BTreeMap<&str, usize> = palette
        .iter()
        .enumerate()
        .map(|(position, (key, _))| (key.as_str(), position + 1))
        .collect();
    let lookup = |key: &str| {
        if key.len() == width && key.bytes().all(|byte| byte == b'.') {
            Some(0)
        } else {
            index.get(key).copied()
        }
    };
    let (cells, fill) = match (value.get("rows"), value.get("fill")) {
        (Some(rows), None) => {
            let rows = rows.as_array()?;
            if rows.len() != SIDE {
                return None;
            }
            let mut cells = Vec::with_capacity(CELLS);
            for row in rows {
                let text = row.as_str()?;
                if !text.is_ascii() || text.len() != SIDE * width {
                    return None;
                }
                for key in text.as_bytes().chunks(width) {
                    cells.push(lookup(std::str::from_utf8(key).ok()?)?);
                }
            }
            (cells, false)
        }
        (None, Some(fill)) => (vec![lookup(fill.as_str()?)?; CELLS], true),
        _ => return None,
    };
    let mut grid = Characters {
        width,
        palette: Vec::new(),
        cells,
        fill,
        keys: BTreeSet::new(),
        values: BTreeMap::new(),
    };
    for (key, value) in palette {
        grid.push(key, value);
    }
    Some(grid)
}

fn read_heights(value: &Value) -> Option<Heights> {
    let integer = |value: &Value| i32::try_from(value.as_i64()?).ok();
    match (value.get("rows"), value.get("fill")) {
        (Some(rows), None) => {
            let rows = rows.as_array()?;
            if rows.len() != SIDE {
                return None;
            }
            let mut cells = Vec::with_capacity(CELLS);
            for row in rows {
                let row = row.as_array()?;
                if row.len() != SIDE {
                    return None;
                }
                for cell in row {
                    cells.push(integer(cell)?);
                }
            }
            Some(Heights { cells, fill: false })
        }
        (None, Some(fill)) => Some(Heights {
            cells: vec![integer(fill)?; CELLS],
            fill: true,
        }),
        _ => None,
    }
}

/// Whether a grid holds nothing: every cell empty, or every height 0.
/// A grid that cannot be read is not empty.
pub(crate) fn grid_is_empty(name: &str, value: &Value) -> bool {
    if name == "height" {
        read_heights(value).is_some_and(|grid| grid.cells.iter().all(|cell| *cell == 0))
    } else {
        read_characters(value).is_some_and(|grid| grid.cells.iter().all(|cell| *cell == 0))
    }
}

/// Whether a chunk document holds no content: no objects, and only empty
/// grids.
pub(crate) fn chunk_is_empty(root: &Value) -> bool {
    let objects = root
        .get("objects")
        .is_none_or(|objects| objects.as_array().is_some_and(<[Value]>::is_empty));
    let terrain = root.get("terrain").is_none_or(|terrain| {
        terrain.as_object().is_some_and(|members| {
            members
                .iter()
                .all(|member| grid_is_empty(&member.key, &member.value))
        })
    });
    objects && terrain
}

/// A key of `width` characters for `value` that is not in `used`: the
/// value's own first characters if they are free, or else the first free
/// key in alphabet order.
fn allocate(width: usize, used: &BTreeSet<String>, value: &str) -> Option<String> {
    let preferred = value.get(..width).filter(|prefix| {
        prefix.len() == width && prefix.bytes().all(|byte| KEY_ALPHABET.contains(&byte))
    });
    if let Some(prefix) = preferred
        && !used.contains(prefix)
    {
        return Some(prefix.to_owned());
    }
    let letters = KEY_ALPHABET.iter().map(|byte| char::from(*byte));
    if width == 1 {
        letters
            .map(String::from)
            .find(|key| !used.contains(key.as_str()))
    } else {
        letters
            .flat_map(|first| {
                KEY_ALPHABET
                    .iter()
                    .map(move |second| format!("{first}{}", char::from(*second)))
            })
            .find(|key| !used.contains(key.as_str()))
    }
}

impl Characters {
    fn empty() -> Self {
        Self::blank(1)
    }

    fn blank(width: usize) -> Self {
        Self {
            width,
            palette: Vec::new(),
            cells: vec![0; CELLS],
            fill: false,
            keys: BTreeSet::new(),
            values: BTreeMap::new(),
        }
    }

    /// Adds a palette entry; returns its cell value.
    fn push(&mut self, key: String, value: String) -> usize {
        let cell = self.palette.len() + 1;
        if !key.is_empty() {
            self.keys.insert(key.clone());
        }
        self.values.entry(value.clone()).or_insert(cell);
        self.palette.push((key, value));
        cell
    }

    fn value(&self, cell: usize) -> CellValue {
        match cell
            .checked_sub(1)
            .and_then(|index| self.palette.get(index))
        {
            Some((_, value)) => CellValue::Id(value.clone()),
            None => CellValue::Empty,
        }
    }

    fn key(&self, cell: usize) -> String {
        match cell
            .checked_sub(1)
            .and_then(|index| self.palette.get(index))
        {
            Some((key, _)) => key.clone(),
            None => ".".repeat(self.width),
        }
    }

    /// The cell for `value`, adding a palette entry if needed; `None` when
    /// no key of this width is free.
    fn cell_for(&mut self, value: &CellValue) -> Option<usize> {
        let CellValue::Id(id) = value else {
            return Some(0);
        };
        if let Some(cell) = self.values.get(id) {
            return Some(*cell);
        }
        let key = allocate(self.width, &self.keys, id)?;
        Some(self.push(key, id.clone()))
    }

    fn row(&self, row: usize) -> String {
        self.cells[row * SIDE..(row + 1) * SIDE]
            .iter()
            .map(|cell| self.key(*cell))
            .collect()
    }

    fn used(&self) -> BTreeSet<usize> {
        self.cells
            .iter()
            .copied()
            .filter(|cell| *cell > 0)
            .collect()
    }

    /// The grid as a new value: its used palette entries in palette order,
    /// and `fill` when every cell is the same.
    fn node(&self) -> Node {
        let used = self.used();
        let mut members = Vec::new();
        if self.width == 2 {
            members.push(("cell_width".to_owned(), Node::integer(2)));
        }
        members.push((
            "palette".to_owned(),
            Node::Object(
                self.palette
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| used.contains(&(index + 1)))
                    .map(|(_, (key, value))| (key.clone(), Node::string(value)))
                    .collect(),
            ),
        ));
        if self.cells.iter().all(|cell| *cell == self.cells[0]) {
            members.push(("fill".to_owned(), Node::string(&self.key(self.cells[0]))));
        } else {
            members.push(("rows".to_owned(), self.rows_node()));
        }
        Node::Object(members)
    }

    fn rows_node(&self) -> Node {
        Node::Array((0..SIDE).map(|row| Node::String(self.row(row))).collect())
    }

    /// The same grid with two-character keys and only the palette entries
    /// it uses, for a palette that has run out of keys; `None` if even that
    /// does not fit.
    fn widened(&self) -> Option<Self> {
        let mut wide = Self::blank(2);
        let mut mapping = BTreeMap::new();
        for cell in self.used() {
            mapping.insert(cell, wide.cell_for(&self.value(cell))?);
        }
        for (index, cell) in self.cells.iter().enumerate() {
            wide.cells[index] = mapping.get(cell).copied().unwrap_or(0);
        }
        Some(wide)
    }
}

impl Heights {
    fn empty() -> Self {
        Self {
            cells: vec![0; CELLS],
            fill: false,
        }
    }

    fn node(&self) -> Node {
        if self.cells.iter().all(|cell| *cell == self.cells[0]) {
            Node::Object(vec![(
                "fill".to_owned(),
                Node::integer(i64::from(self.cells[0])),
            )])
        } else {
            Node::Object(vec![("rows".to_owned(), self.rows_node())])
        }
    }

    fn rows_node(&self) -> Node {
        Node::Array(
            self.cells
                .chunks(SIDE)
                .map(|row| {
                    Node::Array(
                        row.iter()
                            .map(|cell| Node::integer(i64::from(*cell)))
                            .collect(),
                    )
                })
                .collect(),
        )
    }
}

fn key_step(key: &str) -> Step {
    Step::Key(key.to_owned())
}

/// Inserts a new grid into the chunk's `terrain`, creating `terrain` if
/// needed. Grids keep the order ground layers, `height`, `zone`.
fn insert_grid(workspace: &mut Workspace<'_>, path: &str, grid: &str, node: Node) -> Outcome<()> {
    let document = workspace.document(path)?;
    let root = document.root();
    let grid_key = json_string(grid);
    if let Some(terrain) = root.get("terrain") {
        let members = terrain
            .as_object()
            .ok_or_else(|| Failure::conflict(format!("{path}: terrain is not an object")))?;
        let after: &[&str] = match grid {
            "zone" => &[],
            "height" => &["zone"],
            _ => &["height", "zone"],
        };
        let position = members
            .iter()
            .position(|member| after.contains(&member.key.as_str()))
            .unwrap_or(members.len());
        let text = node.layout(&splice::item_indentation(
            document.source(),
            terrain,
            position,
        ));
        let insertion = Insertion {
            key: Some((grid, &grid_key)),
            value: &text,
            layout: None,
            colon: None,
        };
        workspace.insert(path, &[key_step("terrain")], position, &insertion)
    } else {
        let members = root.as_object().unwrap_or_default();
        let position = members
            .iter()
            .position(|member| member.key == "objects")
            .unwrap_or(members.len());
        let text = Node::Object(vec![(grid.to_owned(), node)]).layout(&splice::item_indentation(
            document.source(),
            root,
            position,
        ));
        let insertion = Insertion {
            key: Some(("terrain", "\"terrain\"")),
            value: &text,
            layout: None,
            colon: None,
        };
        workspace.insert(path, &[], position, &insertion)
    }
}

/// Turns the grid's `fill` member into `rows`, keeping every other byte.
fn fill_to_rows(
    workspace: &mut Workspace<'_>,
    path: &str,
    grid_pointer: &[Step],
    rows: &Node,
) -> Outcome<()> {
    let document = workspace.document(path)?;
    let grid = splice::find(document.root(), grid_pointer)
        .ok_or_else(|| Failure::conflict(format!("{path}: the grid is missing")))?;
    let members = grid.as_object().unwrap_or_default();
    let position = members
        .iter()
        .position(|member| member.key == "fill")
        .ok_or_else(|| Failure::conflict(format!("{path}: the grid has no fill")))?;
    let fill = &members[position];
    let text = rows.layout(&splice::item_indentation(document.source(), grid, position));
    let edits = vec![
        (fill.key_span, "\"rows\"".to_owned()),
        (fill.value.span(), text),
    ];
    let inverse = vec![Primitive::Set {
        pointer: grid_pointer.to_vec(),
        value: document.text(grid.span()).to_owned(),
    }];
    workspace.replace_spans(path, edits, inverse)
}

/// The spans and texts of the grid's rows, by row.
fn row_spans(
    workspace: &mut Workspace<'_>,
    path: &str,
    grid_pointer: &[Step],
) -> Outcome<Vec<(Span, String)>> {
    let document = workspace.document(path)?;
    let rows = splice::find(document.root(), grid_pointer)
        .and_then(|grid| grid.get("rows"))
        .and_then(Value::as_array)
        .ok_or_else(|| Failure::conflict(format!("{path}: the grid has no rows")))?;
    Ok(rows
        .iter()
        .map(|row| (row.span(), document.text(row.span()).to_owned()))
        .collect())
}

fn edit_characters(
    workspace: &mut Workspace<'_>,
    path: &str,
    grid: &str,
    before: Option<&Characters>,
    mut after: Characters,
    overflow: bool,
) -> Outcome<()> {
    let grid_pointer = [key_step("terrain"), key_step(grid)];
    let full = || Failure::conflict(format!("{path}: the {grid} palette has no free key"));
    let Some(before) = before else {
        if overflow {
            after = after.widened().ok_or_else(full)?;
        }
        return insert_grid(workspace, path, grid, after.node());
    };
    if after.cells.iter().all(|cell| *cell == 0) {
        return workspace.remove(path, &grid_pointer, None);
    }
    if overflow {
        let document = workspace.document(path)?;
        let value = splice::find(document.root(), &grid_pointer[..1])
            .ok_or_else(|| Failure::conflict(format!("{path}: terrain is missing")))?;
        let position = value
            .as_object()
            .and_then(|members| members.iter().position(|member| member.key == grid))
            .unwrap_or(0);
        let text = after
            .widened()
            .ok_or_else(full)?
            .node()
            .layout(&splice::item_indentation(
                document.source(),
                value,
                position,
            ));
        return workspace.set(path, &grid_pointer, &text);
    }
    let palette_pointer = [key_step("terrain"), key_step(grid), key_step("palette")];
    // Entries the edit leaves unused go first, so that a new entry may take
    // a key one of them had.
    let used = after.used();
    for cell in before.used() {
        if !used.contains(&cell) {
            let (key, _) = &before.palette[cell - 1];
            let mut pointer = palette_pointer.to_vec();
            pointer.push(key_step(key));
            workspace.remove(path, &pointer, None)?;
        }
    }
    for position in before.palette.len()..after.palette.len() {
        let (key, value) = &after.palette[position];
        let key_text = json_string(key);
        let value_text = json_string(value);
        let insertion = Insertion {
            key: Some((key, &key_text)),
            value: &value_text,
            layout: None,
            colon: None,
        };
        let document = workspace.document(path)?;
        let members = splice::find(document.root(), &palette_pointer)
            .and_then(Value::as_object)
            .map_or(0, <[_]>::len);
        workspace.insert(path, &palette_pointer, members, &insertion)?;
    }
    let same = after.cells.iter().all(|cell| *cell == after.cells[0]);
    if before.fill && same {
        let key = json_string(&after.key(after.cells[0]));
        if after.key(after.cells[0]) != before.key(before.cells[0]) {
            let mut pointer = grid_pointer.to_vec();
            pointer.push(key_step("fill"));
            workspace.set(path, &pointer, &key)?;
        }
    } else if before.fill {
        fill_to_rows(workspace, path, &grid_pointer, &after.rows_node())?;
    } else {
        let rows = row_spans(workspace, path, &grid_pointer)?;
        let mut edits = Vec::new();
        let mut inverse = Vec::new();
        for (row, (span, old)) in rows.into_iter().enumerate() {
            let text = json_string(&after.row(row));
            if after.row(row) != before.row(row) {
                edits.push((span, text));
                inverse.push(Primitive::Set {
                    pointer: vec![
                        key_step("terrain"),
                        key_step(grid),
                        key_step("rows"),
                        Step::Index(row),
                    ],
                    value: old,
                });
            }
        }
        workspace.replace_spans(path, edits, inverse)?;
    }
    Ok(())
}

fn edit_heights(
    workspace: &mut Workspace<'_>,
    path: &str,
    before: Option<&Heights>,
    after: &Heights,
) -> Outcome<()> {
    let grid_pointer = [key_step("terrain"), key_step("height")];
    let Some(before) = before else {
        return insert_grid(workspace, path, "height", after.node());
    };
    if after.cells.iter().all(|cell| *cell == 0) {
        return workspace.remove(path, &grid_pointer, None);
    }
    let same = after.cells.iter().all(|cell| *cell == after.cells[0]);
    if before.fill && same {
        if after.cells[0] != before.cells[0] {
            let mut pointer = grid_pointer.to_vec();
            pointer.push(key_step("fill"));
            workspace.set(path, &pointer, &after.cells[0].to_string())?;
        }
        return Ok(());
    }
    if before.fill {
        return fill_to_rows(workspace, path, &grid_pointer, &after.rows_node());
    }
    let document = workspace.document(path)?;
    let rows = splice::find(document.root(), &grid_pointer)
        .and_then(|grid| grid.get("rows"))
        .and_then(Value::as_array)
        .ok_or_else(|| Failure::conflict(format!("{path}: the height grid has no rows")))?;
    // Each changed height is replaced; the inverse restores each changed
    // row whole, one operation a row.
    let mut edits = Vec::new();
    let mut inverse = Vec::new();
    for row in 0..SIDE {
        let range = row * SIDE..(row + 1) * SIDE;
        if before.cells[range.clone()] == after.cells[range.clone()] {
            continue;
        }
        let cells = rows
            .get(row)
            .and_then(Value::as_array)
            .filter(|cells| cells.len() == SIDE)
            .ok_or_else(|| Failure::conflict(format!("{path}: the height grid is malformed")))?;
        for (column, cell) in cells.iter().enumerate() {
            let new = after.cells[row * SIDE + column];
            if before.cells[row * SIDE + column] != new {
                edits.push((cell.span(), new.to_string()));
            }
        }
        inverse.push(Primitive::Set {
            pointer: vec![
                key_step("terrain"),
                key_step("height"),
                key_step("rows"),
                Step::Index(row),
            ],
            value: document.text(rows[row].span()).to_owned(),
        });
    }
    workspace.replace_spans(path, edits, inverse)
}

fn show(value: &CellValue) -> String {
    match value {
        CellValue::Empty => "empty".to_owned(),
        CellValue::Id(id) => json_string(id),
        CellValue::Height(height) => height.to_string(),
    }
}

/// A chunk's site and its cells, by index within the chunk.
type ChunkCells<'c> = (Site, Vec<(usize, &'c Cell)>);

/// Failures of a terrain operation: at a cell's span, or at the operation.
pub(crate) type TerrainFailures = Vec<(Option<Span>, Failure)>;

/// Sets terrain cells of one grid on one layer. Cells may span chunks; a
/// chunk is created when a cell gains content and its file is removed
/// later if it ends up empty.
pub(crate) fn set_cells(
    workspace: &mut Workspace<'_>,
    plane: &str,
    layer: i32,
    grid: &str,
    cells: &[Cell],
    chunks: &mut BTreeSet<String>,
) -> Result<(), TerrainFailures> {
    let mut by_chunk: BTreeMap<String, ChunkCells<'_>> = BTreeMap::new();
    for cell in cells {
        let site = site(plane, layer, cell.at);
        let index = usize::from(site.local[1]) * SIDE + usize::from(site.local[0]);
        if by_chunk.len() == limits::FILES && !by_chunk.contains_key(&site.path) {
            return Err(vec![(
                None,
                Failure::limit(format!(
                    "the operation touches more than {} chunks",
                    limits::FILES
                )),
            )]);
        }
        by_chunk
            .entry(site.path.clone())
            .or_insert_with(|| (site, Vec::new()))
            .1
            .push((index, cell));
    }
    let mut failures = Vec::new();
    for (path, (site, cells)) in by_chunk {
        if workspace.exhausted() || failures.len() >= limits::DIAGNOSTICS {
            break;
        }
        chunks.insert(path.clone());
        if let Err(mut found) = set_chunk_cells(workspace, plane, layer, grid, &path, &site, &cells)
        {
            failures.append(&mut found);
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures)
    }
}

fn set_chunk_cells(
    workspace: &mut Workspace<'_>,
    plane: &str,
    layer: i32,
    grid: &str,
    path: &str,
    site: &Site,
    cells: &[(usize, &Cell)],
) -> Result<(), TerrainFailures> {
    let fail = |failure| vec![(None, failure)];
    let exists = workspace.exists(path).map_err(fail)?;
    let current = if exists {
        let document = workspace.document(path).map_err(fail)?;
        document
            .root()
            .get("terrain")
            .and_then(|terrain| terrain.get(grid))
            .cloned()
    } else {
        None
    };
    let malformed = || {
        fail(Failure::conflict(format!(
            "{path}: the {grid} grid is not a valid grid and cannot be edited"
        )))
    };
    let mut problems = Vec::new();
    if grid == "height" {
        let before = match &current {
            Some(value) => Some(read_heights(value).ok_or_else(malformed)?),
            None => None,
        };
        let mut after = before.clone().unwrap_or_else(Heights::empty);
        for (index, cell) in cells {
            if problems.len() >= limits::DIAGNOSTICS {
                break;
            }
            let found = CellValue::Height(after.cells[*index]);
            if let Some(expect) = &cell.expect
                && *expect != found
            {
                problems.push((Some(cell.span), mismatch(path, grid, cell, &found, expect)));
            }
            if let CellValue::Height(height) = cell.value {
                after.cells[*index] = height;
            }
        }
        if !problems.is_empty() {
            return Err(problems);
        }
        if before.is_none() && after.cells.iter().all(|cell| *cell == 0) {
            return Ok(());
        }
        ensure_chunk(workspace, plane, layer, site).map_err(fail)?;
        edit_heights(workspace, path, before.as_ref(), &after).map_err(fail)
    } else {
        let before = match &current {
            Some(value) => Some(read_characters(value).ok_or_else(malformed)?),
            None => None,
        };
        let (after, overflow) = characters_after(before.as_ref(), cells, path, grid)?;
        if before.is_none() && after.cells.iter().all(|cell| *cell == 0) {
            return Ok(());
        }
        ensure_chunk(workspace, plane, layer, site).map_err(fail)?;
        edit_characters(workspace, path, grid, before.as_ref(), after, overflow).map_err(fail)
    }
}

/// A character grid after setting `cells`, and whether it needs wider
/// keys: values with an entry take it, entries the edit leaves unused free
/// their keys, and new values get free keys.
fn characters_after(
    before: Option<&Characters>,
    cells: &[(usize, &Cell)],
    path: &str,
    grid: &str,
) -> Result<(Characters, bool), TerrainFailures> {
    let mut problems = Vec::new();
    let mut after = before.cloned().unwrap_or_else(Characters::empty);
    // First the expectations, and the cells whose value has an entry;
    // values without one wait until the keys the edit frees are known.
    let mut pending = Vec::new();
    for (index, cell) in cells {
        if problems.len() >= limits::DIAGNOSTICS {
            break;
        }
        let found = after.value(after.cells[*index]);
        if let Some(expect) = &cell.expect
            && *expect != found
        {
            problems.push((Some(cell.span), mismatch(path, grid, cell, &found, expect)));
        }
        after.cells[*index] = match &cell.value {
            CellValue::Id(id) => {
                if let Some(cell) = after.values.get(id) {
                    *cell
                } else {
                    pending.push((*index, id.clone()));
                    0
                }
            }
            CellValue::Empty | CellValue::Height(_) => 0,
        };
    }
    if !problems.is_empty() {
        return Err(problems);
    }
    // An entry the edit leaves unused is removed, so its key is free.
    if let Some(before) = &before {
        let used = after.used();
        for cell in before.used() {
            if !used.contains(&cell) {
                after.keys.remove(&before.palette[cell - 1].0);
            }
        }
    }
    let mut overflow = false;
    for (index, id) in pending {
        let value = CellValue::Id(id.clone());
        if let Some(cell) = after.cell_for(&value) {
            after.cells[index] = cell;
        } else {
            overflow = true;
            // Keep the value for widening: add it with a placeholder
            // key, which widening replaces.
            after.cells[index] = after.push(String::new(), id);
        }
    }
    Ok((after, overflow))
}

fn mismatch(path: &str, grid: &str, cell: &Cell, found: &CellValue, expect: &CellValue) -> Failure {
    Failure::conflict(format!(
        "{path}: {grid} cell ({}, {}) is {}, expected {}",
        cell.at[0],
        cell.at[1],
        show(found),
        show(expect)
    ))
}
