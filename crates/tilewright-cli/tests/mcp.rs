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
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#,
    );
    assert!(
        reply.starts_with(r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-11-25","#),
        "{reply}"
    );
    assert!(
        reply.contains(concat!("\"version\":\"", env!("CARGO_PKG_VERSION"))),
        "{reply}"
    );
    // A notification gets no reply, so the next line answers the ping.
    input
        .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
        .unwrap();
    let reply = exchange(
        &mut input,
        &mut output,
        r#"{"jsonrpc":"2.0","id":"p","method":"ping"}"#,
    );
    assert_eq!(reply, "{\"jsonrpc\":\"2.0\",\"id\":\"p\",\"result\":{}}\n");
    let reply = exchange(
        &mut input,
        &mut output,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
    );
    assert!(reply.contains(r#""name":"world_summary""#), "{reply}");
    let reply = exchange(
        &mut input,
        &mut output,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"world_summary","arguments":{}}}"#,
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
