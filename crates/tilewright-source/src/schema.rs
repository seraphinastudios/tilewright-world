// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Closed field schemas (`tilewright.schema/1`) and deferred references.

use crate::diagnostic::Diagnostics;
use crate::{
    Code, Diagnostic, FieldValue, Plane,
    files::Tree,
    index::{Holder, Recorder, Role, Site, Target, message},
    read::{Reader, is_identifier},
    uid::Claims,
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    sync::Arc,
};
use tilewright_jsonc::{MAX_SAFE_INTEGER, Member, Span, Value};

/// Longest string a `string` field accepts unless its schema says less.
pub const DEFAULT_MAXIMUM_STRING: usize = 65_536;

/// Deepest nesting of `list` and `object` field types. A list or object
/// field of a schema is at level 1, a list or object inside it at level 2,
/// and so on.
pub const MAXIMUM_FIELD_DEPTH: usize = 8;

/// Largest `min_items` or `max_items` of a `list` field: the array limit of
/// the strict JSONC dialect, so the default `max_items` accepts any array.
pub const MAXIMUM_LIST_ITEMS: usize = 250_000;

/// A closed set of fields, in declaration order, indexed by name. A schema
/// document has one, and so does each `object` field type.
#[derive(Clone, Debug, PartialEq)]
pub struct Schema {
    /// Source path of the schema document, relative to the root. Nested
    /// `object` types share their document's path.
    pub path: Arc<str>,
    fields: Vec<(String, FieldSpec)>,
    /// Positions in `fields` by name, kept with the list.
    index: BTreeMap<String, usize>,
    /// Positions in `fields` of the required fields, in declaration order,
    /// listed once so that checking an owner never walks the others.
    required: Vec<usize>,
}

impl Schema {
    pub(crate) const fn new(path: Arc<str>) -> Self {
        Self {
            path,
            fields: Vec::new(),
            index: BTreeMap::new(),
            required: Vec::new(),
        }
    }

    /// Adds a field; names come from one object's keys, so they are unique.
    fn push(&mut self, name: String, spec: FieldSpec) {
        let position = self.fields.len();
        if spec.required {
            self.required.push(position);
        }
        self.index.insert(name.clone(), position);
        self.fields.push((name, spec));
    }

    /// The position and spec of field `name`.
    fn lookup(&self, name: &str) -> Option<(usize, &FieldSpec)> {
        let position = *self.index.get(name)?;
        self.fields.get(position).map(|(_, spec)| (position, spec))
    }

    /// The fields in declaration order.
    #[must_use]
    pub fn fields(&self) -> &[(String, FieldSpec)] {
        &self.fields
    }

