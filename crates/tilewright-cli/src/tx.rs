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
        // The flag with an argument missing is a usage error, not a path.
        [command, transaction, root, inverse]
            if *command == "apply" && *transaction != "--unchecked-access" =>
        {
            apply(
                Path::new(transaction),
                Path::new(root),
                Path::new(inverse),
                false,
            )
        }
        [command, flag, transaction, root, inverse]
            if *command == "apply" && *flag == "--unchecked-access" =>
        {
            apply(
                Path::new(transaction),
                Path::new(root),
                Path::new(inverse),
                true,
            )
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

fn plan(transaction: &Transaction, root: &Path, rebase: bool, json: bool) -> Result<Plan, Failure> {
    let options = Options {
        rebase,
        ..Options::default()
    };
    tilewright_edit::plan(root, transaction, &options)
        .map_err(|problems| failed(&problems, json, Path::new(transaction.name())))
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
    // Compared as macOS and Windows compare names by default: without
    // case, and, on Windows, without trailing dots and spaces.
    let folded = |path: &Path| -> Vec<String> {
        path.components()
            .map(|component| {
                component
                    .as_os_str()
                    .to_string_lossy()
                    .trim_end_matches(['.', ' '])
                    .to_lowercase()
            })
            .collect()
    };
    let (root, target) = (folded(&root), folded(&parent.join(name)));
    let reserved = [
        "world.jsonc".to_owned(),
        tilewright_edit::staging_name("world.jsonc"),
        tilewright_source::LOCK_NAME.to_owned(),
    ];
    // Within the tree, a name that is not ASCII is refused too: a file
    // system may fold it into a reserved one by rules of its own.
    let inside = target
        .strip_prefix(root.as_slice())
        .is_some_and(|relative| {
            matches!(relative, [only] if reserved.contains(only))
                || relative
                    .first()
                    .is_some_and(|first| ["planes", "defs", "profile"].contains(&first.as_str()))
                || relative.iter().any(|component| !component.is_ascii())
                // On Windows a colon names a stream of a file, which goes
                // with the file when publishing replaces it.
                || relative.iter().any(|component| component.contains(':'))
        });
    if inside {
        return Err(Failure::message(format!(
            "{} is inside the source tree where publishing writes, or may be taken for such a name; write it elsewhere",
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

/// Whether `tx apply` may publish here: where the platform does not check
/// everything, only with `--unchecked-access`. Returns the warning to give
/// when it was accepted.
fn consent(unchecked_access: bool) -> Result<Option<String>, Failure> {
    match tilewright_edit::UNCHECKED_ACCESS {
        Some(unchecked) if !unchecked_access => Err(Failure::message(format!(
            "tx apply here does not check everything: {unchecked}; pass --unchecked-access to accept that (SECURITY.md says what it guarantees on each platform); nothing was written"
        ))),
        Some(unchecked) => Ok(Some(format!(
            "warning: with --unchecked-access, tx apply does not check everything here: {unchecked}"
        ))),
        None => Ok(None),
    }
}

fn apply(
    transaction: &Path,
    root: &Path,
    inverse: &Path,
    unchecked_access: bool,
) -> Result<String, Failure> {
    if !tilewright_edit::PUBLISH_SUPPORTED {
        return Err(Failure::message(
            "tx apply is supported only on Linux, macOS and Windows (SECURITY.md says what it guarantees on each); tx preview and tx rebase work here",
        ));
    }
    // Refused before anything is written, the inverse included.
    let warning = consent(unchecked_access)?;
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
    if let Some(warning) = warning {
        eprintln!("tilewright: {warning}");
    }
    let options = tilewright_edit::PublishOptions { unchecked_access };
    let published = plan.publish_with(&options).map_err(|error| Failure {
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
            &["tx", "apply", "--unchecked-access", "a", "b"],
            &["tx", "apply", "--unchecked", "a", "b", "c"],
            &["tx", "frob", "a", "b"],
        ] {
            assert!(run(arguments).unwrap_err().message.starts_with("usage:"));
        }
    }

    // Publishing is supported on Linux, macOS and Windows.
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn apply_needs_consent_where_access_is_unchecked() {
        let (directory, root) = setup();
        let path = |name: &str| directory.path().join(name).display().to_string();
        let chunk = Path::new(&root).join("planes/overworld/layers/0/1_0.jsonc");
        let applied = run(&["tx", "apply", &path("tx.jsonc"), &root, &path("undo.jsonc")]);
        // Only Linux checks everything, and needs no consent.
        let Some(unchecked) = tilewright_edit::UNCHECKED_ACCESS else {
            assert!(super::consent(false).unwrap().is_none());
            assert!(super::consent(true).unwrap().is_none());
            assert!(applied.unwrap().contains("applied; the inverse is in"));
            assert!(chunk.exists());
            return;
        };
        let refused = applied.unwrap_err();
        assert!(
            refused.message.contains(unchecked) && refused.message.contains("--unchecked-access"),
            "{}",
            refused.message
        );
        // Neither the inverse nor the tree was written.
        assert!(!Path::new(&path("undo.jsonc")).exists());
        assert!(!chunk.exists());
        let warning = super::consent(true).unwrap().unwrap();
        assert!(warning.starts_with("warning: ") && warning.contains(unchecked));
        run(&[
            "tx",
            "apply",
            "--unchecked-access",
            &path("tx.jsonc"),
            &root,
            &path("undo.jsonc"),
        ])
        .unwrap();
        assert!(chunk.exists());
    }

    // Publishing is supported on Linux, macOS and Windows.
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
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
            "--unchecked-access",
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
            "--unchecked-access",
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
        for inside in [
            "planes/overworld/undo.jsonc",
            ".world.jsonc.tilewright-tmp",
            ".WORLD.JSONC.tilewright-tmp",
            ".world.jsonc.tilewright-tmp.",
            ".world.j\u{17f}onc.tilewright-tmp",
            "world.jsonc:undo",
            ".tilewright.lock",
        ] {
            let inside = Path::new(&root).join(inside);
            let inside_text = inside.display().to_string();
            let refused = run(&[
                "tx",
                "apply",
                "--unchecked-access",
                &path("undo.jsonc"),
                &root,
                &inside_text,
            ])
            .unwrap_err();
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
            "--unchecked-access",
            &path("undo.jsonc"),
            &root,
            &path("redo.jsonc"),
        ])
        .unwrap();
        assert!(!chunk.exists());
        assert!(run(&["validate", &root]).unwrap().starts_with("valid: "));
    }
}
