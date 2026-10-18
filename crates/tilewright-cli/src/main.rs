// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! `tilewright`: validate world sources, compile packs and inspect them.

use std::{fmt::Write as _, path::Path, process::ExitCode};
use tilewright_jsonc::write_json_string;
use tilewright_source::Diagnostic;

const USAGE: &str = "usage:
  tilewright validate [--json] ROOT
  tilewright compile ROOT OUTPUT_DIRECTORY
  tilewright inspect PACK
  tilewright --version";

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
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

fn run(arguments: &[&str]) -> Result<String, Failure> {
    match arguments {
        ["--version"] => Ok(format!("tilewright {}\n", env!("CARGO_PKG_VERSION"))),
        ["validate", root] => validate(Path::new(root), false),
        ["validate", "--json", root] => validate(Path::new(root), true),
        ["compile", root, output] => compile(Path::new(root), Path::new(output)),
        ["inspect", pack] => inspect(Path::new(pack)),
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
            let _ = writeln!(output, "{diagnostic}");
        }
    }
    output
}

fn load(root: &Path, json: bool) -> Result<tilewright_source::World, Failure> {
    tilewright_source::load(root).map_err(|diagnostics| Failure {
        output: diagnostics_text(&diagnostics, json),
        message: format!("{} problem(s) in {}", diagnostics.len(), root.display()),
    })
}

fn validate(root: &Path, json: bool) -> Result<String, Failure> {
    let world = load(root, json)?;
    if json {
        return Ok("[]\n".to_owned());
    }
    let chunks: usize = world.planes.values().map(|plane| plane.chunks.len()).sum();
    Ok(format!(
        "valid: {} plane(s), {chunks} chunk(s), {} definition(s), source {}\n",
        world.planes.len(),
        world.definitions.len(),
        hex(&world.source_digest)
    ))
}

fn compile(root: &Path, output: &Path) -> Result<String, Failure> {
    let world = load(root, false)?;
    let written = tilewright_pack::write_pack(&world, output)
        .map_err(|error| Failure::message(format!("cannot write pack: {error}")))?;
    Ok(format!(
        "{} {} bytes{}\n",
        written.path.display(),
        written.bytes,
        if written.created {
            ""
        } else {
            " (already present)"
        }
    ))
}

fn inspect(path: &Path) -> Result<String, Failure> {
    let fail =
        |error: tilewright_pack::Error| Failure::message(format!("{}: {error}", path.display()));
    let pack = tilewright_pack::Pack::open(path).map_err(fail)?;
    let summary = pack.verify().map_err(fail)?;
    let mut output = String::new();
    for key in [
        "format",
        "compiler",
        "profile_id",
        "profile_version",
        "source_digest",
    ] {
        let value = pack.meta(key).map_err(fail)?.unwrap_or_default();
        let _ = writeln!(output, "{key}: {value}");
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
    use super::run;
    use std::path::Path;

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
