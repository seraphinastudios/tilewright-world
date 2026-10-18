// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Definitions: `defs/<type>/<id>.jsonc`.

use crate::diagnostic::Diagnostics;
use crate::{
    Code, Definition, Diagnostic, Profile,
    files::{EntryKind, Tree, join},
    read::{Reader, is_identifier},
    schema::{References, read_fields},
};
use std::collections::BTreeMap;

pub(crate) fn read_definitions(
    tree: &mut Tree<'_>,
    profile: &Profile,
    references: &mut References,
    diagnostics: &mut Diagnostics,
) -> BTreeMap<(String, String), Definition> {
    let mut definitions = BTreeMap::new();
    if tree.kind("defs") != Some(EntryKind::Directory) {
        return definitions;
    }
    for type_entry in tree.list("defs") {
        let directory = join("defs", &type_entry.name);
        let Some(schema) = profile
            .definition_types
            .get(&type_entry.name)
            .filter(|_| type_entry.kind == EntryKind::Directory)
        else {
            diagnostics.push(Diagnostic::file(
                &directory,
                Code::UnexpectedFile,
                "not a definition type declared by the profile",
            ));
            continue;
        };
        for entry in tree.list(&directory) {
            let path = join(&directory, &entry.name);
            let Some(id) = entry
                .name
                .strip_suffix(".jsonc")
                .filter(|id| is_identifier(id) && entry.kind == EntryKind::File)
            else {
                diagnostics.push(Diagnostic::file(
                    &path,
                    Code::UnexpectedFile,
                    "definition files are named <id>.jsonc",
                ));
                continue;
            };
            let Some(source) = tree.document(&path) else {
                continue;
            };
            let reader = Reader::new(&source, diagnostics);
            let Some(root) =
                reader.object(reader.root(), &["format", "type", "id", "fields", "custom"])
            else {
                continue;
            };
            let mut ok = reader.format(root, "tilewright.def/1");
            for (key, expected) in [("type", type_entry.name.as_str()), ("id", id)] {
                match reader.required(root, key) {
                    Some(value) if value.as_str() == Some(expected) => {}
                    Some(value) => {
                        reader.report(
                            value.span(),
                            Code::NameMismatch,
                            format!("{key} must be \"{expected}\" to match the path"),
                        );
                        ok = false;
                    }
                    None => ok = false,
                }
            }
            let fields = read_fields(&reader, root, root.get("fields"), schema, references);
            reader.custom(root);
            if let (true, Some(fields)) = (ok, fields) {
                definitions.insert(
                    (type_entry.name.clone(), id.to_owned()),
                    Definition {
                        fields,
                        canonical: reader.canonical(root),
                    },
                );
            }
        }
    }
    definitions
}
