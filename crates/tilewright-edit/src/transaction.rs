// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Transaction documents (`docs/spec/TRANSACTIONS.md`): parsing and shape
//! checks. Every problem is reported; a transaction is returned only when
//! there are none.

use crate::{Code, diagnostic, limits};
use std::collections::{BTreeMap, BTreeSet};
use tilewright_jsonc::{Document, Limits, MAX_SAFE_INTEGER, Span, Value};
use tilewright_source::{CHUNK_SIZE, Diagnostic, is_identifier, is_namespace};

/// The `format` of a transaction document.
pub const FORMAT: &str = "tilewright.transaction/1";

/// SHA-256 digests of files by root-relative path; `None` means the file is
/// absent.
pub type Digests = BTreeMap<String, Option<[u8; 32]>>;

/// A step of a pointer as written.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Segment {
    Key(String),
    Index(usize),
    /// The one array item that is an object with this `"id"`.
    Id(String),
}

/// A tile of a plane, in plane coordinates.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Tile {
    pub plane: String,
    pub layer: i32,
    pub at: [i64; 2],
}

/// The object at position `index` among the objects of a tile.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ObjectAddress {
    pub tile: Tile,
    pub index: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Target {
    Path(String),
    Object(ObjectAddress),
}

/// A terrain cell value: an empty cell, a palette value or a height.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CellValue {
    Empty,
    Id(String),
    Height(i32),
}

#[derive(Clone, Debug)]
pub(crate) struct Cell {
    pub span: Span,
    pub at: [i64; 2],
    pub value: CellValue,
    pub expect: Option<CellValue>,
}

#[derive(Clone, Debug)]
pub(crate) enum Content {
    /// A value of the transaction, written in canonical layout.
    Document(Span),
    /// Exact file text.
    Text(String),
}

/// What a `test` checks.
#[derive(Clone, Debug)]
pub(crate) enum Expected {
    Value(Span),
    Absent,
}

/// Values are spans of the transaction document, used exactly as written.
#[derive(Clone, Debug)]
pub(crate) enum Action {
    Set {
        target: Target,
        pointer: Vec<Segment>,
        /// The last pointer step's text, when it is a key: a member the set
        /// adds is written with the key spelled as there.
        key: Option<Span>,
        value: Span,
        expect: Option<Span>,
    },
    Insert {
        target: Target,
        /// The container; the item goes at `index` of it.
        pointer: Vec<Segment>,
        index: usize,
        key: Option<Span>,
        value: Span,
        layout: Option<(String, String)>,
        colon: Option<String>,
    },
    Remove {
        target: Target,
        pointer: Vec<Segment>,
        expect: Option<Span>,
        separator: Option<String>,
    },
    Test {
        target: Target,
        pointer: Vec<Segment>,
        expected: Expected,
    },
    ObjectInsert {
        tile: Tile,
        index: Option<usize>,
        object: Span,
    },
    ObjectRemove {
        address: ObjectAddress,
        expect: Span,
    },
    ObjectMove {
        address: ObjectAddress,
        expect: Span,
        to: Tile,
        index: Option<usize>,
    },
    TerrainSet {
        plane: String,
        layer: i32,
        grid: String,
        cells: Vec<Cell>,
    },
    Create {
        path: String,
        content: Content,
    },
    Delete {
        path: String,
    },
    Rename {
        path: String,
        to: String,
    },
}

