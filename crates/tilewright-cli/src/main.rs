// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! `tilewright`: validate world sources, compile packs and inspect them.

use std::{
    ffi::{OsStr, OsString},
    fmt::Write as _,
    path::Path,
    process::ExitCode,
};
use tilewright_jsonc::write_json_string;
use tilewright_source::Diagnostic;

const USAGE: &str = "usage:
  tilewright validate [--json] ROOT
  tilewright compile ROOT OUTPUT_DIRECTORY
  tilewright inspect [--max-decoded-bytes BYTES] PACK
  tilewright --version

inspect verifies the pack and stops once its chunks decode to more than
BYTES (default 8589934592, 8 GiB).";

fn main() -> ExitCode {
    // Paths need not be UTF-8, so arguments are read as OS strings.
    let arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    let arguments: Vec<&OsStr> = arguments.iter().map(OsString::as_os_str).collect();
    match run(&arguments) {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(Failure { output, message }) => {
            print!("{output}");
            if !message.is_empty() {
                eprintln!("tilewright: {message}");
            }
            ExitCode::FAILURE
        }
    }
}

/// A failed command: what to print on stdout, and a message for stderr.
#[derive(Debug)]
struct Failure {
    output: String,
    message: String,
}

impl Failure {
    fn message(message: impl Into<String>) -> Self {
        Self {
            output: String::new(),
            message: message.into(),
        }
    }
}

/// An untrusted value for one field of line-oriented output: every control
/// character, newlines included, is escaped, so a value cannot add or forge
/// lines, such as a fake `verified:` line.
fn field(text: &str) -> String {
    text.chars().fold(
        String::with_capacity(text.len()),
        |mut output, character| {
            if character.is_control() {
                output.extend(character.escape_unicode());
            } else {
                output.push(character);
            }
            output
        },
    )
}

/// Text for a terminal: control characters other than newlines are
/// escaped. A backstop behind [`field`], which every untrusted value passes
/// through first.
fn terminal_safe(text: &str) -> String {
    text.chars().fold(
        String::with_capacity(text.len()),
        |mut output, character| {
            if character.is_control() && character != '\n' {
                output.extend(character.escape_unicode());
            } else {
                output.push(character);
            }
            output
        },
    )
}

/// Runs a command; everything it prints is made [`terminal_safe`].
fn run(arguments: &[&OsStr]) -> Result<String, Failure> {
    run_command(arguments)
        .map(|output| terminal_safe(&output))
        .map_err(|failure| Failure {
            output: terminal_safe(&failure.output),
            message: terminal_safe(&failure.message),
        })
}

fn run_command(arguments: &[&OsStr]) -> Result<String, Failure> {
    match arguments {
        [command] if *command == "--version" => {
            Ok(format!("tilewright {}\n", env!("CARGO_PKG_VERSION")))
        }
        [command, root] if *command == "validate" => validate(Path::new(root), false),
        [command, flag, root] if *command == "validate" && *flag == "--json" => {
            validate(Path::new(root), true)
        }
        [command, root, output] if *command == "compile" => {
            compile(Path::new(root), Path::new(output))
        }
        [command, pack] if *command == "inspect" => {
            inspect(Path::new(pack), tilewright_pack::VerifyLimits::default())
        }
        [command, flag, bytes, pack] if *command == "inspect" && *flag == "--max-decoded-bytes" => {
            let maximum_decoded_bytes = bytes
                .to_str()
                .and_then(|bytes| bytes.parse().ok())
                .ok_or_else(|| Failure::message(USAGE))?;
            inspect(
                Path::new(pack),
                tilewright_pack::VerifyLimits {
                    maximum_decoded_bytes,
                },
            )
        }
        _ => Err(Failure::message(USAGE)),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut output, byte| {
        let _ = write!(output, "{byte:02x}");
        output
    })
}

