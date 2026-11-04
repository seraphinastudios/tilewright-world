// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! Definitions: `defs/<type>/<id>.jsonc`.

use crate::diagnostic::Diagnostics;
use crate::{
    Code, Definition, Diagnostic, Profile,
    files::{EntryKind, Tree, ignored, join},
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
    if !tree.container("defs") {
        return definitions;
    }
    // A definition's key comes from its path, so the whole layout is listed
    // before any document is read: references between definitions then
    // resolve as they are read.
    let mut files = Vec::new();
    for type_entry in tree.list("defs") {
        let directory = join("defs", &type_entry.name);
        let Some(schema) = profile
            .definition_types
            .get(&type_entry.name)
            .filter(|_| type_entry.kind == EntryKind::Directory)
        else {
            if let Err(why) = ignored(tree, &directory, &type_entry, false) {
                diagnostics.push(Diagnostic::file(
                    &directory,
                    Code::UnexpectedFile,
                    format!("not a definition type declared by the profile{why}"),
                ));
            }
            continue;
        };
        // A type's directory that holds no file is counted apart, as one
        // the profile does not declare is (`Tree::set_aside`).
        let (entries, other) = tree.listing(&directory, true);
        let mut held = other.is_some();
        for entry in entries {
            let path = join(&directory, &entry.name);
            let Some(id) = entry
                .name
                .strip_suffix(".jsonc")
                .filter(|id| is_identifier(id) && entry.kind == EntryKind::File)
            else {
                if let Err(why) = ignored(tree, &path, &entry, true) {
                    held = true;
                    diagnostics.push(Diagnostic::file(
                        &path,
                        Code::UnexpectedFile,
                        format!("definition files are named <id>.jsonc{why}"),
                    ));
                }
                continue;
            };
            held = true;
            files.push((type_entry.name.clone(), id.to_owned(), path, schema));
        }
        if held {
            tree.hold(&directory);
        } else {
            tree.set_aside(&directory);
        }
    }
    references.definitions_listed(
        files
            .iter()
            .map(|(kind, id, _, _)| (kind.clone(), id.clone())),
    );
    for (kind, id, path, schema) in files {
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
        for (key, expected) in [("type", kind.as_str()), ("id", id.as_str())] {
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
                (kind, id),
                Definition {
                    fields,
                    canonical: reader.canonical(root),
                },
            );
        }
    }
    definitions
}