    #[must_use]
    pub fn field(&self, name: &str) -> Option<&FieldSpec> {
        self.lookup(name).map(|(_, spec)| spec)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldSpec {
    pub field_type: FieldType,
    pub required: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FieldType {
    Boolean,
    Integer {
        minimum: i64,
        maximum: i64,
    },
    Number {
        minimum: f64,
        maximum: f64,
    },
    /// A string of at most `maximum_length` characters, one of `values`
    /// when that is not empty.
    String {
        maximum_length: usize,
        values: BTreeSet<String>,
    },
    /// The identifier of a definition of type `target`.
    Reference {
        target: String,
    },
    /// Any JSON value.
    Json,
    /// An array of `minimum_items` to `maximum_items` values, each of type
    /// `items`.
    List {
        items: Box<FieldType>,
        minimum_items: usize,
        maximum_items: usize,
    },
    /// An object closed by `fields`.
    Object {
        fields: Schema,
    },
}

impl FieldType {
    /// Calls `visit` with every reference target in this type, nested ones
    /// included, and the dotted path of the field below this one that
    /// holds it (`[]` stands for a list's items).
    pub(crate) fn targets<'s>(&'s self, path: &str, visit: &mut impl FnMut(&str, &'s str)) {
        match self {
            Self::Reference { target } => visit(path, target),
            Self::List { items, .. } => items.targets(&format!("{path}[]"), visit),
            Self::Object { fields } => {
                for (name, spec) in fields.fields() {
                    spec.field_type.targets(&format!("{path}.{name}"), visit);
                }
            }
            _ => {}
        }
    }
}

/// Whether `name` is a valid field name: `^[a-z][a-z0-9_]{0,63}$`.
fn is_field_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
}

/// Reads a schema document at `path`. Also returns the span of the
/// `target` of each `reference` field type, in the order
/// [`FieldType::targets`] visits them: declaration order, a nested one
/// after the field that holds it.
pub(crate) fn read_schema(
    tree: &mut Tree<'_>,
    path: &str,
    diagnostics: &mut Diagnostics,
) -> Option<(Schema, Vec<Span>)> {
    let source = tree.document(path)?;
    let reader = Reader::new(&source, diagnostics);
    let root = reader.object(reader.root(), &["format", "fields"])?;
    let format_ok = reader.format(root, "tilewright.schema/1");
    let mut targets = Vec::new();
    let schema = read_field_specs(
        &reader,
        reader.required(root, "fields")?,
        &path.into(),
        0,
        &mut targets,
    );
    schema.filter(|_| format_ok).map(|schema| (schema, targets))
}

/// Reads a `fields` object of field specs. `depth` is the number of `list`
/// and `object` types around it. The span of each `reference` type's
/// `target` is added to `targets`.
fn read_field_specs<'a>(
    reader: &Reader<'a>,
    value: &'a Value,
    path: &Arc<str>,
    depth: usize,
    targets: &mut Vec<Span>,
) -> Option<Schema> {
    let members = reader.members(value)?;
    let mut schema = Schema::new(Arc::clone(path));
    let mut ok = true;
    for member in members {
        // A bad name is reported and its spec is still checked.
        let named = is_field_name(&member.key);
        if !named {
            reader.report(
                member.key_span,
                Code::Identifier,
                format!("\"{}\" is not a valid field name", member.key),
            );
            ok = false;
        }
        match read_spec(reader, &member.value, path, depth, true, targets).filter(|_| named) {
            Some((field_type, required)) => schema.push(
                member.key.clone(),
                FieldSpec {
                    field_type,
                    required,
                },
            ),
            None => ok = false,
        }
    }
    ok.then_some(schema)
}

/// Reads one field spec: its type and whether it is required. List items
/// are never missing, so they take no `required`: `takes_required` is
/// `false` for them. The span of a `reference` type's `target` is added
/// to `targets`.
#[allow(clippy::too_many_lines)]
fn read_spec<'a>(
    reader: &Reader<'a>,
    value: &'a Value,
    path: &Arc<str>,
    depth: usize,
    takes_required: bool,
    targets: &mut Vec<Span>,
) -> Option<(FieldType, bool)> {
    if value.as_object().is_none() {
        reader.report(
            value.span(),
            Code::Type,
            format!("expected an object, found {}", value.kind()),
        );
        return None;
    }
    let spec = value;
    let kind = reader.required(spec, "type");
    // Keys are checked against their type's options, or, when the type is
    // missing or unknown, against every type's, so a key no type allows is
    // reported either way.
    let options: &[&str] = match kind.and_then(Value::as_str) {
        Some("integer" | "number") => &["min", "max"],
        Some("string") => &["max_length", "values"],
        Some("reference") => &["target"],
        Some("list") => &["items", "min_items", "max_items"],
        Some("object") => &["fields"],
        Some("boolean" | "json") => &[],
        _ => &[
            "min",
            "max",
            "max_length",
            "values",
            "target",
            "items",
            "min_items",
            "max_items",
            "fields",
        ],
    };
    let mut allowed = vec!["type"];
    if takes_required {
        allowed.push("required");
    }
    allowed.extend(options);
    reader.object(spec, &allowed)?;
    // Every key is read before any result is used, so one bad key never
    // hides a problem in another.
    let required = match spec.get("required").filter(|_| takes_required) {
        Some(flag) => reader.boolean(flag),
        None => Some(false),
    };
    let kind = kind?;
    // A list or object too deep is reported, and the rest of the spec is
    // still read; the parser's nesting limit bounds the recursion.
    let too_deep = matches!(kind.as_str(), Some("list" | "object")) && depth >= MAXIMUM_FIELD_DEPTH;
    if too_deep {
        reader.report(
            kind.span(),
            Code::Range,
            format!("list and object types nest at most {MAXIMUM_FIELD_DEPTH} deep"),
        );
    }
    let field_type = match kind.as_str() {
        Some("boolean") => Some(FieldType::Boolean),
        Some("json") => Some(FieldType::Json),
        Some("integer") => {
            let bound = |key, default| match spec.get(key) {
                Some(value) => reader.integer(value, -MAX_SAFE_INTEGER, MAX_SAFE_INTEGER),
                None => Some(default),
            };
            let (minimum, maximum) = (
                bound("min", -MAX_SAFE_INTEGER),
                bound("max", MAX_SAFE_INTEGER),
            );
            ordered(
                reader,
                spec,
                minimum.zip(maximum),
                "min is greater than max",
            )
            .map(|(minimum, maximum)| FieldType::Integer { minimum, maximum })
        }
        Some("number") => {
            let bound = |key, default: f64| match spec.get(key) {
                Some(value) => {
                    let number = value.as_f64();
                    if number.is_none() {
                        reader.report(value.span(), Code::Type, "expected a number");
                    }
                    number
                }
                None => Some(default),
            };
            let (minimum, maximum) = (bound("min", f64::MIN), bound("max", f64::MAX));
            ordered(
                reader,
                spec,
                minimum.zip(maximum),
                "min is greater than max",
            )
            .map(|(minimum, maximum)| FieldType::Number { minimum, maximum })
        }
        Some("string") => {
            let maximum_length = match spec.get("max_length") {
                Some(value) => reader
                    .integer(value, 1, 1_048_576)
                    .and_then(|length| usize::try_from(length).ok()),
                None => Some(DEFAULT_MAXIMUM_STRING),
            };
            let values = match spec.get("values") {
                Some(value) => read_values(reader, value),
                None => Some(BTreeSet::new()),
            };
            maximum_length
                .zip(values)
                .map(|(maximum_length, values)| FieldType::String {
                    maximum_length,
                    values,
                })
        }
        Some("reference") => reader.required(spec, "target").and_then(|value| {
            let target = reader.identifier(value)?;
            targets.push(value.span());
            Some(FieldType::Reference { target })
        }),
        Some("list") => {
            let count = |key, default| match spec.get(key) {
                Some(value) => {
                    let limit = i64::try_from(MAXIMUM_LIST_ITEMS).unwrap_or(i64::MAX);
                    reader
                        .integer(value, 0, limit)
                        .and_then(|count| usize::try_from(count).ok())
                }
                None => Some(default),
            };
            let (minimum, maximum) = (
                count("min_items", 0),
                count("max_items", MAXIMUM_LIST_ITEMS),
            );
            let items = reader
                .required(spec, "items")
                .and_then(|items| read_spec(reader, items, path, depth + 1, false, targets));
            let bounds = ordered(
                reader,
                spec,
                minimum.zip(maximum),
                "min_items is greater than max_items",
            );
            bounds.zip(items).map(
                |((minimum_items, maximum_items), (items, _))| FieldType::List {
                    items: Box::new(items),
                    minimum_items,
                    maximum_items,
                },
            )
        }
        Some("object") => reader
            .required(spec, "fields")
            .and_then(|fields| read_field_specs(reader, fields, path, depth + 1, targets))
            .map(|fields| FieldType::Object { fields }),
        _ => {
            reader.report(
                kind.span(),
                Code::Range,
                "type must be boolean, integer, number, string, reference, json, list or object",
            );
            None
        }
    };
    field_type.zip(required).filter(|_| !too_deep)
}

