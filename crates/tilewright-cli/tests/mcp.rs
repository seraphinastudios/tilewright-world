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
