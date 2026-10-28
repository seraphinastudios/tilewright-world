// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! `tilewright tx`: preview, rebase and apply transactions
//! (`docs/spec/TRANSACTIONS.md`).

use super::{Failure, USAGE, diagnostics_text, field, hex, json_string};
use std::{ffi::OsStr, fmt::Write as _, io::Read as _, path::Path};
use tilewright_edit::{FileChange, Options, Plan, Transaction, digest};

pub(super) fn run(arguments: &[&OsStr]) -> Result<String, Failure> {
    match arguments {
        [command, transaction, root] if *command == "preview" => {
            preview(Path::new(transaction), Path::new(root), false)
        }
        [command, flag, transaction, root] if *command == "preview" && *flag == "--json" => {
            preview(Path::new(transaction), Path::new(root), true)
        }
        [command, transaction, root, output] if *command == "rebase" => {
            rebase(Path::new(transaction), Path::new(root), Path::new(output))
        }
        [command, transaction, root, inverse] if *command == "apply" => {
            apply(Path::new(transaction), Path::new(root), Path::new(inverse))
        }
        _ => Err(Failure::message(USAGE)),
    }
}

fn shown(path: &Path) -> String {
    field(&path.display().to_string())
}

/// Reads a transaction file of at most the format's 64 MiB.
fn read(path: &Path, json: bool) -> Result<Transaction, Failure> {
    let limit =
        u64::try_from(tilewright_jsonc::Limits::default().max_input_bytes).unwrap_or(u64::MAX);
    let cannot = |error: &dyn std::fmt::Display| {
        Failure::message(format!("{}: {}", shown(path), field(&error.to_string())))
    };
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(limit + 1).read_to_end(&mut bytes))
        .map_err(|error| cannot(&error))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > limit {
        return Err(cannot(&format!("larger than {limit} bytes")));
    }
    let text = String::from_utf8(bytes).map_err(|_| cannot(&"not UTF-8"))?;
    let name = path.display().to_string();
    Transaction::parse(&name, text).map_err(|problems| failed(&problems, json, path))
}

fn failed(problems: &[tilewright_source::Diagnostic], json: bool, path: &Path) -> Failure {
    Failure {
        output: diagnostics_text(problems, json),
        message: format!("{} problem(s) in {}", problems.len(), shown(path)),
    }
}

/// Plans `transaction` and checks that it could be undone and redone
/// (`Plan::check_undo`): `apply` writes the inverse as the way back, so it
/// must apply, and `preview` and `rebase` refuse what `apply` would. An
/// inverse is checked like any transaction; one `apply` wrote passes.
fn plan(transaction: &Transaction, root: &Path, rebase: bool, json: bool) -> Result<Plan, Failure> {
    let options = Options {
        rebase,
        ..Options::default()
    };
    let refused = |problems: Vec<tilewright_source::Diagnostic>| {
        failed(&problems, json, Path::new(transaction.name()))
    };
    let plan = tilewright_edit::plan(root, transaction, &options).map_err(refused)?;
    plan.check_undo().map_err(refused)?;
    Ok(plan)
}

fn change(file: &FileChange) -> &'static str {
    match (&file.before, &file.after) {
        (None, Some(_)) => "created",
        (Some(_), None) => "deleted",
        _ => "modified",
    }
}

/// One line per changed file: `A`, `M` or `D` and the path.
fn changes(plan: &Plan) -> String {
    let mut output = String::new();
    for file in plan.files().iter().filter(|file| file.is_changed()) {
        let mark = match change(file) {
            "created" => 'A',
            "deleted" => 'D',
            _ => 'M',
        };
        let _ = writeln!(output, "{mark} {}", field(&file.path));
    }
    output
}

fn preview(transaction: &Path, root: &Path, json: bool) -> Result<String, Failure> {
    let plan = plan(&read(transaction, json)?, root, false, json)?;
    let source = hex(&plan.world().source_digest());
    if !json {
        return Ok(format!("{}valid: source {source}\n", changes(&plan)));
    }
    let digest_text = |text: Option<&str>| {
        digest(text).map_or_else(
            || "null".to_owned(),
            |digest| format!("\"{}\"", hex(&digest)),
        )
    };
    let mut output = String::from("{\"files\":[");
    for (index, file) in plan
        .files()
        .iter()
        .filter(|file| file.is_changed())
        .enumerate()
    {
        if index > 0 {
            output.push(',');
        }
        output.push_str("{\"path\":");
        json_string(&file.path, &mut output);
        let _ = write!(
            output,
            ",\"change\":\"{}\",\"base\":{},\"result\":{}}}",
            change(file),
            digest_text(file.before.as_deref()),
            digest_text(file.after.as_deref())
        );
    }
    let _ = writeln!(output, "],\"source_digest\":\"{source}\"}}");
    Ok(output)
}

