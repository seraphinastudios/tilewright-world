// Copyright 2026 Seraphina Studios
// SPDX-License-Identifier: MIT

//! A read-only Model Context Protocol server over one Tilewright world
//! source tree, run by `tilewright mcp --root <tree>` (docs/MCP.md).
//!
//! It implements MCP revision 2026-07-28, and only that revision, over the
//! stdio transport: newline-delimited JSON-RPC 2.0 messages on standard
//! input and output, handled one at a time. The revision is stateless:
//! there is no `initialize` handshake, and every request names its protocol
//! version in `params._meta`. Requests are read with the toolkit's strict
//! JSONC parser under tight limits, with comments refused, and every reply
//! is written by hand with every control character escaped. The tree is
//! loaded once at start, and again by the `refresh` tool; every result
//! names the revision it was computed at.
//!
//! Nothing here writes, creates, locks or publishes, starts a process or
//! opens a connection: `clippy.toml` refuses the functions that could, and
//! the tests run every tool on a copy of each fixture and require the copy
//! to be unchanged.
//!
//! ```
//! use std::io::Cursor;
//! let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/basic");
//! let mut server = tilewright_mcp::Server::open(&root, "0.0.0");
//! let input = concat!(
//!     r#"{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"#,
//!     r#""io.modelcontextprotocol/protocolVersion":"2026-07-28","#,
//!     r#""io.modelcontextprotocol/clientCapabilities":{}}}}"#,
//!     "\n",
//! );
//! let mut output = Vec::new();
//! server.serve(Cursor::new(input), &mut output, &mut std::io::sink()).unwrap();
//! let output = String::from_utf8(output).unwrap();
//! assert!(output.starts_with(r#"{"jsonrpc":"2.0","id":1,"result":{"resultType":"complete","supportedVersions":["2026-07-28"]"#));
//! ```

mod budget;
mod json;
mod snapshot;
mod tools;

pub use budget::{
    DEFAULT_LIMIT, MAXIMUM_CURSOR_BYTES, MAXIMUM_LIMIT, MAXIMUM_RESULT_BYTES, MAXIMUM_STRING_BYTES,
};
pub use json::json_string;

use snapshot::Snapshot;
use std::{
    fmt::Write as _,
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
};
use tilewright_jsonc::{Document, Limits, Value};

/// The MCP revision this server implements, and the only one it serves.
pub const PROTOCOL_VERSION: &str = "2026-07-28";

/// The `_meta` key of a request's protocol version.
const VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";
/// The `_meta` key of a request's client capabilities.
const CAPABILITIES_KEY: &str = "io.modelcontextprotocol/clientCapabilities";

/// How long, in milliseconds, a client may keep the `server/discover` and
/// `tools/list` results: they do not change while the server runs, and an
/// hour bounds how long a client keeps them across an upgrade.
const TTL_MS: u32 = 3_600_000;

/// Most bytes of one message, its newline and a CR before it not counted.
/// A longer line is refused without being kept, and reading goes on at the
/// next newline.
pub const MAXIMUM_MESSAGE_BYTES: usize = 64 * 1024;

/// The parser's limits for one message: JSON on one line, without
/// comments, and small.
const MESSAGE_LIMITS: Limits = Limits {
    max_input_bytes: MAXIMUM_MESSAGE_BYTES,
    max_depth: 32,
    max_values: 10_000,
    max_array_items: 1_000,
    max_object_keys: 128,
    max_string_bytes: 16 * 1024,
    max_number_bytes: 32,
    max_comments: 0,
    max_comment_bytes: 0,
    max_lines: 1,
    max_line_bytes: MAXIMUM_MESSAGE_BYTES,
};

/// The error codes this server sends: JSON-RPC 2.0's, and the revision's
/// own for an unsupported protocol version.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ErrorCode {
    Parse,
    InvalidRequest,
    MethodNotFound,
    InvalidParams,
    UnsupportedProtocolVersion,
}

impl ErrorCode {
    const fn number(self) -> i32 {
        match self {
            Self::Parse => -32700,
            Self::InvalidRequest => -32600,
            Self::MethodNotFound => -32601,
            Self::InvalidParams => -32602,
            Self::UnsupportedProtocolVersion => -32022,
        }
    }
}