/// `bounds` when its lower end is not above its upper end; otherwise reports
/// `message` at `spec`.
fn ordered<T: PartialOrd>(
    reader: &Reader<'_>,
    spec: &Value,
    bounds: Option<(T, T)>,
    message: &str,
) -> Option<(T, T)> {
    let (lower, upper) = bounds?;
    if lower > upper {
        reader.report(spec.span(), Code::Range, message);
        return None;
    }
    Some((lower, upper))
}

/// A string spec's `values`: unique strings, every item checked.
fn read_values<'a>(reader: &Reader<'a>, value: &'a Value) -> Option<BTreeSet<String>> {
    let mut values = BTreeSet::new();
    let mut ok = true;
    for item in reader.array(value)? {
        match reader.string(item) {
            Some(text) if !values.insert(text.to_owned()) => {
                reader.report(item.span(), Code::Duplicate, "value listed twice");
                ok = false;
            }
            Some(_) => {}
            None => ok = false,
        }
    }
    ok.then_some(values)
}

/// What references of one load may resolve to. Every target set is known
/// before any reference to it is read, so each reference is checked as it
/// is read: render layers, ground layers and definition types from the
/// profile before its schemas are read, definition keys from the `defs/`
/// layout before any definition is parsed, a plane's zones before its
/// zone parents and default zone are checked, planes and their anchors
/// before any chunk is read, and terrains before any transition. It also
/// holds the object uids claimed so far, so a duplicate is reported where
/// it is read.
///
/// Every reference the loader checks or follows goes through
/// [`References::check`] or [`References::follow`], which also record it
/// when the load builds a [`ReferenceIndex`](crate::ReferenceIndex).
pub(crate) struct References {
    render_layers: BTreeSet<String>,
    ground_layers: BTreeSet<String>,
    definition_types: BTreeSet<String>,
    /// Definition IDs by type, from the file names under `defs/`.
    definitions: BTreeMap<String, BTreeSet<String>>,
    /// Anchor IDs by plane, for every plane that loaded.
    planes: BTreeMap<String, BTreeSet<String>>,
    /// Zone IDs by plane, for every plane whose zones were read.
    zones: BTreeMap<String, BTreeSet<String>>,
    /// The terrains of the transitions document.
    terrains: BTreeSet<String>,
    uids: Claims,
    /// Present when the load records its references.
    recorder: Option<RefCell<Recorder>>,
    /// The field path of each `reference` field type a recorded value
    /// had, by the type's address, so that each is written out once.
    field_paths: RefCell<BTreeMap<usize, Rc<str>>>,
}

