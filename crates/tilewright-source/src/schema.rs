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
use tilewright_jsonc::{MAX_SAFE_INTEGER, Value};

/// Longest string a `string` field accepts unless its schema says less.
pub const DEFAULT_MAXIMUM_STRING: usize = 65_536;

/// A closed set of fields, in declaration order, indexed by name.
#[derive(Clone, Debug, PartialEq)]
pub struct Schema {
    /// Source path, relative to the root.
    pub path: String,
    fields: Vec<(String, FieldSpec)>,
    /// Positions in `fields` by name, kept with the list.
    index: BTreeMap<String, usize>,
}

impl Schema {
    pub(crate) const fn new(path: String) -> Self {
        Self {
            path,
            fields: Vec::new(),
            index: BTreeMap::new(),
        }
    }

    /// Adds a field; names come from one object's keys, so they are unique.
    fn push(&mut self, name: String, spec: FieldSpec) {
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
    let fields = reader.required(root, "fields")?;
    let members = reader.members(fields)?;
    let mut schema = Schema::new(path.to_owned());
    let mut ok = format_ok;
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
        match read_spec(&reader, &member.value) {
            Some(spec) => schema.push(member.key.clone(), spec),
            None => ok = false,
        }
    }
    ok.then_some(schema)
}

fn read_spec<'a>(reader: &Reader<'a>, value: &'a Value) -> Option<FieldSpec> {
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
    let allowed: &[&str] = match kind.as_str() {
        Some("integer" | "number") => &["type", "required", "min", "max"],
        Some("string") => &["type", "required", "max_length", "values"],
        Some("reference") => &["type", "required", "target"],
        _ => &["type", "required"],
    };
    reader.object(spec, allowed)?;
    let required = match spec.get("required") {
        Some(flag) => reader.boolean(flag)?,
        None => false,
    };
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
        _ => {
            reader.report(
                kind.span(),
                Code::Range,
                "type must be boolean, integer, number, string, reference or json",
            );
            return None;
        }
    };
    Some(FieldSpec {
        field_type,
        required,
    })
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
    let mut output = Vec::new();
    let mut ok = true;
    let members = match fields {
        Some(fields) => reader.members(fields)?,
        None => &[],
    };
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
        match check_field(reader, &member.value, spec, references) {
            Some(value) => output.push((member.key.clone(), value)),
            None => ok = false,
        }
    }
    // One diagnostic per owner, however many fields it lacks.
    let present: BTreeSet<&str> = members.iter().map(|member| member.key.as_str()).collect();
    let missing: Vec<&str> = schema
        .fields()
        .iter()
        .filter(|(name, spec)| spec.required && !present.contains(name.as_str()))
        .map(|(name, _)| name.as_str())
        .collect();
    if let Some(message) = missing_message(&missing) {
        reader.report(fields.unwrap_or(owner).span(), Code::MissingKey, message);
        ok = false;
    }
    ok.then_some(output)
}

/// Names a few missing required fields and counts the rest.
fn missing_message(missing: &[&str]) -> Option<String> {
    const NAMED: usize = 3;
    let named = missing
        .iter()
        .take(NAMED)
        .map(|name| format!("\"{name}\""))
        .collect::<Vec<_>>()
        .join(", ");
    match missing.len() {
        0 => None,
        1 => Some(format!("required field {named} is missing")),
        count if count <= NAMED => Some(format!("required fields {named} are missing")),
        count => Some(format!(
            "required fields {named} and {} more are missing",
            count - NAMED
        )),
    }
}

fn check_field<'a>(
    reader: &Reader<'a>,
    value: &'a Value,
    spec: &FieldSpec,
    references: &mut References,
) -> Option<FieldValue> {
    match &spec.field_type {
        FieldType::Boolean => reader.boolean(value).map(FieldValue::Bool),
        FieldType::Integer { minimum, maximum } => reader
            .integer(value, *minimum, *maximum)
            .map(FieldValue::Integer),
        FieldType::Number { minimum, maximum } => {
            let Some(number) = value.as_f64() else {
                reader.report(
                    value.span(),
                    Code::Type,
                    format!("expected a number, found {}", value.kind()),
                );
                return None;
            };
            if number < *minimum || number > *maximum {
                reader.report(
                    value.span(),
                    Code::Range,
                    format!("{number} is outside {minimum}..={maximum}"),
                );
                return None;
            }
            Some(FieldValue::Number(number))
        }
        FieldType::String {
            maximum_length,
            values,
        } => {
            let text = reader.text(value, *maximum_length)?;
            if !values.is_empty() && !values.contains(&text) {
                reader.report(
                    value.span(),
                    Code::Range,
                    format!("\"{text}\" is not one of the allowed values"),
                );
                return None;
            }
            Some(FieldValue::String(text))
        }
        FieldType::Reference { target } => {
            let id = reader.string(value)?;
            if !is_identifier(id) {
                reader.report(
                    value.span(),
                    Code::Identifier,
                    format!("\"{id}\" is not a valid identifier"),
                );
                return None;
            }
            reader.reference(
                references,
                &Target::Definition {
                    kind: target.clone(),
                    id: id.to_owned(),
                },
                value.span(),
            );
            Some(FieldValue::String(id.to_owned()))
        }
        FieldType::Json => Some(reader.field_value(value)),
    }
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