/// What an error reply's `data` holds.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Data {
    /// `{"code": ...}`: a stable code of this server's.
    Reason(&'static str),
    /// `{"code": ..., "supported": [...]}`, for a method of the revisions
    /// before this one, such as `initialize`.
    Legacy(&'static str),
    /// The revision's `{"supported": [...], "requested": ...}`.
    Unsupported(String),
}

/// A JSON-RPC error reply: its code, a fixed message, and its data.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Failure {
    code: ErrorCode,
    message: &'static str,
    data: Data,
}

const fn failure(code: ErrorCode, reason: &'static str, message: &'static str) -> Failure {
    Failure {
        code,
        message,
        data: Data::Reason(reason),
    }
}

/// A request ID: a string or an integer, never null.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Id {
    Integer(i64),
    String(String),
}

impl Id {
    fn write(&self, output: &mut String) {
        match self {
            Self::Integer(value) => {
                let _ = write!(output, "{value}");
            }
            Self::String(value) => json_string(value, output),
        }
    }
}

/// The MCP server: the root it serves, and the tree's snapshot.
pub struct Server {
    root: PathBuf,
    /// The `io.modelcontextprotocol/serverInfo` object.
    info: String,
    snapshot: Snapshot,
}

impl Server {
    /// Loads the tree at `root`. A tree that does not load, or a root that
    /// does not exist, still makes a server, whose `validate` tool reports
    /// why. `version` is the server's version for its `serverInfo`.
    #[must_use]
    pub fn open(root: &Path, version: &str) -> Self {
        let mut info =
            String::from("{\"name\":\"tilewright\",\"title\":\"Tilewright world\",\"version\":");
        json_string(version, &mut info);
        info.push('}');
        Self {
            root: root.to_path_buf(),
            info,
            snapshot: Snapshot::load(root),
        }
    }

    /// The revision of the snapshot, when the tree loads.
    #[must_use]
    pub fn revision(&self) -> Option<&str> {
        self.snapshot.revision()
    }

    /// Serves messages from `input` until it ends, writing each reply as
    /// one line to `output` and flushing it, and notes for people to `log`.
    ///
    /// # Errors
    /// Returns an error when reading `input` or writing `output` fails;
    /// a failure to write to `log` is ignored.
    pub fn serve(
        &mut self,
        mut input: impl BufRead,
        mut output: impl Write,
        log: &mut impl Write,
    ) -> io::Result<()> {
        let _ = writeln!(log, "tilewright mcp: {}", self.status());
        let mut line = Vec::new();
        loop {
            let reply = match read_line(&mut input, &mut line, MAXIMUM_MESSAGE_BYTES)? {
                Line::End => return Ok(()),
                Line::TooLong => {
                    let _ = writeln!(
                        log,
                        "tilewright mcp: refused a message longer than {MAXIMUM_MESSAGE_BYTES} bytes"
                    );
                    Some(error_reply(
                        None,
                        &failure(
                            ErrorCode::InvalidRequest,
                            "request.too_large",
                            "the message is longer than the server reads (64 KiB)",
                        ),
                    ))
                }
                Line::Message => self.handle(&line),
            };
            if let Some(reply) = reply {
                output.write_all(reply.as_bytes())?;
                output.write_all(b"\n")?;
                output.flush()?;
            }
        }
    }