/// Refuses an output inside the source tree's documents, where it would
/// itself become a stray source file, or at the name `world.jsonc` is
/// staged in, which publishing replaces.
fn outside(root: &Path, path: &Path) -> Result<(), Failure> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let (Ok(root), Ok(parent), Some(name)) =
        (root.canonicalize(), parent.canonicalize(), path.file_name())
    else {
        return Ok(());
    };
    let documents = ["planes", "defs", "profile"].map(std::ffi::OsStr::new);
    let staging = tilewright_edit::staging_name("world.jsonc");
    let inside = parent.join(name).strip_prefix(&root).is_ok_and(|relative| {
        relative == Path::new("world.jsonc")
            || relative == Path::new(&staging)
            || relative
                .components()
                .next()
                .is_some_and(|first| documents.contains(&first.as_os_str()))
    });
    if inside {
        return Err(Failure::message(format!(
            "{} is inside the source tree's documents; write it elsewhere",
            shown(path)
        )));
    }
    Ok(())
}

/// Writes a new file; returns whether its name is durable.
fn write(path: &Path, text: &str) -> Result<bool, Failure> {
    tilewright_edit::write_new(path, text).map_err(|error| {
        Failure::message(format!(
            "cannot write {}: {}",
            shown(path),
            field(&error.to_string())
        ))
    })
}

fn rebase(transaction: &Path, root: &Path, output: &Path) -> Result<String, Failure> {
    outside(root, output)?;
    let transaction = read(transaction, false)?;
    let plan = plan(&transaction, root, true, false)?;
    let durable = if write(output, plan.sealed())? {
        ""
    } else {
        " (its name could not be synced, so it may not be durable yet)"
    };
    Ok(format!(
        "{}rebased: {} operation(s) on {} file(s), written to {}{durable}\n",
        changes(&plan),
        transaction.len(),
        plan.files().len(),
        shown(output)
    ))
}

fn apply(transaction: &Path, root: &Path, inverse: &Path) -> Result<String, Failure> {
    if !tilewright_edit::PUBLISH_SUPPORTED {
        return Err(Failure::message(
            "tx apply is supported only on Linux, where files are reached through directory handles and checked for access control lists; tx preview and tx rebase work here",
        ));
    }
    outside(root, inverse)?;
    let plan = plan(&read(transaction, false)?, root, false, false)?;
    // The inverse is durable before any file changes, so an interrupted
    // apply can be undone with it, where the platform can make it so.
    let inverse_durable = write(inverse, plan.inverse())?;
    let unsynced_inverse = if inverse_durable {
        ""
    } else {
        "; the inverse's name could not be synced, so it may not be durable yet"
    };
    let published = plan.publish().map_err(|error| Failure {
        output: diagnostics_text(&error.problems, false),
        message: format!(
            "{}; {} problem(s){unsynced_inverse}",
            error.summary(),
            error.problems.len()
        ),
    })?;
    let durable = match (inverse_durable, published.durable) {
        (true, true) => "",
        (false, true) => " (the inverse's name could not be synced, so it may not be durable yet)",
        (true, false) => " (the renames could not be synced, so they may not be durable yet)",
        (false, false) => {
            " (the inverse's name and the renames could not be synced, so they may not be durable yet)"
        }
    };
    Ok(format!(
        "{}applied; the inverse is in {}{durable}\n",
        changes(&plan),
        shown(inverse)
    ))
}

#[cfg(test)]
mod tests {
    use std::{ffi::OsStr, fs, path::Path};

    fn run(arguments: &[&str]) -> Result<String, super::Failure> {
        let arguments: Vec<&OsStr> = arguments.iter().map(OsStr::new).collect();
        crate::run(&arguments)
    }