fn diagnostics_text(diagnostics: &[Diagnostic], json: bool) -> String {
    let mut output = String::new();
    if json {
        output.push('[');
        for (index, diagnostic) in diagnostics.iter().enumerate() {
            if index > 0 {
                output.push(',');
            }
            output.push_str("{\"path\":");
            write_json_string(&diagnostic.path, &mut output);
            let _ = write!(
                output,
                ",\"start\":{},\"end\":{},\"line\":{},\"column\":{},\"code\":",
                diagnostic.start, diagnostic.end, diagnostic.line, diagnostic.column
            );
            write_json_string(diagnostic.code, &mut output);
            output.push_str(",\"message\":");
            write_json_string(&diagnostic.message, &mut output);
            output.push('}');
        }
        output.push_str("]\n");
    } else {
        for diagnostic in diagnostics {
            let path = field(&diagnostic.path);
            let message = field(&diagnostic.message);
            let _ = if diagnostic.line == 0 {
                writeln!(output, "{path}: {}: {message}", diagnostic.code)
            } else {
                writeln!(
                    output,
                    "{path}:{}:{}: {}: {message}",
                    diagnostic.line, diagnostic.column, diagnostic.code
                )
            };
        }
    }
    output
}

fn load(root: &Path, json: bool) -> Result<tilewright_source::World, Failure> {
    tilewright_source::load(root).map_err(|diagnostics| Failure {
        output: diagnostics_text(&diagnostics, json),
        message: format!(
            "{} problem(s) in {}",
            diagnostics.len(),
            field(&root.display().to_string())
        ),
    })
}

fn validate(root: &Path, json: bool) -> Result<String, Failure> {
    let world = load(root, json)?;
    if json {
        return Ok("[]\n".to_owned());
    }
    let chunks: usize = world
        .planes()
        .values()
        .map(|plane| plane.chunks.len())
        .sum();
    Ok(format!(
        "valid: {} plane(s), {chunks} chunk(s), {} definition(s), source {}\n",
        world.planes().len(),
        world.definitions().len(),
        hex(&world.source_digest())
    ))
}

fn compile(root: &Path, output: &Path) -> Result<String, Failure> {
    let world = load(root, false)?;
    let written = tilewright_pack::write_pack(&world, output).map_err(|error| {
        Failure::message(format!("cannot write pack: {}", field(&error.to_string())))
    })?;
    Ok(format!(
        "{} {} bytes{}\n",
        field(&written.path.display().to_string()),
        written.bytes,
        if written.created {
            ""
        } else {
            " (already present)"
        }
    ))
}