impl Action {
    /// The one file a document-level action touches; `None` for actions
    /// that address tiles.
    pub(crate) fn document_path(&self) -> Option<&str> {
        match self {
            Self::Set { target, .. }
            | Self::Insert { target, .. }
            | Self::Remove { target, .. }
            | Self::Test { target, .. } => match target {
                Target::Path(path) => Some(path),
                Target::Object(_) => None,
            },
            Self::Create { path, .. } | Self::Delete { path } => Some(path),
            Self::ObjectInsert { .. }
            | Self::ObjectRemove { .. }
            | Self::ObjectMove { .. }
            | Self::TerrainSet { .. }
            | Self::Rename { .. } => None,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Operation {
    pub span: Span,
    pub action: Action,
}

/// A parsed, shape-checked transaction document.
#[derive(Clone, Debug)]
pub struct Transaction {
    pub(crate) name: String,
    pub(crate) document: Document,
    pub(crate) base: Option<(Span, Digests)>,
    pub(crate) result: Option<Digests>,
    pub(crate) operations: Vec<Operation>,
}

impl Transaction {
    /// Parses and checks a transaction document. `name` labels its
    /// diagnostics, such as the path it was read from.
    ///
    /// # Errors
    /// Returns every problem with the document, each at its byte span.
    pub fn parse(name: &str, text: impl Into<String>) -> Result<Self, Vec<Diagnostic>> {
        let text = text.into();
        let document = Document::parse(text, &Limits::default()).map_err(|error| {
            vec![Diagnostic {
                path: name.to_owned(),
                start: error.offset,
                end: error.offset,
                line: error.line,
                column: error.column,
                code: error.code.as_str(),
                message: "invalid JSONC".to_owned(),
            }]
        })?;
        let mut shape = Shape::new(name, &document);
        let parsed = shape.transaction();
        let problems = shape.finish();
        match parsed {
            Some((base, result, operations)) if problems.is_empty() => Ok(Self {
                name: name.to_owned(),
                base,
                result,
                operations,
                document,
            }),
            _ => Err(problems),
        }
    }

    /// The transaction's document text.
    #[must_use]
    pub fn text(&self) -> &str {
        self.document.source()
    }

    /// The name its diagnostics use.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The number of operations.
    #[must_use]
    pub fn len(&self) -> usize {
        self.operations.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.operations.is_empty()
    }

    /// The recorded base digests, if any.
    #[must_use]
    pub fn base(&self) -> Option<&Digests> {
        self.base.as_ref().map(|(_, digests)| digests)
    }

    /// The recorded result digests, if any.
    #[must_use]
    pub fn result(&self) -> Option<&Digests> {
        self.result.as_ref()
    }

    /// Whether every operation names one document by path, so the
    /// operations of different files are independent.
    pub(crate) fn documents_only(&self) -> bool {
        self.operations
            .iter()
            .all(|operation| operation.action.document_path().is_some())
    }

    pub(crate) fn value(&self, span: Span) -> &Value {
        find_span(self.document.root(), span).unwrap_or_else(|| self.document.root())
    }
}

/// The value of `root` at exactly `span`. Items and members are in source
/// order, so each level is a binary search.
fn find_span(root: &Value, span: Span) -> Option<&Value> {
    if root.span() == span {
        return Some(root);
    }
    let child = match root {
        Value::Array { items, .. } => {
            let index = items.partition_point(|item| item.span().end <= span.start);
            items.get(index)
        }
        Value::Object { members, .. } => {
            let index = members.partition_point(|member| member.value.span().end <= span.start);
            members.get(index).map(|member| &member.value)
        }
        _ => None,
    }?;
    (child.span().start <= span.start && span.end <= child.span().end)
        .then(|| find_span(child, span))
        .flatten()
}

/// Whether a transaction may touch `path`: `world.jsonc`, or a `.jsonc`
/// file under `profile/`, `planes/` or `defs/`. Components are ASCII
/// letters, digits, `_`, `.` and `-`, not starting with `.`, so chunk and
/// layer names may be negative: at most 1,024 bytes, 32 components and
/// 128 bytes a component.
pub(crate) fn is_document_path(path: &str) -> bool {
    let components: Vec<&str> = path.split('/').collect();
    path.len() <= 1024
        && components.len() <= 32
        && path.as_bytes().ends_with(b".jsonc")
        && components.iter().all(|component| {
            let bytes = component.as_bytes();
            !bytes.is_empty()
                && bytes.len() <= 128
                && bytes[0] != b'.'
                && bytes
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(byte))
        })
        && (path == "world.jsonc"
            || (components.len() > 1 && matches!(components[0], "profile" | "planes" | "defs")))
}

/// The tile range of format 1: chunk coordinates keep every tile within
/// the format's integer range (FORMAT.md section 2).
pub(crate) fn chunk_of(coordinate: i64) -> Option<(i64, u16)> {
    let size = i64::from(CHUNK_SIZE);
    let chunk = coordinate.div_euclid(size);
    let range = -(MAX_SAFE_INTEGER / size)..=(MAX_SAFE_INTEGER - (size - 1)) / size;
    range.contains(&chunk).then(|| {
        (
            chunk,
            u16::try_from(coordinate.rem_euclid(size)).unwrap_or(0),
        )
    })
}

/// Checks the shape of a transaction document.
struct Shape<'a> {
    name: &'a str,
    document: &'a Document,
    lines: diagnostic::Lines<'a>,
    problems: Vec<Diagnostic>,
    /// Whether a problem was dropped at the limit, whose last place is
    /// kept for a note that there were more.
    dropped: bool,
    cells: usize,
}

type Parsed = (Option<(Span, Digests)>, Option<Digests>, Vec<Operation>);

impl<'a> Shape<'a> {
    fn new(name: &'a str, document: &'a Document) -> Self {
        Self {
            name,
            document,
            lines: diagnostic::Lines::new(document.source()),
            problems: Vec::new(),
            dropped: false,
            cells: 0,
        }
    }