impl References {
    /// The references of a load; `record` makes it keep them.
    pub fn new(record: bool) -> Self {
        Self {
            render_layers: BTreeSet::new(),
            ground_layers: BTreeSet::new(),
            definition_types: BTreeSet::new(),
            definitions: BTreeMap::new(),
            planes: BTreeMap::new(),
            zones: BTreeMap::new(),
            terrains: BTreeSet::new(),
            uids: Claims::default(),
            recorder: record.then(|| RefCell::new(Recorder::default())),
            field_paths: RefCell::new(BTreeMap::new()),
        }
    }

    /// The field path of a value of the `reference` type `field_type`,
    /// at `path`: written out the first time a value of that type is
    /// recorded, and shared after that, as every value of one type of a
    /// schema has one path. Schemas live as long as the load, so a type's
    /// address names it.
    fn field_path(&self, field_type: &FieldType, path: &FieldPath<'_>) -> Rc<str> {
        let key = std::ptr::from_ref(field_type).addr();
        Rc::clone(
            self.field_paths
                .borrow_mut()
                .entry(key)
                .or_insert_with(|| path.render().into()),
        )
    }

    /// Whether this load records its references, so that a caller builds
    /// what only a record needs.
    pub const fn records(&self) -> bool {
        self.recorder.is_some()
    }

    /// The references recorded, if this load records them.
    pub fn into_recorder(self) -> Option<Recorder> {
        self.recorder.map(RefCell::into_inner)
    }

