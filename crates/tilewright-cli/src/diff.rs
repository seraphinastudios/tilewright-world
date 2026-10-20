// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! `tilewright diff`: compares the IDs of two worlds (`docs/spec/DIFF.md`).
//! Like diff(1), it exits 0 when nothing breaking changed, 1 when an ID was
//! removed or renamed without the allowlist accepting it, and 2 when a side,
//! the allowlist or the arguments could not be read.

use super::{USAGE, diagnostics_text, field, terminal_safe};
use std::{ffi::OsStr, fmt::Write as _, path::Path, process::ExitCode};
use tilewright_diff::{Allowlist, Error, Snapshot};

/// No ID was removed or renamed without the allowlist accepting it.
const SAME: u8 = 0;
/// An ID was removed or renamed without the allowlist accepting it.
const BREAKING: u8 = 1;
/// A side, the allowlist or the arguments could not be read.
const TROUBLE: u8 = 2;

/// What a run prints, and its exit code.
#[derive(Debug)]
pub struct Outcome {
    pub stdout: String,
    pub stderr: String,
    pub code: u8,
}

impl Outcome {
    fn trouble(stderr: &str) -> Self {
        Self {
            stdout: String::new(),
            stderr: terminal_safe(stderr),
            code: TROUBLE,
        }
    }
}

pub fn main(arguments: &[&OsStr]) -> ExitCode {
    let outcome = run(arguments);
    print!("{}", outcome.stdout);
    eprint!("{}", outcome.stderr);
    ExitCode::from(outcome.code)
}

struct Options<'a> {
    json: bool,
    allow: Option<&'a OsStr>,
    old: &'a OsStr,
    new: &'a OsStr,
}

/// `[--json] [--allow ALLOWLIST] OLD NEW`: each flag at most once, in any
/// order, then the two sides, neither of which may look like a flag.
fn options<'a>(arguments: &[&'a OsStr]) -> Option<Options<'a>> {
    let (mut json, mut allow) = (false, None);
    let mut rest = arguments;
    loop {
        match rest {
            [flag, tail @ ..] if *flag == "--json" && !json => {
                json = true;
                rest = tail;
            }
            [flag, path, tail @ ..] if *flag == "--allow" && allow.is_none() => {
                allow = Some(*path);
                rest = tail;
            }
            [old, new] => {
                let flag = |side: &OsStr| side.as_encoded_bytes().starts_with(b"--");
                return (!flag(old) && !flag(new)).then_some(Options {
                    json,
                    allow,
                    old,
                    new,
                });
            }
            _ => return None,
        }
    }
}

/// Runs `tilewright diff` with the arguments after `diff`. The text report
/// and every message are made terminal safe; the JSON report escapes
/// control characters itself.
pub fn run(arguments: &[&OsStr]) -> Outcome {
    let Some(options) = options(arguments) else {
        return Outcome::trouble(&format!("tilewright: {USAGE}\n"));
    };
    let allowlist = match options.allow {
        None => Allowlist::default(),
        Some(path) => match Allowlist::read(Path::new(path)) {
            Ok(allowlist) => allowlist,
            Err(error) => {
                let path = field(&Path::new(path).display().to_string());
                let message = field(&error.message);
                return Outcome::trouble(&if error.line == 0 {
                    format!("tilewright: allowlist {path}: {}: {message}\n", error.code)
                } else {
                    format!(
                        "tilewright: allowlist {path}:{}:{}: {}: {message}\n",
                        error.line, error.column, error.code
                    )
                });
            }
        },
    };
    let mut stderr = String::new();
    let old = side("old", options.old, &mut stderr);
    let new = side("new", options.new, &mut stderr);
    let (Some(old), Some(new)) = (old, new) else {
        return Outcome::trouble(&stderr);
    };
    let report = tilewright_diff::diff(&old, &new, &allowlist);
    Outcome {
        stdout: if options.json {
            report.to_json()
        } else {
            terminal_safe(&report.to_text())
        },
        stderr: String::new(),
        code: if report.breaking() { BREAKING } else { SAME },
    }
}