    /// The reply to one message, if it gets one.
    fn handle(&mut self, line: &[u8]) -> Option<String> {
        // A line ending in CR LF is read as if it ended in LF.
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.iter().all(|byte| matches!(byte, b' ' | b'\t' | b'\r')) {
            return None;
        }
        let parse_error = |reason| {
            Some(error_reply(
                None,
                &failure(ErrorCode::Parse, reason, "the message is not valid JSON"),
            ))
        };
        let Ok(text) = std::str::from_utf8(line) else {
            return parse_error("request.not_utf8");
        };
        let document = match Document::parse(text, &MESSAGE_LIMITS) {
            Ok(document) => document,
            Err(error) => return parse_error(error.code.as_str()),
        };
        let invalid = |reason, message| failure(ErrorCode::InvalidRequest, reason, message);
        let root = document.root();
        if root.as_object().is_none() {
            // Including an array: the revision has no batches.
            return Some(error_reply(
                None,
                &invalid("request.not_object", "a message must be a JSON object"),
            ));
        }
        let method = root.get("method");
        // A response, whatever its ID: this server sends no requests, so
        // there is nothing to answer it with.
        if method.is_none() && (root.get("result").is_some() || root.get("error").is_some()) {
            return None;
        }
        let id = match root.get("id") {
            None => None,
            Some(Value::Integer { value, .. }) => Some(Id::Integer(*value)),
            Some(Value::String { value, .. }) => Some(Id::String(value.clone())),
            Some(_) => {
                return Some(error_reply(
                    None,
                    &invalid(
                        "request.id",
                        "a request ID must be a string or an integer, and not null",
                    ),
                ));
            }
        };
        let checked = (|| {
            if root.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
                return Err(invalid("request.jsonrpc", "jsonrpc must be \"2.0\""));
            }
            let method = method
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("request.method", "method must be a string"))?;
            let params = match root.get("params") {
                None => None,
                Some(params @ Value::Object { .. }) => Some(params),
                Some(_) => {
                    return Err(failure(
                        ErrorCode::InvalidParams,
                        "params.not_object",
                        "params must be an object",
                    ));
                }
            };
            Ok((method, params))
        })();
        // A notification is never answered, not even when it is wrong.
        // `notifications/cancelled` can only name a request already
        // answered, as requests are handled one at a time, so it is
        // ignored, as are all others.
        let id = id?;
        Some(
            match checked.and_then(|(method, params)| self.request(method, params)) {
                Ok(fields) => {
                    let mut reply = String::from("{\"jsonrpc\":\"2.0\",\"id\":");
                    id.write(&mut reply);
                    reply.push_str(",\"result\":{\"resultType\":\"complete\",");
                    reply.push_str(&fields);
                    reply.push_str(",\"_meta\":{\"io.modelcontextprotocol/serverInfo\":");
                    reply.push_str(&self.info);
                    reply.push_str("}}}");
                    reply
                }
                Err(failure) => error_reply(Some(&id), &failure),
            },
        )
    }

    /// The fields of a request's result, after `resultType`.
    fn request(&mut self, method: &str, params: Option<&Value>) -> Result<String, Failure> {
        match method {
            "server/discover" | "tools/list" | "tools/call" => {}
            // The handshake of the revisions before this one. The revision
            // leaves the code to the server and asks it to name the
            // versions it supports.
            "initialize" => {
                return Err(Failure {
                    code: ErrorCode::MethodNotFound,
                    message: "this server implements MCP revision 2026-07-28 only, which has no initialize: send each request with its protocol version in params._meta",
                    data: Data::Legacy("method.legacy"),
                });
            }
            _ => {
                return Err(failure(
                    ErrorCode::MethodNotFound,
                    "method.unknown",
                    "this server does not have that method",
                ));
            }
        }
        let missing = failure(
            ErrorCode::InvalidParams,
            "params.meta",
            "every request needs params._meta with io.modelcontextprotocol/protocolVersion and io.modelcontextprotocol/clientCapabilities",
        );
        let params = params.ok_or_else(|| missing.clone())?;
        let meta = params.get("_meta").ok_or_else(|| missing.clone())?;
        let version = meta
            .get(VERSION_KEY)
            .and_then(Value::as_str)
            .ok_or_else(|| missing.clone())?;
        // An unsupported version is refused as such whatever else the
        // request lacks: the revision's rule for it has no condition.
        if version != PROTOCOL_VERSION {
            return Err(Failure {
                code: ErrorCode::UnsupportedProtocolVersion,
                message: "Unsupported protocol version",
                data: Data::Unsupported(version.to_owned()),
            });
        }
        meta.get(CAPABILITIES_KEY)
            .and_then(Value::as_object)
            .ok_or_else(|| missing.clone())?;
        match method {
            "server/discover" => {
                let mut fields = String::from("\"supportedVersions\":");
                fields.push_str(&supported());
                fields.push_str(
                    ",\"capabilities\":{\"tools\":{\"listChanged\":false}},\"instructions\":",
                );
                json_string(tools::INSTRUCTIONS, &mut fields);
                let _ = write!(fields, ",\"ttlMs\":{TTL_MS},\"cacheScope\":\"public\"");
                Ok(fields)
            }
            "tools/list" => {
                if params.get("cursor").is_some() {
                    return Err(failure(
                        ErrorCode::InvalidParams,
                        "cursor.invalid",
                        "this server lists every tool on one page and issues no cursor for tools/list",
                    ));
                }
                let mut fields = tools::list();
                let _ = write!(fields, ",\"ttlMs\":{TTL_MS},\"cacheScope\":\"public\"");
                Ok(fields)
            }
            _ => {
                let name = params.get("name").and_then(Value::as_str).ok_or(failure(
                    ErrorCode::InvalidParams,
                    "params.name",
                    "params.name must be a string",
                ))?;
                let arguments = match params.get("arguments") {
                    None => None,
                    Some(arguments @ Value::Object { .. }) => Some(arguments),
                    Some(_) => {
                        return Err(failure(
                            ErrorCode::InvalidParams,
                            "params.arguments",
                            "params.arguments must be an object",
                        ));
                    }
                };
                let tool = tools::Tool::named(name).ok_or(failure(
                    ErrorCode::InvalidParams,
                    "tool.unknown",
                    "unknown tool: tools/list names the tools of this server",
                ))?;
                Ok(tools::call(tool, arguments, &mut self.snapshot, &self.root))
            }
        }
    }

    /// One line on the snapshot for the log.
    fn status(&self) -> String {
        match self.snapshot.revision() {
            Some(revision) => format!("serving revision {revision}"),
            None => format!(
                "the tree does not load ({} diagnostic(s)); serving its diagnostics",
                self.snapshot.diagnostics().len()
            ),
        }
    }
}

