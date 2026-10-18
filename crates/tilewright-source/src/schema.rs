// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Closed field schemas (`tilewright.schema/1`) and deferred references.

use crate::diagnostic::Diagnostics;
use crate::{
    Code, FieldValue, Plane, Profile,
    files::Tree,
    read::{Reader, is_identifier},
};
use std::collections::{BTreeMap, BTreeSet};
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
    /// Source path of the schema document, relative to the root.
    pub path: String,
    fields: Vec<(String, FieldSpec)>,
    /// Positions in `fields` by name, kept with the list.
    index: BTreeMap<String, usize>,
    /// Positions in `fields` of the required fields, in declaration order.
    required: Vec<usize>,
}

impl Schema {
    pub(crate) const fn new(path: String) -> Self {
        Self {
            path,
            fields: Vec::new(),
            index: BTreeMap::new(),
            required: Vec::new(),
        }
    }

    /// Adds a field; names come from one object's keys, so they are unique.
    fn push(&mut self, name: String, spec: FieldSpec) {
        if spec.required {
            self.required.push(self.fields.len());
        }
        self.index.insert(name.clone(), self.fields.len());
        self.fields.push((name, spec));
    }

    /// The fields in declaration order.
    #[must_use]
    pub fn fields(&self) -> &[(String, FieldSpec)] {
        &self.fields
    }

    #[must_use]
    pub fn field(&self, name: &str) -> Option<&FieldSpec> {
        let position = *self.index.get(name)?;
        self.fields.get(position).map(|(_, spec)| spec)
    }

    /// The names of the required fields, in declaration order.
    pub fn required(&self) -> impl Iterator<Item = &str> {
        self.required
            .iter()
            .filter_map(|position| self.fields.get(*position))
            .map(|(name, _)| name.as_str())
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

/// Reads a schema document at `path`.
pub(crate) fn read_schema(
    tree: &mut Tree<'_>,
    path: &str,
    diagnostics: &mut Diagnostics,
) -> Option<Schema> {
    let source = tree.document(path)?;
    let reader = Reader::new(&source, diagnostics);
    let root = reader.object(reader.root(), &["format", "fields"])?;
    let format_ok = reader.format(root, "tilewright.schema/1");
    let schema = read_field_specs(&reader, reader.required(root, "fields")?, path, 0);
    schema.filter(|_| format_ok)
}

/// Reads a `fields` object of field specs. `depth` is the number of `list`
/// and `object` types around it.
fn read_field_specs<'a>(
    reader: &Reader<'a>,
    value: &'a Value,
    path: &str,
    depth: usize,
) -> Option<Schema> {
    let members = reader.members(value)?;
    let mut schema = Schema::new(path.to_owned());
    let mut ok = true;
    for member in members {
        if !is_field_name(&member.key) {
            reader.report(
                member.key_span,
                Code::Identifier,
                format!("\"{}\" is not a valid field name", member.key),
            );
            ok = false;
            continue;
        }
        match read_spec(reader, &member.value, path, depth, true) {
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
/// `false` for them.
#[allow(clippy::too_many_lines)]
fn read_spec<'a>(
    reader: &Reader<'a>,
    value: &'a Value,
    path: &str,
    depth: usize,
    takes_required: bool,
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
    let kind = reader.required(spec, "type")?;
    let options: &[&str] = match kind.as_str() {
        Some("integer" | "number") => &["min", "max"],
        Some("string") => &["max_length", "values"],
        Some("reference") => &["target"],
        Some("list") => &["items", "min_items", "max_items"],
        Some("object") => &["fields"],
        _ => &[],
    };
    let mut allowed = vec!["type"];
    if takes_required {
        allowed.push("required");
    }
    allowed.extend(options);
    reader.object(spec, &allowed)?;
    let required = match spec.get("required").filter(|_| takes_required) {
        Some(flag) => reader.boolean(flag)?,
        None => false,
    };
    if matches!(kind.as_str(), Some("list" | "object")) && depth >= MAXIMUM_FIELD_DEPTH {
        reader.report(
            kind.span(),
            Code::Range,
            format!("list and object types nest at most {MAXIMUM_FIELD_DEPTH} deep"),
        );
        return None;
    }
    let field_type = match kind.as_str() {
        Some("boolean") => FieldType::Boolean,
        Some("json") => FieldType::Json,
        Some("integer") => {
            let bound = |reader: &Reader<'a>, key, default| match spec.get(key) {
                Some(value) => reader.integer(value, -MAX_SAFE_INTEGER, MAX_SAFE_INTEGER),
                None => Some(default),
            };
            let minimum = bound(reader, "min", -MAX_SAFE_INTEGER)?;
            let maximum = bound(reader, "max", MAX_SAFE_INTEGER)?;
            if minimum > maximum {
                reader.report(spec.span(), Code::Range, "min is greater than max");
                return None;
            }
            FieldType::Integer { minimum, maximum }
        }
        Some("number") => {
            let bound = |reader: &Reader<'a>, key, default: f64| match spec.get(key) {
                Some(value) => {
                    let number = value.as_f64();
                    if number.is_none() {
                        reader.report(value.span(), Code::Type, "expected a number");
                    }
                    number
                }
                None => Some(default),
            };
            let minimum = bound(reader, "min", f64::MIN)?;
            let maximum = bound(reader, "max", f64::MAX)?;
            if minimum > maximum {
                reader.report(spec.span(), Code::Range, "min is greater than max");
                return None;
            }
            FieldType::Number { minimum, maximum }
        }
        Some("string") => {
            let maximum_length = match spec.get("max_length") {
                Some(value) => usize::try_from(reader.integer(value, 1, 1_048_576)?).ok()?,
                None => DEFAULT_MAXIMUM_STRING,
            };
            let values = match spec.get("values") {
                Some(value) => {
                    let mut values = BTreeSet::new();
                    for item in reader.array(value)? {
                        if !values.insert(reader.string(item)?.to_owned()) {
                            reader.report(item.span(), Code::Duplicate, "value listed twice");
                            return None;
                        }
                    }
                    values
                }
                None => BTreeSet::new(),
            };
            FieldType::String {
                maximum_length,
                values,
            }
        }
        Some("reference") => FieldType::Reference {
            target: reader.identifier(reader.required(spec, "target")?)?,
        },
        Some("list") => {
            let count = |key, default| match spec.get(key) {
                Some(value) => {
                    let limit = i64::try_from(MAXIMUM_LIST_ITEMS).unwrap_or(i64::MAX);
                    usize::try_from(reader.integer(value, 0, limit)?).ok()
                }
                None => Some(default),
            };
            let minimum_items = count("min_items", 0)?;
            let maximum_items = count("max_items", MAXIMUM_LIST_ITEMS)?;
            if minimum_items > maximum_items {
                reader.report(
                    spec.span(),
                    Code::Range,
                    "min_items is greater than max_items",
                );
                return None;
            }
            let items = reader.required(spec, "items")?;
            let (items, _) = read_spec(reader, items, path, depth + 1, false)?;
            FieldType::List {
                items: Box::new(items),
                minimum_items,
                maximum_items,
            }
        }
        Some("object") => FieldType::Object {
            fields: read_field_specs(reader, reader.required(spec, "fields")?, path, depth + 1)?,
        },
        _ => {
            reader.report(
                kind.span(),
                Code::Range,
                "type must be boolean, integer, number, string, reference, json, list or object",
            );
            return None;
        }
    };
    Some((field_type, required))
}

/// What a reference names.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum Target {
    Definition { kind: String, id: String },
    Plane(String),
    Anchor { plane: String, id: String },
    RenderLayer(String),
}