    /// Records the profile's render layers.
    pub fn render_layers_declared<'n>(&mut self, layers: impl IntoIterator<Item = &'n String>) {
        self.render_layers.extend(layers.into_iter().cloned());
    }

    /// Records the profile's ground layers and definition types.
    pub fn profile_declared<'n>(
        &mut self,
        ground_layers: impl IntoIterator<Item = &'n String>,
        definition_types: impl IntoIterator<Item = &'n String>,
    ) {
        self.ground_layers
            .extend(ground_layers.into_iter().cloned());
        self.definition_types
            .extend(definition_types.into_iter().cloned());
    }

    /// Claims an object uid for an object of the chunk at `path`; see
    /// [`Claims::claim`].
    pub fn claim_uid(&mut self, uid: &str, path: &Arc<str>) -> Option<Arc<str>> {
        self.uids.claim(uid, path)
    }

    /// Records the definitions the `defs/` layout holds, before any is read.
    pub fn definitions_listed(&mut self, keys: impl IntoIterator<Item = (String, String)>) {
        for (kind, id) in keys {
            self.definitions.entry(kind).or_default().insert(id);
        }
    }

    /// Records the zones of `plane` that read, before their parents and
    /// the plane's default zone are checked.
    pub fn zones_read<'z>(&mut self, plane: &str, zones: impl IntoIterator<Item = &'z String>) {
        self.zones
            .entry(plane.to_owned())
            .or_default()
            .extend(zones.into_iter().cloned());
    }

    /// Records the terrains of the transitions document, before any
    /// transition is read.
    pub fn terrains_declared<'t>(&mut self, terrains: impl IntoIterator<Item = &'t String>) {
        self.terrains.extend(terrains.into_iter().cloned());
    }

    /// Records a plane that loaded, with its anchors, before any chunk is
    /// read.
    pub fn plane_read(&mut self, plane: &Plane) {
        self.planes
            .insert(plane.id.clone(), plane.anchors.keys().cloned().collect());
    }

    /// Whether `target` resolves. A file always does here: whether it is
    /// there is the reading's to report.
    pub fn resolves(&self, target: &Target<'_>) -> bool {
        match *target {
            Target::RenderLayer(layer) => self.render_layers.contains(layer),
            Target::GroundLayer(layer) => self.ground_layers.contains(layer),
            Target::DefinitionType(kind) => self.definition_types.contains(kind),
            Target::Definition { kind, id } => self
                .definitions
                .get(kind)
                .is_some_and(|ids| ids.contains(id)),
            Target::Plane(plane) => self.planes.contains_key(plane),
            Target::Anchor { plane, id } => self
                .planes
                .get(plane)
                .is_some_and(|anchors| anchors.contains(id)),
            Target::Zone { plane, id } => self
                .zones
                .get(plane)
                .is_some_and(|zones| zones.contains(id)),
            Target::Terrain(terrain) => self.terrains.contains(terrain),
            Target::File(_) => true,
        }
    }

    /// The one check of a reference: records it when this load records
    /// references, and says whether it resolves. The caller reports one
    /// that does not, with [`message`](crate::index::message).
    pub fn check(&self, site: &Site<'_>) -> bool {
        self.follow(site);
        self.resolves(&site.target)
    }

    /// Records a reference that the layout, not a lookup, checks: a path
    /// read as a document, a plane directory, a definition's directory.
    pub fn follow(&self, site: &Site<'_>) {
        if let Some(recorder) = &self.recorder {
            recorder.borrow_mut().record(site);
        }
    }

    /// Checks a reference whose problem is reported on its file as a
    /// whole, without a position; returns whether it resolves.
    pub fn check_file(&self, site: &Site<'_>, diagnostics: &mut Diagnostics) -> bool {
        let resolves = self.check(site);
        if !resolves {
            diagnostics.push(Diagnostic::file(site.file, Code::Reference, message(site)));
        }
        resolves
    }
}

/// Where a value is in a `fields` object, for the field path a recorded
/// reference carries: built on the stack as values are checked, and
/// written out only when a reference is recorded.
#[derive(Clone, Copy)]
pub(crate) enum FieldPath<'p> {
    Root,
    Field(&'p FieldPath<'p>, &'p str),
    Items(&'p FieldPath<'p>),
}

impl FieldPath<'_> {
    /// The path as [`Role::Field`](crate::index::Role::Field) documents
    /// it: names joined by `.`, with `[]` for a list's items.
    fn render(&self) -> String {
        match self {
            Self::Root => String::new(),
            Self::Field(parent, name) => match parent.render() {
                parent if parent.is_empty() => (*name).to_owned(),
                parent => format!("{parent}.{name}"),
            },
            Self::Items(parent) => format!("{}[]", parent.render()),
        }
    }
}

/// Validates a `fields` object against `schema`. Missing required fields are
/// reported at `owner`, the object holding `fields`, and `holder` holds
/// the references its values make.
pub(crate) fn read_fields<'a>(
    reader: &Reader<'a>,
    (owner, holder): (&'a Value, Holder<'_>),
    fields: Option<&'a Value>,
    schema: &Schema,
    references: &mut References,
) -> Option<Vec<(String, FieldValue)>> {
    let members = match fields {
        Some(fields) => reader.members(fields)?,
        None => &[],
    };
    let mut output = Vec::with_capacity(members.len());
    let at = Values {
        holder,
        path: &FieldPath::Root,
    };
    let ok = check_members(
        reader,
        members,
        fields.unwrap_or(owner).span(),
        schema,
        (references, at),
        &mut |member, spec| {
            output.push((
                member.key.clone(),
                typed_value(reader, &member.value, &spec.field_type),
            ));
        },
    );
    ok.then_some(output)
}