/// The versions this server supports, as a JSON array.
fn supported() -> String {
    let mut output = String::from("[");
    json_string(PROTOCOL_VERSION, &mut output);
    output.push(']');
    output
}

fn error_reply(id: Option<&Id>, failure: &Failure) -> String {
    let mut reply = String::from("{\"jsonrpc\":\"2.0\",");
    // An ID that could not be read is left out: the revision's error
    // response has an optional ID that is a string or a number, never null.
    if let Some(id) = id {
        reply.push_str("\"id\":");
        id.write(&mut reply);
        reply.push(',');
    }
    let _ = write!(
        reply,
        "\"error\":{{\"code\":{},\"message\":",
        failure.code.number()
    );
    json_string(failure.message, &mut reply);
    reply.push_str(",\"data\":{");
    match &failure.data {
        Data::Reason(reason) => {
            reply.push_str("\"code\":");
            json_string(reason, &mut reply);
        }
        Data::Legacy(reason) => {
            reply.push_str("\"code\":");
            json_string(reason, &mut reply);
            reply.push_str(",\"supported\":");
            reply.push_str(&supported());
        }
        Data::Unsupported(requested) => {
            reply.push_str("\"supported\":");
            reply.push_str(&supported());
            reply.push_str(",\"requested\":");
            // The client's own value, cut so a reply stays small.
            json_string(json::clip(requested, 256).0, &mut reply);
        }
    }
    reply.push_str("}}}");
    reply
}

/// What [`read_line`] found.
#[derive(Debug, Eq, PartialEq)]
enum Line {
    /// A message, in the buffer, without its newline.
    Message,
    /// A line longer than the cap, which was skipped to its newline.
    TooLong,
    /// The end of the input.
    End,
}

/// Reads the next line into `line`, keeping at most `maximum` bytes, not
/// counting a CR that ends it: a longer line is read on to its newline
/// without being kept. The last line may lack a newline.
fn read_line(input: &mut impl BufRead, line: &mut Vec<u8>, maximum: usize) -> io::Result<Line> {
    line.clear();
    // One byte more is kept, in case it is the CR of a CR LF.
    let kept = maximum + 1;
    let fits = |line: &Vec<u8>| line.len() <= maximum || line.last() == Some(&b'\r');
    let mut too_long = false;
    loop {
        let available = match input.fill_buf() {
            Ok(available) => available,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if available.is_empty() {
            return Ok(if too_long || !fits(line) {
                Line::TooLong
            } else if line.is_empty() {
                Line::End
            } else {
                Line::Message
            });
        }
        let (part, end) = match available.iter().position(|byte| *byte == b'\n') {
            Some(newline) => (&available[..newline], Some(newline + 1)),
            None => (available, None),
        };
        if !too_long {
            if line.len() + part.len() > kept {
                too_long = true;
                line.clear();
            } else {
                line.extend_from_slice(part);
            }
        }
        let used = end.unwrap_or(part.len());
        input.consume(used);
        if end.is_some() {
            return Ok(if too_long || !fits(line) {
                Line::TooLong
            } else {
                Line::Message
            });
        }
    }
}

#[cfg(test)]
mod tests;