impl Target {
    /// The diagnostic message when the reference does not resolve.
    pub fn message(&self) -> String {
        match self {
            Self::Definition { kind, id } => format!("no {kind} definition \"{id}\""),
            Self::Plane(plane) => format!("no plane \"{plane}\""),
            Self::Anchor { plane, id } => format!("no anchor \"{id}\" in plane \"{plane}\""),
            Self::RenderLayer(layer) => {
                format!("\"{layer}\" is not a render layer of the profile")
            }
        }
    }
}

/// What references of one load may resolve to. Every target set is known
/// before any reference to it is read, so each reference is checked as it
/// is read and none is kept: render layers from the profile, definition keys
/// from the `defs/` layout before any definition is parsed, and planes and
/// their anchors before any chunk is read.
pub(crate) struct References {
    render_layers: BTreeSet<String>,
    /// Definition IDs by type, from the file names under `defs/`.
    definitions: BTreeMap<String, BTreeSet<String>>,
    /// Anchor IDs by plane, for every plane that loaded.
    planes: BTreeMap<String, BTreeSet<String>>,
}

impl References {
    pub fn new(profile: &Profile) -> Self {
        Self {
            render_layers: profile.render_layers.iter().cloned().collect(),
            definitions: BTreeMap::new(),
            planes: BTreeMap::new(),
        }
    }

    /// Records the definitions the `defs/` layout holds, before any is read.
    pub fn definitions_listed(&mut self, keys: impl IntoIterator<Item = (String, String)>) {
        for (kind, id) in keys {
            self.definitions.entry(kind).or_default().insert(id);
        }
    }