/// Checks an object's members against `schema`: each key is one of its
/// fields and each value has that field's type. Missing required fields are
/// one diagnostic at `owner`. `accept` gets every member whose value is
/// valid.
///
/// The work is proportional to the members the object has, however many
/// fields the schema requires. An object with every required field is not
/// checked for missing ones; naming the first few that another object lacks
/// visits at most [`NAMED`] more required fields than it has.
fn check_members<'a>(
    reader: &Reader<'a>,
    members: &'a [Member],
    owner: Span,
    schema: &Schema,
    (references, at): (&mut References, Values<'_>),
    accept: &mut dyn FnMut(&'a Member, &FieldSpec),
) -> bool {
    let mut ok = true;
    // Positions of the required fields present. Keys are unique, so the
    // owner lacks exactly the other required fields.
    let mut present = BTreeSet::new();
    for member in members {
        let Some((position, spec)) = schema.lookup(&member.key) else {
            reader.report(
                member.key_span,
                Code::UnknownKey,
                format!("field \"{}\" is not in {}", member.key, schema.path),
            );
            ok = false;
            continue;
        };
        if spec.required {
            present.insert(position);
        }
        let path = FieldPath::Field(at.path, &member.key);
        let at = Values { path: &path, ..at };
        if check_value(reader, &member.value, &spec.field_type, (references, at)) {
            accept(member, spec);
        } else {
            ok = false;
        }
    }
    // One diagnostic per owner, however many fields it lacks.
    let missing = schema.required.len() - present.len();
    if missing > 0 {
        let named: Vec<&str> = schema
            .required
            .iter()
            .inspect(|_| visited())
            .filter(|position| !present.contains(position))
            .take(NAMED)
            .filter_map(|position| schema.fields.get(*position))
            .map(|(name, _)| name.as_str())
            .collect();
        reader.report(owner, Code::MissingKey, missing_message(&named, missing));
        ok = false;
    }
    ok
}

/// Missing required fields named in one diagnostic; the rest are counted.
const NAMED: usize = 3;

#[cfg(test)]
thread_local! {
    /// Required fields visited on this thread while naming missing ones, so
    /// tests can bound the work of a load.
    pub(crate) static REQUIRED_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Counts a required field visited while naming missing ones.
#[cfg(test)]
fn visited() {
    REQUIRED_VISITS.with(|visits| visits.set(visits.get() + 1));
}

#[cfg(not(test))]
const fn visited() {}

/// The diagnostic for an owner that lacks `count` required fields, the
/// first of which are `named`.
fn missing_message(named: &[&str], count: usize) -> String {
    let list = named
        .iter()
        .map(|name| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(", ");
    match count {
        1 => format!("required field {list} is missing"),
        count if count <= named.len() => format!("required fields {list} are missing"),
        count => format!(
            "required fields {list} and {} more are missing",
            count - named.len()
        ),
    }
}

/// The model value of a field whose value has passed [`check_value`].
fn typed_value(reader: &Reader<'_>, value: &Value, field_type: &FieldType) -> FieldValue {
    match (field_type, value.as_f64()) {
        (FieldType::Number { .. }, Some(number)) => FieldValue::Number(number),
        _ => reader.field_value(value),
    }
}

/// Where the values being checked are: what holds their references, and
/// the field path of the value.
#[derive(Clone, Copy)]
struct Values<'p> {
    holder: Holder<'p>,
    path: &'p FieldPath<'p>,
}

/// Checks that `value` has type `field_type`, reporting every problem in
/// it, nested ones included. Recursion follows the type, whose nesting
/// the schema bounds by [`MAXIMUM_FIELD_DEPTH`].
fn check_value<'a>(
    reader: &Reader<'a>,
    value: &'a Value,
    field_type: &FieldType,
    (references, at): (&mut References, Values<'_>),
) -> bool {
    match field_type {
        FieldType::Boolean => reader.boolean(value).is_some(),
        FieldType::Integer { minimum, maximum } => {
            reader.integer(value, *minimum, *maximum).is_some()
        }
        FieldType::Number { minimum, maximum } => {
            let Some(number) = value.as_f64() else {
                reader.report(
                    value.span(),
                    Code::Type,
                    format!("expected a number, found {}", value.kind()),
                );
                return false;
            };
            if number < *minimum || number > *maximum {
                reader.report(
                    value.span(),
                    Code::Range,
                    format!("{number} is outside {minimum}..={maximum}"),
                );
                return false;
            }
            true
        }
        FieldType::String {
            maximum_length,
            values,
        } => {
            let Some(text) = reader.text(value, *maximum_length) else {
                return false;
            };
            if !values.is_empty() && !values.contains(&text) {
                reader.report(
                    value.span(),
                    Code::Range,
                    format!("\"{text}\" is not one of the allowed values"),
                );
                return false;
            }
            true
        }
        FieldType::Reference { target } => {
            let Some(id) = reader.string(value) else {
                return false;
            };
            if !is_identifier(id) {
                reader.report(
                    value.span(),
                    Code::Identifier,
                    format!("\"{id}\" is not a valid identifier"),
                );
                return false;
            }
            // The path is written out only for a reference that is kept.
            let field = if references.records() {
                references.field_path(field_type, at.path)
            } else {
                Rc::from("")
            };
            reader.reference(
                references,
                Target::Definition { kind: target, id },
                (Role::Field { field: &field }, at.holder),
                value.span(),
            );
            true
        }
        FieldType::Json => true,
        FieldType::List {
            items,
            minimum_items,
            maximum_items,
        } => check_list(
            reader,
            value,
            items,
            *minimum_items..=*maximum_items,
            (references, at),
        ),
        FieldType::Object { fields } => {
            let Some(members) = reader.members(value) else {
                return false;
            };
            check_members(
                reader,
                members,
                value.span(),
                fields,
                (references, at),
                &mut |_, _| {},
            )
        }
    }
}

/// Checks a `list` value: an array of `count` items, each of type `items`.
/// Every item is checked, even when the count is wrong, so each bad one is
/// reported.
fn check_list<'a>(
    reader: &Reader<'a>,
    value: &'a Value,
    items: &FieldType,
    count: std::ops::RangeInclusive<usize>,
    (references, at): (&mut References, Values<'_>),
) -> bool {
    let Some(elements) = reader.array(value) else {
        return false;
    };
    let mut ok = count.contains(&elements.len());
    if !ok {
        reader.report(
            value.span(),
            Code::Range,
            format!(
                "{} items, outside {}..={}",
                elements.len(),
                count.start(),
                count.end()
            ),
        );
    }
    let path = FieldPath::Items(at.path);
    let at = Values { path: &path, ..at };
    for element in elements {
        ok &= check_value(reader, element, items, (references, at));
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::{References, Target};
    use std::path::Path;

    #[test]
    fn references_resolve_against_what_is_known_before_they_are_read() {
        let world =
            crate::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/basic"))
                .expect("the fixture is valid");
        let mut references = References::new(false);
        references.render_layers_declared(&world.profile().render_layers);
        let proto = |id| Target::Definition { kind: "proto", id };
        let anchor = Target::Anchor {
            plane: "overworld",
            id: "village.well",
        };
        assert!(references.resolves(&Target::RenderLayer("wall")));
        assert!(!references.resolves(&Target::RenderLayer("roof")));
        assert!(!references.resolves(&proto("tree")));
        assert!(!references.resolves(&anchor));
        references.definitions_listed(world.definitions().keys().cloned());
        references.plane_read(&world.planes()["overworld"]);
        assert!(references.resolves(&proto("tree")));
        assert!(!references.resolves(&proto("missing")));
        assert!(references.resolves(&anchor));
        assert!(references.resolves(&Target::Plane("overworld")));
        assert!(!references.resolves(&Target::Plane("vault")));
        assert!(!references.resolves(&Target::Zone {
            plane: "overworld",
            id: "village"
        }));
        references.zones_read("overworld", world.planes()["overworld"].zones.keys());
        assert!(references.resolves(&Target::Zone {
            plane: "overworld",
            id: "village"
        }));
        assert!(!references.resolves(&Target::Zone {
            plane: "vault",
            id: "village"
        }));
    }
}
