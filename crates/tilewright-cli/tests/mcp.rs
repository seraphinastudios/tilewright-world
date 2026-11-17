// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! `tilewright mcp` over real pipes, as an MCP client runs it.

use std::{
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Command, Stdio},
};

fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
        .display()
        .to_string()
}

/// A request line of revision 2026-07-28: `params` are the members after
/// `_meta`.
fn request(id: &str, method: &str, params: &str) -> String {
    let separator = if params.is_empty() { "" } else { "," };
    format!(
        r#"{{"jsonrpc":"2.0","id":{id},"method":"{method}","params":{{"_meta":{{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{{}}}}{separator}{params}}}}}"#
    )
}

/// Sends one request line and reads one reply line.
fn exchange(input: &mut impl Write, output: &mut impl BufRead, request: &str) -> String {
    input.write_all(request.as_bytes()).unwrap();
    input.write_all(b"\n").unwrap();
    input.flush().unwrap();
    let mut reply = String::new();
    output.read_line(&mut reply).unwrap();
    assert!(reply.ends_with('\n'), "{reply}");
    reply
}

#[test]
fn serves_a_session_over_pipes() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_tilewright"))
        .args(["mcp", "--root", &fixture("basic")])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let reply = exchange(
        &mut input,
        &mut output,
        &request("1", "server/discover", ""),
    );
    assert!(
        reply.starts_with(
            r#"{"jsonrpc":"2.0","id":1,"result":{"resultType":"complete","supportedVersions":["2026-07-28"],"#
        ),
        "{reply}"
    );
    assert!(
        reply.contains(concat!("\"version\":\"", env!("CARGO_PKG_VERSION"))),
        "{reply}"
    );
    // A notification gets no reply, so the next line answers tools/list.
    input
        .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/cancelled\",\"params\":{\"requestId\":1}}\n")
        .unwrap();
    let reply = exchange(&mut input, &mut output, &request("2", "tools/list", ""));
    assert!(reply.contains(r#""name":"world_summary""#), "{reply}");
    // A client of an earlier revision is told which one this server speaks.
    let reply = exchange(
        &mut input,
        &mut output,
        r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"old","version":"1"}}}"#,
    );
    assert!(
        reply.contains(r#""code":-32601"#) && reply.contains(r#""supported":["2026-07-28"]"#),
        "{reply}"
    );
    let reply = exchange(
        &mut input,
        &mut output,
        &request(
            "3",
            "tools/call",
            r#""name":"world_summary","arguments":{}"#,
        ),
    );
    assert!(reply.contains(r#""objects":11,"object_uids":5"#), "{reply}");
    drop(input);
    let mut rest = String::new();
    std::io::Read::read_to_string(&mut output, &mut rest).unwrap();
    assert_eq!(rest, "");
    let finished = child.wait_with_output().unwrap();
    assert!(finished.status.success());
    let log = String::from_utf8(finished.stderr).unwrap();
    assert!(
        log.starts_with("tilewright mcp: serving revision "),
        "{log}"
    );
}

#[test]
fn refuses_other_arguments() {
    for arguments in [
        &["mcp"][..],
        &["mcp", "--root"],
        &["mcp", &fixture("basic")],
        &["mcp", "--root", &fixture("basic"), "extra"],
    ] {
        let finished = Command::new(env!("CARGO_BIN_EXE_tilewright"))
            .args(arguments)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!finished.status.success(), "{arguments:?}");
        assert!(finished.stdout.is_empty());
        assert!(
            String::from_utf8(finished.stderr)
                .unwrap()
                .contains("tilewright mcp --root ROOT")
        );
    }
}

#[test]
fn a_closed_output_ends_the_server_quietly() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_tilewright"))
        .args(["mcp", "--root", &fixture("basic")])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The client stops reading before the server answers, and closes the
    // server's standard error too.
    drop(child.stdout.take());
    drop(child.stderr.take());
    let mut input = child.stdin.take().unwrap();
    for _ in 0..64 {
        if input
            .write_all(format!("{}\n", request("1", "tools/list", "")).as_bytes())
            .is_err()
        {
            break;
        }
    }
    drop(input);
    let status = child.wait().unwrap();
    assert!(status.success(), "{status}");
}

/// Copies the directory `from` to `to`, which must not exist.
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