/// Loads one side, or appends why it did not load to `stderr`: each source
/// diagnostic on its own line, prefixed with the side's name, then a
/// summary.
fn side(name: &str, path: &OsStr, stderr: &mut String) -> Option<Snapshot> {
    let path = Path::new(path);
    let error = match tilewright_diff::load(path) {
        Ok(snapshot) => return Some(snapshot),
        Err(error) => error,
    };
    let shown = field(&path.display().to_string());
    match error {
        Error::Source(diagnostics) => {
            for line in diagnostics_text(&diagnostics, false).lines() {
                let _ = writeln!(stderr, "{name}: {line}");
            }
            let _ = writeln!(
                stderr,
                "tilewright: {name} {shown}: {} problem(s)",
                diagnostics.len()
            );
        }
        other => {
            let _ = writeln!(
                stderr,
                "tilewright: {name} {shown}: {}",
                field(&other.to_string())
            );
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{BREAKING, Outcome, SAME, TROUBLE};
    use std::{ffi::OsStr, path::Path};

    fn run(arguments: &[&str]) -> Outcome {
        let arguments: Vec<&OsStr> = arguments.iter().map(OsStr::new).collect();
        super::run(&arguments)
    }

    fn fixture() -> String {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/basic")
            .display()
            .to_string()
    }

    /// A copy of the fixture with one stairwell renamed, and its root.
    fn renamed() -> (tempfile::TempDir, String) {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("world");
        let mut pending = vec![(Path::new(&fixture()).to_path_buf(), root.clone())];
        while let Some((from, to)) = pending.pop() {
            std::fs::create_dir_all(&to).unwrap();
            for entry in std::fs::read_dir(from).unwrap() {
                let entry = entry.unwrap();
                let target = to.join(entry.file_name());
                if entry.file_type().unwrap().is_dir() {
                    pending.push((entry.path(), target));
                } else {
                    std::fs::copy(entry.path(), target).unwrap();
                }
            }
        }
        let stairwells = root.join("planes/overworld/stairwells.jsonc");
        let text = std::fs::read_to_string(&stairwells).unwrap();
        std::fs::write(
            &stairwells,
            text.replace("\"cave.chute\"", "\"cave.slide\""),
        )
        .unwrap();
        let root = root.display().to_string();
        (directory, root)
    }

    #[test]
    fn exit_codes_follow_diff() {
        let fixture = fixture();
        let same = run(&[&fixture, &fixture]);
        assert_eq!(
            (same.code, same.stdout.as_str(), same.stderr.as_str()),
            (
                SAME,
                "summary: 0 removed, 0 renamed, 0 added, 0 not allowed\n",
                ""
            )
        );

        let (directory, root) = renamed();
        let changed = run(&[&fixture, &root]);
        assert_eq!(changed.code, BREAKING);
        assert_eq!(
            changed.stdout,
            "renamed stairwell overworld/cave.chute -> overworld/cave.slide (same content)\nsummary: 0 removed, 1 renamed, 0 added, 1 not allowed\n"
        );
        let json = run(&["--json", &fixture, &root]);
        assert_eq!(json.code, BREAKING);
        assert!(json.stdout.contains("\"renamed\":[{\"kind\":\"stairwell\",\"from\":\"overworld/cave.chute\",\"to\":\"overworld/cave.slide\",\"basis\":\"content\",\"allowed\":false}]"), "{}", json.stdout);
        tilewright_jsonc::Document::parse(
            json.stdout.trim_end(),
            &tilewright_jsonc::Limits::default(),
        )
        .expect("the report is JSON");

        let allowlist = directory.path().join("allow.jsonc");
        std::fs::write(
            &allowlist,
            r#"{"format": "tilewright.diff-allow/1", "rename": [{"kind": "stairwell", "from": "overworld/cave.chute", "to": "overworld/cave.slide"}]}"#,
        )
        .unwrap();
        let allowlist = allowlist.display().to_string();
        for arguments in [
            ["--allow", &allowlist, "--json", &fixture, &root],
            ["--json", "--allow", &allowlist, &fixture, &root],
        ] {
            let accepted = run(&arguments);
            assert_eq!(accepted.code, SAME, "{}", accepted.stderr);
            assert!(accepted.stdout.contains("\"breaking\":false"));
        }
    }

    #[test]
    fn unreadable_sides_and_allowlists_exit_2() {
        let directory = tempfile::tempdir().unwrap();
        let fixture = fixture();
        let missing = directory.path().join("missing").display().to_string();
        let outcome = run(&[&missing, &missing]);
        assert_eq!((outcome.code, outcome.stdout.as_str()), (TROUBLE, ""));
        assert_eq!(
            outcome.stderr,
            format!(
                "old: world.jsonc: source.missing_file: file is missing\ntilewright: old {missing}: 1 problem(s)\nnew: world.jsonc: source.missing_file: file is missing\ntilewright: new {missing}: 1 problem(s)\n"
            )
        );

        let garbage = directory.path().join("garbage.twpack");
        std::fs::write(&garbage, "not a pack").unwrap();
        let garbage = garbage.display().to_string();
        let outcome = run(&["--json", &fixture, &garbage]);
        assert_eq!((outcome.code, outcome.stdout.as_str()), (TROUBLE, ""));
        assert!(
            outcome
                .stderr
                .starts_with(&format!("tilewright: new {garbage}: ")),
            "{}",
            outcome.stderr
        );

        let allowlist = directory.path().join("allow.jsonc");
        std::fs::write(
            &allowlist,
            "{\n  \"format\": \"tilewright.diff-allow/2\"\n}",
        )
        .unwrap();
        let allowlist = allowlist.display().to_string();
        let outcome = run(&["--allow", &allowlist, &fixture, &fixture]);
        assert_eq!(outcome.code, TROUBLE);
        assert_eq!(
            outcome.stderr,
            format!(
                "tilewright: allowlist {allowlist}:2:13: shape.format: format must be \"tilewright.diff-allow/1\"\n"
            )
        );
        let outcome = run(&["--allow", &missing, &fixture, &fixture]);
        assert_eq!(outcome.code, TROUBLE);
        assert!(
            outcome
                .stderr
                .starts_with(&format!("tilewright: allowlist {missing}: source.io: ")),
            "{}",
            outcome.stderr
        );
    }

    #[test]
    fn malformed_arguments_print_usage_and_exit_2() {
        let fixture = fixture();
        for arguments in [
            &[][..],
            &[fixture.as_str()],
            &[&fixture, &fixture, &fixture],
            &["--json", "--json", &fixture, &fixture],
            &[&fixture, &fixture, "--json"],
            &[&fixture, "--json"],
            &["--allow", &fixture],
            &["--allow", "a", "--allow", "b", &fixture, &fixture],
            &["--verbose", &fixture, &fixture],
        ] {
            let outcome = run(arguments);
            assert_eq!(outcome.code, TROUBLE, "{arguments:?}");
            assert!(outcome.stdout.is_empty());
            assert!(
                outcome.stderr.starts_with("tilewright: usage:"),
                "{arguments:?}"
            );
        }
    }
}