fn inspect(path: &Path, limits: tilewright_pack::VerifyLimits) -> Result<String, Failure> {
    let fail = |error: tilewright_pack::Error| {
        Failure::message(format!(
            "{}: {}",
            field(&path.display().to_string()),
            field(&error.to_string())
        ))
    };
    let pack = tilewright_pack::Pack::open(path).map_err(fail)?;
    let summary = pack.verify_with(&limits).map_err(fail)?;
    let mut output = String::new();
    for key in [
        "format",
        "compiler",
        "profile_id",
        "profile_version",
        "source_digest",
    ] {
        let value = pack.meta(key).map_err(fail)?.unwrap_or_default();
        let _ = writeln!(output, "{key}: {}", field(&value));
    }
    let _ = writeln!(
        output,
        "digest: {}",
        hex(&pack.file_digest().map_err(fail)?)
    );
    let _ = writeln!(
        output,
        "planes: {}\nzones: {}\nanchors: {}\nchunks: {}\nobjects: {}\ndefinitions: {}\nstrings: {}\nverified: yes",
        summary.planes,
        summary.zones,
        summary.anchors,
        summary.chunks,
        summary.objects,
        summary.definitions,
        summary.strings
    );
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::Failure;
    use std::{ffi::OsStr, path::Path};

    fn run(arguments: &[&str]) -> Result<String, Failure> {
        let arguments: Vec<&OsStr> = arguments.iter().map(OsStr::new).collect();
        super::run(&arguments)
    }

    fn fixture() -> String {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/basic")
            .display()
            .to_string()
    }

    #[test]
    fn validate_compile_and_inspect_the_fixture() {
        let root = fixture();
        let summary = run(&["validate", &root]).unwrap();
        assert!(summary.starts_with("valid: 2 plane(s), 4 chunk(s), 11 definition(s)"));
        assert_eq!(run(&["validate", "--json", &root]).unwrap(), "[]\n");

        let output = tempfile::tempdir().unwrap();
        let directory = output.path().display().to_string();
        let compiled = run(&["compile", &root, &directory]).unwrap();
        let pack = compiled.split(' ').next().unwrap().to_owned();
        assert!(pack.ends_with(".twpack"));
        assert!(
            run(&["compile", &root, &directory])
                .unwrap()
                .contains("already present")
        );
        let inspected = run(&["inspect", &pack]).unwrap();
        assert!(inspected.contains("format: tilewright.pack/1"));
        assert!(inspected.contains("objects: 11"));
        assert!(inspected.contains("verified: yes"));
        assert!(
            run(&["inspect", "--max-decoded-bytes", "100000000", &pack])
                .unwrap()
                .contains("verified: yes")
        );
        let limited = run(&["inspect", "--max-decoded-bytes", "1000", &pack]).unwrap_err();
        assert!(
            limited.message.contains("verification limit"),
            "{}",
            limited.message
        );
        let bad = run(&["inspect", "--max-decoded-bytes", "lots", &pack]).unwrap_err();
        assert!(bad.message.starts_with("usage:"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn paths_need_not_be_utf8() {
        use std::os::unix::ffi::OsStrExt;

        fn copy_tree(from: &Path, to: &Path) {
            std::fs::create_dir_all(to).unwrap();
            for entry in std::fs::read_dir(from).unwrap() {
                let entry = entry.unwrap();
                let target = to.join(entry.file_name());
                if entry.file_type().unwrap().is_dir() {
                    copy_tree(&entry.path(), &target);
                } else {
                    std::fs::copy(entry.path(), target).unwrap();
                }
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join(OsStr::from_bytes(b"source-\xff"));
        copy_tree(Path::new(&fixture()), &root);
        let output = directory.path().join(OsStr::from_bytes(b"packs-\xfe"));
        let run = |arguments: &[&OsStr]| super::run(arguments).unwrap();
        assert!(run(&[OsStr::new("validate"), root.as_os_str()]).starts_with("valid: "));
        run(&[OsStr::new("compile"), root.as_os_str(), output.as_os_str()]);
        let pack = std::fs::read_dir(&output)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert!(run(&[OsStr::new("inspect"), pack.as_os_str()]).contains("verified: yes"));
        assert!(super::run(&[OsStr::from_bytes(b"\xff")]).is_err());
    }

    #[test]
    fn printed_text_escapes_control_characters() {
        assert_eq!(
            super::terminal_safe("a\u{1b}[2Jb\u{7}\n"),
            "a\\u{1b}[2Jb\\u{7}\n"
        );

        // A valid source can put control characters in its profile version,
        // which `inspect` prints, and in keys, which diagnostics quote.
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("source");
        let output = directory.path().join("packs");
        std::fs::create_dir_all(&root).unwrap();
        for entry in walk(Path::new(&fixture())) {
            let relative = entry.strip_prefix(fixture()).unwrap();
            let target = root.join(relative);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::copy(&entry, &target).unwrap();
        }
        let profile = root.join("profile/profile.jsonc");
        let text = std::fs::read_to_string(&profile).unwrap();
        std::fs::write(
            &profile,
            text.replace(
                "\"version\": \"1.0.0\"",
                "\"version\": \"1\\u001b]52;c;eA==\\u0007\"",
            ),
        )
        .unwrap();
        let root_text = root.display().to_string();
        let output_text = output.display().to_string();
        run(&["compile", &root_text, &output_text]).unwrap();
        let pack = std::fs::read_dir(&output)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let inspected = run(&["inspect", &pack.display().to_string()]).unwrap();
        assert!(!inspected.contains('\u{1b}') && !inspected.contains('\u{7}'));
        assert!(inspected.contains("profile_version: 1\\u{1b}]52;c;eA==\\u{7}"));

        let world = root.join("world.jsonc");
        let text = std::fs::read_to_string(&world).unwrap();
        std::fs::write(&world, text.replacen('{', "{\"\\u001b[31mx\": 1, ", 1)).unwrap();
        let failure = run(&["validate", &root_text]).unwrap_err();
        assert!(!failure.output.contains('\u{1b}'), "{}", failure.output);
        assert!(
            failure.output.contains("\\u{1b}[31mx"),
            "{}",
            failure.output
        );
    }

    #[test]
    fn untrusted_values_cannot_forge_lines() {
        assert_eq!(super::field("a\nb\u{1b}"), "a\\u{a}b\\u{1b}");

        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("source");
        let output = directory.path().join("packs");
        for entry in walk(Path::new(&fixture())) {
            let target = root.join(entry.strip_prefix(fixture()).unwrap());
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::copy(&entry, &target).unwrap();
        }
        // A profile version that would add a forged status line.
        let profile = root.join("profile/profile.jsonc");
        let text = std::fs::read_to_string(&profile).unwrap();
        std::fs::write(
            &profile,
            text.replace(
                "\"version\": \"1.0.0\"",
                "\"version\": \"x\\nverified: no\"",
            ),
        )
        .unwrap();
        let root_text = root.display().to_string();
        run(&["compile", &root_text, &output.display().to_string()]).unwrap();
        let pack = std::fs::read_dir(&output)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let inspected = run(&["inspect", &pack.display().to_string()]).unwrap();
        let lines: Vec<&str> = inspected.lines().collect();
        assert!(
            lines.contains(&"profile_version: x\\u{a}verified: no"),
            "{inspected}"
        );
        assert!(!lines.contains(&"verified: no"), "{inspected}");
        assert_eq!(lines.last(), Some(&"verified: yes"));

        // A file name with a newline cannot forge a diagnostic line.
        #[cfg(unix)]
        {
            std::fs::write(
                root.join("planes/overworld/x\nplanes:1:1: ref.unresolved: forged"),
                "",
            )
            .unwrap();
            let failure = run(&["validate", &root_text]).unwrap_err();
            assert!(
                failure
                    .output
                    .lines()
                    .all(|line| !line.starts_with("planes:1:1")),
                "{}",
                failure.output
            );
            assert!(
                failure
                    .output
                    .contains("x\\u{a}planes:1:1: ref.unresolved: forged"),
                "{}",
                failure.output
            );
        }
    }

    /// Every file under `root`.
    fn walk(root: &Path) -> Vec<std::path::PathBuf> {
        let mut files = Vec::new();
        let mut directories = vec![root.to_path_buf()];
        while let Some(directory) = directories.pop() {
            for entry in std::fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    directories.push(path);
                } else {
                    files.push(path);
                }
            }
        }
        files
    }

    #[test]
    fn invalid_sources_report_json_diagnostics() {
        let copy = tempfile::tempdir().unwrap();
        std::fs::write(copy.path().join("world.jsonc"), "{\"format\": 1,}").unwrap();
        let failure = run(&["validate", "--json", &copy.path().display().to_string()]).unwrap_err();
        assert_eq!(
            failure.output,
            "[{\"path\":\"world.jsonc\",\"start\":12,\"end\":12,\"line\":1,\"column\":13,\"code\":\"jsonc.trailing_comma\",\"message\":\"invalid JSONC\"}]\n"
        );
        assert!(failure.message.contains("1 problem(s)"));
        assert!(run(&["frobnicate"]).is_err());
    }
}