/// Runs `tilewright diff --allow` with `entries` as the removals allowed,
/// and returns its exit code.
fn diff_allowing(directory: &Path, old: &Path, new: &Path, entries: &[String]) -> i32 {
    let allow = directory.join("allow.jsonc");
    std::fs::write(
        &allow,
        format!(
            r#"{{"format": "tilewright.diff-allow/1", "remove": [{}]}}"#,
            entries.join(",")
        ),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tilewright"))
        .arg("diff")
        .arg("--allow")
        .arg(&allow)
        .arg(old)
        .arg(new)
        .output()
        .unwrap();
    output.status.code().unwrap()
}

#[test]
fn impacts_allowance_paged_to_its_end_is_what_diff_needs() {
    // An object carrying 120 objects with uids: its removal needs 121
    // allowlist entries, more than one page of impact gives.
    let directory = tempfile::tempdir().unwrap();
    let old = directory.path().join("old");
    copy_tree(Path::new(&fixture("basic")), &old);
    let items: Vec<String> = (0..120)
        .map(|index| format!(r#"{{"uid": "bag.item{index}", "proto": "coin"}}"#))
        .collect();
    let chunk = "planes/overworld/layers/0/1_0.jsonc";
    std::fs::write(
        old.join(chunk),
        format!(
            r#"{{"format": "tilewright.chunk/1", "plane": "overworld", "layer": 0, "chunk": [1, 0], "objects": [{{"uid": "bag", "at": [0, 0], "proto": "chest", "inventory": [{}]}}]}}"#,
            items.join(",")
        ),
    )
    .unwrap();
    let new = directory.path().join("new");
    copy_tree(&old, &new);
    std::fs::write(
        new.join(chunk),
        r#"{"format": "tilewright.chunk/1", "plane": "overworld", "layer": 0, "chunk": [1, 0], "objects": []}"#,
    )
    .unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_tilewright"))
        .arg("mcp")
        .arg("--root")
        .arg(&old)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut entries: Vec<String> = Vec::new();
    let mut first_page = 0;
    let mut cursor: Option<String> = None;
    for id in 1.. {
        let extra = cursor
            .as_ref()
            .map(|token| format!(r#","entries_cursor":"{token}""#))
            .unwrap_or_default();
        let reply = exchange(
            &mut input,
            &mut output,
            &request(
                &id.to_string(),
                "tools/call",
                &format!(
                    r#""name":"impact","arguments":{{"kind":"object","id":"bag","action":"remove"{extra}}}"#
                ),
            ),
        );
        let document = tilewright_jsonc::Document::parse(
            reply.trim_end(),
            &tilewright_jsonc::Limits::default(),
        )
        .unwrap();
        let allowance = document
            .root()
            .get("result")
            .and_then(|result| result.get("structuredContent"))
            .and_then(|content| content.get("diff_allowance"))
            .unwrap_or_else(|| panic!("{reply}"));
        let page = allowance.get("entries").unwrap().as_array().unwrap();
        entries.extend(
            page.iter()
                .map(|entry| document.text(entry.span()).to_owned()),
        );
        if id == 1 {
            first_page = entries.len();
            assert_eq!(allowance.get("entries_total").unwrap().as_i64(), Some(121));
            assert_eq!(
                allowance.get("entries_truncated").unwrap().as_bool(),
                Some(true)
            );
        }
        match allowance.get("entries_next_cursor").unwrap().as_str() {
            Some(token) => cursor = Some(token.to_owned()),
            None => break,
        }
    }
    drop(input);
    assert!(child.wait().unwrap().success());
    assert_eq!(first_page, 100);
    assert_eq!(entries.len(), 121);
    // Exactly these entries: with all of them diff accepts the removal
    // (0), with the first page alone or one left out it does not (1).
    assert_eq!(diff_allowing(directory.path(), &old, &new, &entries), 0);
    assert_eq!(
        diff_allowing(directory.path(), &old, &new, &entries[..first_page]),
        1
    );
    for left_out in [0, 100, 120] {
        let mut fewer = entries.clone();
        fewer.remove(left_out);
        assert_eq!(
            diff_allowing(directory.path(), &old, &new, &fewer),
            1,
            "{left_out}"
        );
    }
}