    fn finish(mut self) -> Vec<Diagnostic> {
        self.problems.sort();
        self.problems.dedup();
        if self.dropped {
            self.problems.push(diagnostic::file(
                self.name,
                Code::Limit.as_str(),
                format!(
                    "more than {} problems; the rest are not reported",
                    limits::DIAGNOSTICS - 1
                ),
            ));
        }
        self.problems
    }

    fn report(&mut self, span: Span, code: Code, message: impl Into<String>) {
        if self.problems.len() >= limits::DIAGNOSTICS - 1 {
            self.dropped = true;
        } else {
            self.problems.push(diagnostic::at(
                self.name,
                &self.lines,
                span.start,
                span.end,
                code.as_str(),
                message,
            ));
        }
    }

    fn shape(&mut self, span: Span, message: impl Into<String>) {
        self.report(span, Code::Shape, message);
    }

    fn object(&mut self, value: &'a Value, allowed: &[&str]) -> Option<&'a Value> {
        let Some(members) = value.as_object() else {
            self.shape(
                value.span(),
                format!("expected an object, found {}", value.kind()),
            );
            return None;
        };
        // An unknown key is reported, and the rest of the object is still
        // checked.
        for member in members {
            if !allowed.contains(&member.key.as_str()) {
                self.shape(
                    member.key_span,
                    format!("unknown key {}", diagnostic::quoted(&member.key)),
                );
            }
        }
        Some(value)
    }

    fn required(&mut self, object: &'a Value, key: &str) -> Option<&'a Value> {
        let value = object.get(key);
        if value.is_none() {
            self.shape(object.span(), format!("missing key \"{key}\""));
        }
        value
    }

    fn string(&mut self, value: &'a Value) -> Option<&'a str> {
        let text = value.as_str();
        if text.is_none() {
            self.shape(
                value.span(),
                format!("expected a string, found {}", value.kind()),
            );
        }
        text
    }

    fn text(&mut self, value: &'a Value, maximum: usize) -> Option<String> {
        let text = self.string(value)?;
        if text.chars().count() > maximum {
            self.shape(value.span(), format!("longer than {maximum} characters"));
            return None;
        }
        Some(text.to_owned())
    }

    fn identifier(&mut self, value: &'a Value) -> Option<String> {
        let text = self.string(value)?;
        if is_identifier(text) {
            Some(text.to_owned())
        } else {
            self.shape(
                value.span(),
                format!("{} is not an identifier", diagnostic::quoted(text)),
            );
            None
        }
    }

    fn integer(&mut self, value: &'a Value, minimum: i64, maximum: i64) -> Option<i64> {
        match value.as_i64() {
            Some(number) if (minimum..=maximum).contains(&number) => Some(number),
            Some(number) => {
                self.shape(
                    value.span(),
                    format!("{number} is outside {minimum}..={maximum}"),
                );
                None
            }
            None => {
                self.shape(
                    value.span(),
                    format!("expected an integer, found {}", value.kind()),
                );
                None
            }
        }
    }

    fn index(&mut self, value: &'a Value) -> Option<usize> {
        let maximum = i64::try_from(limits::INDEX).unwrap_or(i64::MAX);
        usize::try_from(self.integer(value, 0, maximum)?).ok()
    }

    fn layer(&mut self, value: &'a Value) -> Option<i32> {
        i32::try_from(self.integer(value, i64::from(i32::MIN), i64::from(i32::MAX))?).ok()
    }

    /// A tile position in plane coordinates, within the format's chunk
    /// range.
    fn at(&mut self, value: &'a Value) -> Option<[i64; 2]> {
        let Some([x, y]) = value.as_array() else {
            self.shape(value.span(), "expected [x, y]");
            return None;
        };
        let x = self.integer(x, -MAX_SAFE_INTEGER, MAX_SAFE_INTEGER);
        let y = self.integer(y, -MAX_SAFE_INTEGER, MAX_SAFE_INTEGER);
        let (x, y) = (x?, y?);
        if chunk_of(x).is_none() || chunk_of(y).is_none() {
            self.shape(value.span(), "outside the tiles a plane can hold");
            return None;
        }
        Some([x, y])
    }

    fn path(&mut self, value: &'a Value) -> Option<String> {
        let text = self.string(value)?;
        if is_document_path(text) {
            Some(text.to_owned())
        } else {
            self.report(
                value.span(),
                Code::Path,
                format!(
                    "{} is not a source document path: world.jsonc, or a .jsonc file under profile/, planes/ or defs/",
                    diagnostic::quoted(text)
                ),
            );
            None
        }
    }

    /// A separator, colon or layout string; its content is checked where it
    /// is used. Like file text, it may be given in pieces.
    fn separator(&mut self, value: &'a Value) -> Option<String> {
        self.text_pieces(value)
    }

    /// Exact text: a string, or an array of strings joined, so that text
    /// longer than a string or a line may be given in pieces.
    fn text_pieces(&mut self, value: &'a Value) -> Option<String> {
        if let Some(text) = value.as_str() {
            return Some(text.to_owned());
        }
        let Some(pieces) = value.as_array() else {
            self.shape(value.span(), "expected a string or an array of strings");
            return None;
        };
        // Every piece is checked; the text is whole only if each is a
        // string.
        let mut text = String::new();
        let mut whole = true;
        for piece in pieces {
            match self.string(piece) {
                Some(piece) if whole => text.push_str(piece),
                Some(_) => {}
                None => whole = false,
            }
        }
        whole.then_some(text)
    }

    /// A digest list: `[{"path", "sha256"}, ...]`, each path once, with
    /// `null` for a file that is absent.
    fn digests(&mut self, value: &'a Value) -> Option<Digests> {
        let Some(items) = value.as_array() else {
            self.shape(
                value.span(),
                "expected an array of {\"path\", \"sha256\"} entries",
            );
            return None;
        };
        if items.len() > limits::FILES {
            self.report(
                value.span(),
                Code::Limit,
                format!("more than {} files", limits::FILES),
            );
            return None;
        }
        let mut digests = BTreeMap::new();
        let mut ok = true;
        for item in items {
            let Some(entry) = self.object(item, &["path", "sha256"]) else {
                ok = false;
                continue;
            };
            let path = self
                .required(entry, "path")
                .and_then(|path| self.path(path));
            let digest = match self.required(entry, "sha256") {
                Some(Value::Null { .. }) => Some(None),
                Some(Value::String { value: text, span }) => {
                    let digest = parse_digest(text);
                    if digest.is_none() {
                        self.shape(*span, "a digest is 64 lowercase hexadecimal digits");
                    }
                    digest.map(Some)
                }
                Some(other) => {
                    self.shape(other.span(), "expected a digest or null");
                    None
                }
                None => None,
            };
            let (Some(path), Some(digest)) = (path, digest) else {
                ok = false;
                continue;
            };
            if digests.insert(path, digest).is_some() {
                self.shape(entry.span(), "this path is listed twice");
                ok = false;
            }
        }
        ok.then_some(digests)
    }

    fn transaction(&mut self) -> Option<Parsed> {
        let root = self.document.root();
        let root = self.object(
            root,
            &[
                "format",
                "title",
                "actor",
                "base",
                "result",
                "operations",
                "custom",
            ],
        )?;
        let mut ok = true;
        match self.required(root, "format") {
            Some(value) if value.as_str() == Some(FORMAT) => {}
            Some(value) => {
                self.shape(value.span(), format!("format must be \"{FORMAT}\""));
                ok = false;
            }
            None => ok = false,
        }
        for key in ["title", "actor"] {
            if let Some(value) = root.get(key) {
                ok &= self.text(value, 256).is_some();
            }
        }
        if let Some(custom) = root.get("custom") {
            ok &= self.custom(custom);
        }
        // A malformed digest list is reported, and the rest still checked.
        let base = match root.get("base") {
            Some(value) => {
                let digests = self.digests(value);
                ok &= digests.is_some();
                digests.map(|digests| (value.span(), digests))
            }
            None => None,
        };
        let result = match root.get("result") {
            Some(value) => {
                let digests = self.digests(value);
                ok &= digests.is_some();
                digests
            }
            None => None,
        };
        let items = self.required(root, "operations")?;
        let Some(items) = items.as_array() else {
            self.shape(items.span(), "operations must be an array");
            return None;
        };
        if items.len() > limits::OPERATIONS {
            self.report(
                root.span(),
                Code::Limit,
                format!("more than {} operations", limits::OPERATIONS),
            );
            return None;
        }
        let mut operations = Vec::with_capacity(items.len());
        for item in items {
            match self.operation(item) {
                Some(action) => operations.push(Operation {
                    span: item.span(),
                    action,
                }),
                None => ok = false,
            }
        }
        ok.then_some((base, result, operations))
    }

    fn custom(&mut self, value: &'a Value) -> bool {
        let Some(members) = value.as_object() else {
            self.shape(value.span(), "custom must be an object");
            return false;
        };
        let mut ok = true;
        for member in members {
            if !is_namespace(&member.key) {
                self.shape(
                    member.key_span,
                    format!(
                        "custom key {} must be a domain-qualified namespace",
                        diagnostic::quoted(&member.key)
                    ),
                );
                ok = false;
            }
        }
        ok
    }

    fn operation(&mut self, item: &'a Value) -> Option<Action> {
        if item.as_object().is_none() {
            self.shape(
                item.span(),
                format!("an operation is an object, found {}", item.kind()),
            );
            return None;
        }
        let kind = self.required(item, "op").and_then(|kind| self.string(kind));
        // Without a known operation, its keys are still checked against
        // every operation's.
        let Some(kind) = kind else {
            self.object(item, ANY_OPERATION_KEYS);
            return None;
        };
        let allowed: &[&str] = match kind {
            "set" => &["op", "path", "object", "pointer", "value", "expect"],
            "insert" => &[
                "op", "path", "object", "pointer", "key", "value", "before", "after", "colon",
            ],
            "remove" => &["op", "path", "object", "pointer", "expect", "separator"],
            "test" => &["op", "path", "object", "pointer", "value", "absent"],
            "object.insert" => &["op", "plane", "layer", "at", "index", "object"],
            "object.remove" => &["op", "plane", "layer", "at", "index", "expect"],
            "object.move" => &["op", "plane", "layer", "at", "index", "expect", "to"],
            "terrain.set" => &["op", "plane", "layer", "grid", "cells"],
            "document.create" => &["op", "path", "document", "text"],
            "document.delete" => &["op", "path"],
            "document.rename" => &["op", "path", "to"],
            _ => {
                self.shape(
                    item.get("op").map_or(item.span(), Value::span),
                    format!("unknown operation {}", diagnostic::quoted(kind)),
                );
                self.object(item, ANY_OPERATION_KEYS);
                return None;
            }
        };
        let item = self.object(item, allowed)?;
        match kind {
            "set" => self.set(item),
            "insert" => self.insert(item),
            "remove" => self.remove(item),
            "test" => self.test(item),
            "object.insert" => self.object_insert(item),
            "object.remove" => {
                let address = self.address(item);
                let expect = self.object_expect(item);
                Some(Action::ObjectRemove {
                    address: address?,
                    expect: expect?,
                })
            }
            "object.move" => self.object_move(item),
            "terrain.set" => self.terrain(item),
            "document.create" => self.create(item),
            "document.delete" => {
                let path = self.required(item, "path")?;
                Some(Action::Delete {
                    path: self.path(path)?,
                })
            }
            "document.rename" => {
                let path = self.required(item, "path").and_then(|path| self.path(path));
                let to = self.required(item, "to").and_then(|to| self.path(to));
                let (path, to) = (path?, to?);
                if path == to {
                    self.shape(item.span(), "a document cannot be renamed to itself");
                    return None;
                }
                Some(Action::Rename { path, to })
            }
            _ => None,
        }
    }

    fn create(&mut self, item: &'a Value) -> Option<Action> {
        let path = self.required(item, "path").and_then(|path| self.path(path));
        let content = match (item.get("document"), item.get("text")) {
            (Some(value), None) => {
                if matches!(value, Value::Object { .. }) {
                    Some(Content::Document(value.span()))
                } else {
                    self.shape(value.span(), "document must be an object");
                    None
                }
            }
            (None, Some(value)) => self.text_pieces(value).map(Content::Text),
            _ => {
                self.shape(item.span(), "give exactly one of document or text");
                None
            }
        };
        Some(Action::Create {
            path: path?,
            content: content?,
        })
    }

    fn object_insert(&mut self, item: &'a Value) -> Option<Action> {
        let tile = self.tile(item);
        let index = match item.get("index") {
            Some(value) => self.index(value).map(Some),
            None => Some(None),
        };
        let object = self.required(item, "object").and_then(|object| {
            if !matches!(object, Value::Object { .. }) {
                self.shape(object.span(), "object must be an object");
                return None;
            }
            if let Some(at) = object.member("at") {
                self.shape(
                    at.key_span,
                    "the object's tile comes from the operation; leave out \"at\"",
                );
                return None;
            }
            Some(object.span())
        });
        Some(Action::ObjectInsert {
            tile: tile?,
            index: index?,
            object: object?,
        })
    }

    fn target(&mut self, item: &'a Value) -> Option<Target> {
        match (item.get("path"), item.get("object")) {
            (Some(path), None) => Some(Target::Path(self.path(path)?)),
            (None, Some(object)) => {
                let object = self.object(object, &["plane", "layer", "at", "index"])?;
                Some(Target::Object(self.address(object)?))
            }
            _ => {
                self.shape(item.span(), "give exactly one of path or object");
                None
            }
        }
    }

    fn tile(&mut self, item: &'a Value) -> Option<Tile> {
        let plane = self
            .required(item, "plane")
            .and_then(|value| self.identifier(value));
        let layer = self
            .required(item, "layer")
            .and_then(|value| self.layer(value));
        let at = self.required(item, "at").and_then(|value| self.at(value));
        Some(Tile {
            plane: plane?,
            layer: layer?,
            at: at?,
        })
    }

    fn address(&mut self, item: &'a Value) -> Option<ObjectAddress> {
        let tile = self.tile(item);
        let index = self
            .required(item, "index")
            .and_then(|value| self.index(value));
        Some(ObjectAddress {
            tile: tile?,
            index: index?,
        })
    }

    /// The `expect` of an object operation: an object that names at least
    /// `proto` and not `at`.
    fn object_expect(&mut self, item: &'a Value) -> Option<Span> {
        let expect = self.required(item, "expect")?;
        let mut usable = true;
        if !matches!(expect, Value::Object { .. }) || expect.get("proto").is_none() {
            self.shape(
                expect.span(),
                "expect must be an object that names at least the object's proto",
            );
            usable = false;
        }
        if let Some(at) = expect.member("at") {
            self.shape(
                at.key_span,
                "the tile comes from the address; leave out \"at\"",
            );
            usable = false;
        }
        usable.then(|| expect.span())
    }

    /// A pointer: keys, indexes and `{"id": ...}` selectors.
    fn pointer(&mut self, value: &'a Value) -> Option<Vec<Segment>> {
        let Some(items) = value.as_array() else {
            self.shape(
                value.span(),
                "a pointer is an array of keys, indexes and {\"id\": ...}",
            );
            return None;
        };
        if items.len() > limits::POINTER {
            self.report(
                value.span(),
                Code::Limit,
                format!("a pointer has at most {} steps", limits::POINTER),
            );
            return None;
        }
        // Every step is checked; the pointer is whole only if each is
        // valid.
        let mut segments = Vec::with_capacity(items.len());
        let mut whole = true;
        for item in items {
            let segment = match item {
                Value::String { value, .. } => Some(Segment::Key(value.clone())),
                Value::Integer { .. } => self.index(item).map(Segment::Index),
                Value::Object { .. } => self
                    .object(item, &["id"])
                    .and_then(|selector| self.required(selector, "id"))
                    .and_then(|id| self.identifier(id))
                    .map(Segment::Id),
                other => {
                    self.shape(
                        other.span(),
                        "a pointer step is a key, an index or {\"id\": ...}",
                    );
                    None
                }
            };
            match segment {
                Some(segment) => segments.push(segment),
                None => whole = false,
            }
        }
        whole.then_some(segments)
    }

    /// The pointer of an operation, checked against its target: an object
    /// target's pointer is not empty and does not start at `at`. A target
    /// that failed its own checks does not stop the pointer's.
    fn target_pointer(&mut self, item: &'a Value, target: Option<&Target>) -> Option<Vec<Segment>> {
        let value = self.required(item, "pointer")?;
        let pointer = self.pointer(value);
        // Checked on the steps as written, so that a problem with a later
        // step does not hide it.
        if let Some(Target::Object(_)) = target {
            match value.as_array().map(<[Value]>::first) {
                Some(None) => {
                    self.shape(value.span(), "an object's pointer names a part of it");
                    return None;
                }
                Some(Some(first)) if first.as_str() == Some("at") => {
                    self.shape(value.span(), "move the object to change its tile");
                    return None;
                }
                _ => {}
            }
        }
        pointer
    }

    // Each of these checks every field before giving up on the operation,
    // so that all of its problems are reported.

    fn set(&mut self, item: &'a Value) -> Option<Action> {
        let target = self.target(item);
        let pointer = self.target_pointer(item, target.as_ref());
        let key = item
            .get("pointer")
            .and_then(Value::as_array)
            .and_then(<[Value]>::last)
            .filter(|step| step.as_str().is_some())
            .map(Value::span);
        let value = self.required(item, "value").map(Value::span);
        let expect = item.get("expect").map(Value::span);
        Some(Action::Set {
            target: target?,
            pointer: pointer?,
            key,
            value: value?,
            expect,
        })
    }

    fn insert(&mut self, item: &'a Value) -> Option<Action> {
        let target = self.target(item);
        let pointer = self
            .target_pointer(item, target.as_ref())
            .and_then(|mut pointer| {
                let span = item.get("pointer").map_or(item.span(), Value::span);
                let Some(Segment::Index(index)) = pointer.pop() else {
                    self.shape(span, "an insert's pointer ends with an index");
                    return None;
                };
                if matches!(target, Some(Target::Object(_))) && pointer.is_empty() {
                    self.shape(span, "an object's pointer names a part of it");
                    return None;
                }
                Some((pointer, index))
            });
        let key = match item.get("key") {
            Some(value) => self.string(value).map(|_| Some(value.span())),
            None => Some(None),
        };
        let value = self.required(item, "value").map(Value::span);
        let layout = match (item.get("before"), item.get("after")) {
            (Some(before), Some(after)) => {
                let before = self.separator(before);
                let after = self.separator(after);
                before.zip(after).map(Some)
            }
            (None, None) => Some(None),
            _ => {
                self.shape(item.span(), "give both before and after, or neither");
                None
            }
        };
        let colon = match (item.get("colon"), item.get("key")) {
            (Some(value), Some(_)) => self.separator(value).map(Some),
            (Some(value), None) => {
                self.shape(value.span(), "colon goes with key, for an object member");
                None
            }
            (None, _) => Some(None),
        };
        let (pointer, index) = pointer?;
        Some(Action::Insert {
            target: target?,
            pointer,
            index,
            key: key?,
            value: value?,
            layout: layout?,
            colon: colon?,
        })
    }

    fn remove(&mut self, item: &'a Value) -> Option<Action> {
        let target = self.target(item);
        let pointer = self
            .target_pointer(item, target.as_ref())
            .and_then(|pointer| {
                if pointer.is_empty() {
                    let span = item.get("pointer").map_or(item.span(), Value::span);
                    self.shape(span, "the root value cannot be removed");
                    return None;
                }
                Some(pointer)
            });
        let expect = item.get("expect").map(Value::span);
        let separator = match item.get("separator") {
            Some(value) => self.separator(value).map(Some),
            None => Some(None),
        };
        Some(Action::Remove {
            target: target?,
            pointer: pointer?,
            expect,
            separator: separator?,
        })
    }

    fn test(&mut self, item: &'a Value) -> Option<Action> {
        let target = self.target(item);
        let pointer = self.target_pointer(item, target.as_ref());
        let expected = match (item.get("value"), item.get("absent")) {
            (Some(value), None) => Some(Expected::Value(value.span())),
            (None, Some(Value::Bool { value: true, .. })) => Some(Expected::Absent),
            _ => {
                self.shape(item.span(), "give exactly one of value or \"absent\": true");
                None
            }
        };
        Some(Action::Test {
            target: target?,
            pointer: pointer?,
            expected: expected?,
        })
    }

    fn object_move(&mut self, item: &'a Value) -> Option<Action> {
        let address = self.address(item);
        let expect = self.object_expect(item);
        let to = self
            .required(item, "to")
            .and_then(|to| self.object(to, &["plane", "layer", "at", "index"]));
        // The destination's fields are checked whether or not the source's
        // passed; a missing one is the source's.
        let plane = match to.and_then(|to| to.get("plane")) {
            Some(value) => self.identifier(value),
            None => address.as_ref().map(|address| address.tile.plane.clone()),
        };
        let layer = match to.and_then(|to| to.get("layer")) {
            Some(value) => self.layer(value),
            None => address.as_ref().map(|address| address.tile.layer),
        };
        let at = match to.and_then(|to| to.get("at")) {
            Some(value) => self.at(value),
            None => address.as_ref().map(|address| address.tile.at),
        };
        let index = match to.and_then(|to| to.get("index")) {
            Some(value) => self.index(value).map(Some),
            None => Some(None),
        };
        to?;
        Some(Action::ObjectMove {
            address: address?,
            expect: expect?,
            to: Tile {
                plane: plane?,
                layer: layer?,
                at: at?,
            },
            index: index?,
        })
    }

    fn terrain(&mut self, item: &'a Value) -> Option<Action> {
        let plane = self
            .required(item, "plane")
            .and_then(|value| self.identifier(value));
        let layer = self
            .required(item, "layer")
            .and_then(|value| self.layer(value));
        let grid = self
            .required(item, "grid")
            .and_then(|value| self.identifier(value));
        // The cells are checked even when the address is not.
        let height = grid.as_deref() == Some("height");
        let cells_value = self.required(item, "cells")?;
        let Some(items) = cells_value.as_array() else {
            self.shape(cells_value.span(), "cells must be an array");
            return None;
        };
        self.cells += items.len();
        if self.cells > limits::CELLS {
            self.report(
                cells_value.span(),
                Code::Limit,
                format!("more than {} cells in one transaction", limits::CELLS),
            );
            return None;
        }
        let mut seen = BTreeSet::new();
        let mut cells = Vec::with_capacity(items.len());
        let mut ok = true;
        for cell in items {
            let Some(cell) = self.object(cell, &["at", "value", "expect"]) else {
                ok = false;
                continue;
            };
            let at = self.required(cell, "at").and_then(|value| self.at(value));
            let value = self
                .required(cell, "value")
                .and_then(|value| self.cell_value(value, height));
            let expect = match cell.get("expect") {
                Some(value) => self.cell_value(value, height).map(Some),
                None => Some(None),
            };
            let (Some(at), Some(value), Some(expect)) = (at, value, expect) else {
                ok = false;
                continue;
            };
            if !seen.insert(at) {
                self.shape(cell.span(), "this cell is listed twice");
                ok = false;
                continue;
            }
            cells.push(Cell {
                span: cell.span(),
                at,
                value,
                expect,
            });
        }
        if !ok {
            return None;
        }
        Some(Action::TerrainSet {
            plane: plane?,
            layer: layer?,
            grid: grid?,
            cells,
        })
    }

    fn cell_value(&mut self, value: &'a Value, height: bool) -> Option<CellValue> {
        if height {
            Some(CellValue::Height(
                i32::try_from(self.integer(value, i64::from(i32::MIN), i64::from(i32::MAX))?)
                    .ok()?,
            ))
        } else if matches!(value, Value::Null { .. }) {
            Some(CellValue::Empty)
        } else {
            Some(CellValue::Id(self.identifier(value)?))
        }
    }
}

/// A digest as 64 lowercase hexadecimal digits.
pub(crate) fn parse_digest(text: &str) -> Option<[u8; 32]> {
    let bytes = text.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut digest = [0_u8; 32];
    for (index, pair) in bytes.chunks(2).enumerate() {
        let nibble = |byte: u8| match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        };
        digest[index] = nibble(pair[0])? << 4 | nibble(pair[1])?;
    }
    Some(digest)
}

/// A digest as 64 lowercase hexadecimal digits.
pub(crate) fn hex(digest: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    digest
        .iter()
        .fold(String::with_capacity(64), |mut output, byte| {
            let _ = write!(output, "{byte:02x}");
            output
        })
}

/// The keys of every operation, against which an operation that names no
/// known `op` is checked.
const ANY_OPERATION_KEYS: &[&str] = &[
    "op",
    "path",
    "object",
    "pointer",
    "value",
    "expect",
    "key",
    "before",
    "after",
    "colon",
    "separator",
    "absent",
    "plane",
    "layer",
    "at",
    "index",
    "grid",
    "cells",
    "document",
    "text",
    "to",
];