    fn copy(from: &Path, to: &Path) {
        fs::create_dir_all(to).unwrap();
        for entry in fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    /// A copy of the fixture and a transaction beside it.
    fn setup() -> (tempfile::TempDir, String) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("source");
        copy(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/basic"),
            &root,
        );
        fs::write(
            directory.path().join("tx.jsonc"),
            r#"{
  "format": "tilewright.transaction/1",
  "title": "Plant a tree",
  "operations": [
    // Chunks are found from the tile.
    {"op": "object.insert", "plane": "overworld", "layer": 0, "at": [40, 2], "object": {"proto": "tree"}}
  ]
}
"#,
        )
        .unwrap();
        let root = root.display().to_string();
        (directory, root)
    }

    #[test]
    fn preview_and_rebase_write_nothing_but_their_output() {
        let (directory, root) = setup();
        let path = |name: &str| directory.path().join(name).display().to_string();
        let layer = Path::new(&root).join("planes/overworld/layers/0");
        let before = fs::read_dir(&layer).unwrap().count();
        let preview = run(&["tx", "preview", &path("tx.jsonc"), &root]).unwrap();
        assert!(
            preview.starts_with("A planes/overworld/layers/0/1_0.jsonc\nvalid: source "),
            "{preview}"
        );
        let json = run(&["tx", "preview", "--json", &path("tx.jsonc"), &root]).unwrap();
        let parsed = tilewright_jsonc::Document::parse(
            json.trim_end(),
            &tilewright_jsonc::Limits::default(),
        )
        .unwrap();
        let files = parsed.root().get("files").unwrap().as_array().unwrap();
        assert_eq!(files[0].get("change").unwrap().as_str(), Some("created"));
        assert_eq!(files[0].get("base").unwrap().kind(), "null");
        assert_eq!(fs::read_dir(&layer).unwrap().count(), before);

        let rebased = run(&[
            "tx",
            "rebase",
            &path("tx.jsonc"),
            &root,
            &path("sealed.jsonc"),
        ])
        .unwrap();
        assert!(
            rebased.contains("rebased: 1 operation(s) on 1 file(s)"),
            "{rebased}"
        );
        let sealed = fs::read_to_string(path("sealed.jsonc")).unwrap();
        assert!(sealed.contains("// Chunks are found from the tile."));
        assert!(
            sealed
                .contains("{\"path\": \"planes/overworld/layers/0/1_0.jsonc\", \"sha256\": null}")
        );
        let again = run(&[
            "tx",
            "rebase",
            &path("tx.jsonc"),
            &root,
            &path("sealed.jsonc"),
        ]);
        assert!(again.unwrap_err().message.contains("cannot write"));
        assert_eq!(fs::read_dir(&layer).unwrap().count(), before);

        fs::write(
            path("bad.jsonc"),
            "{\"format\": \"tilewright.transaction/1\"}",
        )
        .unwrap();
        let bad = run(&["tx", "preview", &path("bad.jsonc"), &root]).unwrap_err();
        assert!(
            bad.output.contains("tx.shape: missing key \"operations\""),
            "{}",
            bad.output
        );
        for arguments in [
            &["tx"][..],
            &["tx", "preview"],
            &["tx", "apply", "a", "b"],
            &["tx", "frob", "a", "b"],
        ] {
            assert!(run(arguments).unwrap_err().message.starts_with("usage:"));
        }
    }

    #[test]
    fn a_transaction_that_could_not_be_undone_is_refused_before_anything_is_written() {
        let (directory, root) = setup();
        let path = |name: &str| directory.path().join(name).display().to_string();
        // A thin stroke over 479 chunks, painting in each 32 values the
        // palette lacks. Its inverse removes each entry it adds in 8 lines;
        // the inverse's own inverse, built when the inverse is planned,
        // puts each back in 16: past the 250,000 lines of a document, so
        // the undo would be refused.
        let defs = Path::new(&root).join("defs/proto");
        for value in 0..32 {
            fs::write(
                defs.join(format!("p{value:02}.jsonc")),
                format!(
                    "{{\"format\": \"tilewright.def/1\", \"type\": \"proto\", \"id\": \"p{value:02}\", \"fields\": {{\"face\": \"p.png\", \"layer\": \"floor\"}}}}\n"
                ),
            )
            .unwrap();
        }
        let layer = Path::new(&root).join("planes/overworld/layers/2");
        fs::create_dir_all(&layer).unwrap();
        let rows = vec![format!("        \"{}\"", "g".repeat(32)); 32].join(",\n");
        let mut cells = Vec::new();
        for cx in 0..479 {
            fs::write(
                layer.join(format!("{cx}_0.jsonc")),
                format!(
                    "{{\n  \"format\": \"tilewright.chunk/1\",\n  \"plane\": \"overworld\",\n  \"layer\": 2,\n  \"chunk\": [{cx}, 0],\n  \"terrain\": {{\n    \"floor\": {{\n      \"palette\": {{\"g\": \"grass\"}},\n      \"rows\": [\n{rows}\n      ]\n    }}\n  }}\n}}\n"
                ),
            )
            .unwrap();
            for value in 0..32 {
                cells.push(format!(
                    "{{\"at\": [{}, 0], \"value\": \"p{value:02}\"}}",
                    cx * 32 + value
                ));
            }
        }
        fs::write(
            path("stroke.jsonc"),
            format!(
                "{{\n  \"format\": \"tilewright.transaction/1\",\n  \"operations\": [\n    {{\"op\": \"terrain.set\", \"plane\": \"overworld\", \"layer\": 2, \"grid\": \"floor\", \"cells\": [\n{}\n    ]}}\n  ]\n}}\n",
                cells.join(",\n")
            ),
        )
        .unwrap();
        let chunk = layer.join("0_0.jsonc");
        let before = fs::read(&chunk).unwrap();
        let refused = |failure: super::Failure| {
            assert!(
                failure.output.contains(
                    ": tx.limit: the transaction could not be undone: its inverse would be refused: its own inverse would exceed the limits of a transaction: invalid JSONC (jsonc.too_many_lines); split the transaction"
                ),
                "{}",
                failure.output
            );
        };
        refused(run(&["tx", "preview", &path("stroke.jsonc"), &root]).unwrap_err());
        // Publishing is supported on Linux only.
        #[cfg(target_os = "linux")]
        {
            refused(
                run(&[
                    "tx",
                    "apply",
                    &path("stroke.jsonc"),
                    &root,
                    &path("undo.jsonc"),
                ])
                .unwrap_err(),
            );
            assert!(!Path::new(&path("undo.jsonc")).exists());
        }
        assert_eq!(fs::read(&chunk).unwrap(), before);
    }

    // Publishing is supported on Linux only.
    #[cfg(target_os = "linux")]
    #[test]
    fn apply_writes_the_inverse_and_the_inverse_undoes_it() {
        let (directory, root) = setup();
        let path = |name: &str| directory.path().join(name).display().to_string();
        let chunk = Path::new(&root).join("planes/overworld/layers/0/1_0.jsonc");
        run(&[
            "tx",
            "rebase",
            &path("tx.jsonc"),
            &root,
            &path("sealed.jsonc"),
        ])
        .unwrap();
        let applied = run(&[
            "tx",
            "apply",
            &path("sealed.jsonc"),
            &root,
            &path("undo.jsonc"),
        ])
        .unwrap();
        assert!(applied.contains("applied; the inverse is in"), "{applied}");
        assert!(chunk.exists());

        // Its base no longer holds, so applying it again is a conflict, and
        // the inverse is not written.
        let conflict = run(&[
            "tx",
            "apply",
            &path("sealed.jsonc"),
            &root,
            &path("undo2.jsonc"),
        ])
        .unwrap_err();
        assert!(
            conflict.output.contains("tx.conflict"),
            "{}",
            conflict.output
        );
        assert!(!Path::new(&path("undo2.jsonc")).exists());

        // Neither inside the documents nor where world.jsonc is staged.
        for inside in ["planes/overworld/undo.jsonc", ".world.jsonc.tilewright-tmp"] {
            let inside = Path::new(&root).join(inside);
            let inside_text = inside.display().to_string();
            let refused =
                run(&["tx", "apply", &path("undo.jsonc"), &root, &inside_text]).unwrap_err();
            assert!(
                refused.message.contains("inside the source tree"),
                "{}",
                refused.message
            );
            assert!(!inside.exists());
        }

        run(&[
            "tx",
            "apply",
            &path("undo.jsonc"),
            &root,
            &path("redo.jsonc"),
        ])
        .unwrap();
        assert!(!chunk.exists());
        assert!(run(&["validate", &root]).unwrap().starts_with("valid: "));
    }
}