    /// Records a plane that loaded, with its anchors, before any chunk is
    /// read.
    pub fn plane_read(&mut self, plane: &Plane) {
        self.planes
            .insert(plane.id.clone(), plane.anchors.keys().cloned().collect());
    }

    /// Whether `target` resolves.
    pub fn resolves(&self, target: &Target) -> bool {
        match target {
            Target::RenderLayer(layer) => self.render_layers.contains(layer),
            Target::Definition { kind, id } => self
                .definitions
                .get(kind)
                .is_some_and(|ids| ids.contains(id)),
            Target::Plane(plane) => self.planes.contains_key(plane),
            Target::Anchor { plane, id } => self
                .planes
                .get(plane)
                .is_some_and(|anchors| anchors.contains(id)),
        }
    }
}

/// Validates a `fields` object against `schema`. Missing required fields are
/// reported at `owner`, the object holding `fields`.
pub(crate) fn read_fields<'a>(
    reader: &Reader<'a>,
    owner: &'a Value,
    fields: Option<&'a Value>,
    schema: &Schema,
    references: &mut References,
) -> Option<Vec<(String, FieldValue)>> {
    let members = match fields {
        Some(fields) => reader.members(fields)?,
        None => &[],
    };
    let mut output = Vec::with_capacity(members.len());
    let ok = check_members(
        reader,
        members,
        fields.unwrap_or(owner).span(),
        schema,
        references,
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
fn check_members<'a>(
    reader: &Reader<'a>,
    members: &'a [Member],
    owner: Span,
    schema: &Schema,
    references: &mut References,
    accept: &mut dyn FnMut(&'a Member, &FieldSpec),
) -> bool {
    let mut ok = true;
    // Keys are unique, so counting the required ones present is enough to
    // know whether any is missing.
    let mut required = 0;
    for member in members {
        let Some(spec) = schema.field(&member.key) else {
            reader.report(
                member.key_span,
                Code::UnknownKey,
                format!("field \"{}\" is not in {}", member.key, schema.path),
            );
            ok = false;
            continue;
        };
        required += usize::from(spec.required);
        if check_value(reader, &member.value, &spec.field_type, references) {
            accept(member, spec);
        } else {
            ok = false;
        }
    }
    let missing = schema.required.len().saturating_sub(required);
    if missing > 0 {
        reader.report(
            owner,
            Code::MissingKey,
            missing_message(schema, members, missing),
        );
        ok = false;
    }
    ok
}

/// Names the first few missing required fields and counts the rest. The
/// walk stops at the last field it names, so it passes at most the
/// object's own members and the fields it names.
fn missing_message(schema: &Schema, members: &[Member], missing: usize) -> String {
    const NAMED: usize = 3;
    let present: BTreeSet<&str> = members.iter().map(|member| member.key.as_str()).collect();
    let named = schema
        .required()
        .filter(|name| !present.contains(name))
        .take(NAMED)
        .map(|name| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(", ");
    match missing {
        1 => format!("required field {named} is missing"),
        count if count <= NAMED => format!("required fields {named} are missing"),
        count => format!(
            "required fields {named} and {} more are missing",
            count - NAMED
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

/// Checks that `value` has type `field_type`, reporting every problem in
/// it, nested ones included. Recursion follows the type, whose nesting
/// the schema bounds by [`MAXIMUM_FIELD_DEPTH`].
fn check_value<'a>(
    reader: &Reader<'a>,
    value: &'a Value,
    field_type: &FieldType,
    references: &mut References,
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
            reader.reference(
                references,
                &Target::Definition {
                    kind: target.clone(),
                    id: id.to_owned(),
                },
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
            references,
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
                references,
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
    references: &mut References,
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
    for element in elements {
        ok &= check_value(reader, element, items, references);
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
        let mut references = References::new(world.profile());
        let proto = |id: &str| Target::Definition {
            kind: "proto".to_owned(),
            id: id.to_owned(),
        };
        let anchor = Target::Anchor {
            plane: "overworld".to_owned(),
            id: "village.well".to_owned(),
        };
        assert!(references.resolves(&Target::RenderLayer("wall".to_owned())));
        assert!(!references.resolves(&Target::RenderLayer("roof".to_owned())));
        assert!(!references.resolves(&proto("tree")));
        assert!(!references.resolves(&anchor));
        references.definitions_listed(world.definitions().keys().cloned());
        references.plane_read(&world.planes()["overworld"]);
        assert!(references.resolves(&proto("tree")));
        assert!(!references.resolves(&proto("missing")));
        assert!(references.resolves(&anchor));
        assert!(references.resolves(&Target::Plane("overworld".to_owned())));
        assert!(!references.resolves(&Target::Plane("vault".to_owned())));
    }
}
