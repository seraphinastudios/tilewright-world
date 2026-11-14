# The MCP server

`tilewright mcp --root ROOT` serves read-only queries over one world source
tree to a [Model Context Protocol](https://modelcontextprotocol.io) client,
such as an editor or a coding agent. The client starts it as a subprocess
and talks to it over its standard input and output. It is part of the
`tilewright` command, in the unpublished crate `tilewright-mcp`.

```sh
cargo run --locked --package tilewright-cli -- mcp --root fixtures/basic
```

A client configuration names the command and its arguments, for example
`{"command": "tilewright", "args": ["mcp", "--root", "/path/to/world"]}`.

## Protocol

The server implements MCP revision
[2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28)
and only that revision. That revision is stateless: there is no
`initialize` handshake, and every request names its protocol version in
`params._meta`. **Clients that speak only revision 2025-11-25 or an earlier
one cannot connect.** Their `initialize` is refused with an error that names
the revision this server supports. Clients that speak both eras send
`server/discover` first, as the revision's stdio transport asks, and stay
modern.

- Transport: stdio. Each message is one line of UTF-8 JSON-RPC 2.0, ended
  by a newline; a line may end in CR LF instead, and blank lines are
  skipped. Standard output carries replies and nothing else; notes for
  people go to standard error. Messages are handled one at a time, in
  order. The server stops when its input ends, and quietly, with status 0,
  when its output is closed.
- Every request needs `params._meta` with
  `io.modelcontextprotocol/protocolVersion` (a string) and
  `io.modelcontextprotocol/clientCapabilities` (an object); the server uses
  no client capability. `io.modelcontextprotocol/clientInfo` is optional
  and ignored. A request for any version but `2026-07-28` gets the
  revision's `UnsupportedProtocolVersionError`.
- Methods: `server/discover`, `tools/list` and `tools/call`. `server/discover`
  gives the supported versions, the capabilities (`tools`, without list
  change notifications) and the instructions. `ping`, `initialize` and
  `logging/setLevel`, which the revision removed, are not served.
- Every result has `resultType: "complete"` and names the server in
  `_meta["io.modelcontextprotocol/serverInfo"]`. The `server/discover` and
  `tools/list` results may be cached for an hour (`ttlMs` 3600000,
  `cacheScope` `public`): they do not change while the server runs.
- Notifications get no reply, not even a wrong one.
  `notifications/cancelled` is accepted and needs nothing, because each
  request is answered before the next is read, so a cancellation always
  comes too late. The server sends no notifications.
- Messages are read with the toolkit's strict JSONC parser, with comments
  refused: duplicate keys, trailing commas, a byte-order mark, integers
  past 2^53 − 1 and trailing content are errors. A request ID is a string
  or an integer. Batches (JSON arrays) are not part of JSON-RPC as the
  revision uses it and are refused. A response sent to the server, whatever
  its ID, is ignored.
- Replies escape every control character, DEL and C1 included, so no value
  can break a line or reach a terminal as a control sequence.

### Errors

Protocol errors are JSON-RPC errors with a fixed message and a `data`
object. An error about a message whose ID could not be read has no `id`:
the revision's error response allows the ID to be left out but not to be
`null`.

| Code | `data` | When |
| --- | --- | --- |
| -32700 | `code`: a `jsonc.*` code, or `request.not_utf8` | The line is not UTF-8 JSON within the limits |
| -32600 | `code`: `request.too_large` | The line is longer than 64 KiB |
| -32600 | `code`: `request.not_object`, `request.id`, `request.jsonrpc`, `request.method` | Not a JSON-RPC request |
| -32601 | `code`: `method.legacy`, `supported`: `["2026-07-28"]` | `initialize`, from a client of an earlier revision |
| -32601 | `code`: `method.unknown` | Any other method |
| -32602 | `code`: `params.meta` | `params._meta` without its protocol version or client capabilities |
| -32602 | `code`: `params.not_object`, `params.name`, `params.arguments` | Malformed params |
| -32602 | `code`: `tool.unknown` | A tool this server does not have |
| -32602 | `code`: `cursor.invalid` | A cursor for `tools/list`, which lists every tool on one page |
| -32022 | `supported`: `["2026-07-28"]`, `requested` | Another protocol version |

A tool that is called correctly but cannot answer returns a result with
`isError: true` and one line of text the server writes,
`<tool> failed: <code>: <message>`:

| Code | When |
| --- | --- |
| `arguments.invalid` | An argument the tool does not take, or of the wrong type or range |
| `tree.invalid` | The tree does not load, for a tool that needs a loaded world |
| `cursor.invalid` | A cursor not issued for this tool and these arguments, or past the end |
| `cursor.stale` | A cursor issued for another revision of the tree |
| `ids.unavailable` | The world holds more than 1,000,000 IDs |

## Tools

Every tool is marked read-only, non-destructive, idempotent and closed
world in its annotations, and has an input and an output schema;
`tools/list` gives them in a fixed order. A successful result has
`structuredContent`, which conforms to the output schema, and, as its text
content, the same JSON serialized, as the revision says a tool with
structured content should, so that a client that passes only the text to
a model passes the data. Either way, every value read from the world is a
JSON string inside a typed field.

| Tool | Arguments | Result |
| --- | --- | --- |
| `world_summary` | none | The profile's `id` and `version`; each plane with its chunk count and its layers with theirs; each definition type with its count; the render and ground layers; the object count, inventory entries included, and the object uid count. Each list says whether it was cut (`planes_truncated`, `definition_types_truncated`, `render_layers_truncated`, `ground_layers_truncated`); `search_ids` reaches every plane, definition and render layer |
| `validate` | `limit`, `cursor` | `valid`, `total`, and a page of `diagnostics` in the shape of `tilewright validate --json` (`path`, `start`, `end`, `line`, `column`, `code`, `message`), each with `truncated` |
| `search_ids` | `kind` (required), `prefix`, `contains`, `scope`, `limit`, `cursor` | The IDs of one kind, sorted, as `tilewright diff` writes them; `scope` is a plane for anchors, objects, stairwells and zones, and a definition type for definitions |
| `refresh` | none | Reloads the tree: its `revision`, `valid`, the diagnostic count, the `previous_revision`, and whether it `changed` |

Every result carries `revision`, the lowercase hex source digest that
`tilewright validate` prints and packs record. A tree that does not load
has no revision today: `validate` and `refresh` then give
`"revision": null`, and the other tools fail with `tree.invalid`. The
server starts on such a tree, and on a root that does not exist, so that a
client can read why.

## Snapshots and cursors

The server loads the tree when it starts, with the bounds `tilewright
validate` uses, and keeps it. Every result is computed from that snapshot,
until `refresh` loads the tree again. Editing files does not change what
the server answers until then. The snapshot belongs to the server process,
not to a client: a `refresh` one client calls changes what every later
request sees, which each result's `revision` shows.

Each load opens ROOT afresh, on every platform: a root created after the
server started, or replaced, removed or repaired while it runs, is what
the next `refresh` reads. A ROOT that does not exist is reported as the
diagnostic `root.missing`, and one that is not a directory as
`root.not_directory`, both with the path `.`; a root that exists without
a `world.jsonc` gives the loader's `source.missing_file` instead.

`refresh` drops the old snapshot before it loads the tree again, so the
server holds at most one world, plus what a load holds while it runs. A
load is bounded by `tilewright_source::LoadOptions::default`: 500,000
files, 1,000,000 directory entries, 8 GiB read, 10,000 diagnostics. Those
bounds, not the server, cap its memory.

A paged result has `next_cursor`, a string when more items remain and
`null` otherwise; pass it back as `cursor`, with the same other arguments,
for the next page. The `limit` may change from page to page. A cursor names
the tool, a fingerprint of the arguments, the snapshot and an offset.
Cursors are opaque to clients and last only while the server runs. One
issued before a `refresh` that changed the tree is refused with
`cursor.stale`. For a tree that does not load, the snapshot is a
fingerprint of its diagnostics, so a reload that finds the same problems
keeps its cursors.

## Limits

| Limit | Value |
| --- | --- |
| One message, its newline and a CR before it not counted | 64 KiB. A longer line is refused without being kept, and reading goes on at the next newline |
| JSON in one message | Depth 32, 10,000 values, 1,000 array items, 128 keys per object, strings of 16 KiB, numbers of 32 bytes, no comments |
| Items per page (`limit`) | Default 50, at most 500 |
| A tool call's `result` as sent | 256 KiB, counting the structured result and its text copy together, and the fields around them. A page stops before the item that would pass it and says so with `byte_budget_reached`. One item costs at most 128 KiB, both copies counted, so a page always holds its first item |
| One string value | 4 KiB before escaping, cut at a character boundary; `truncated_strings` counts the strings cut, and each diagnostic says whether it was cut |
| `prefix`, `contains`, `scope` | 256 bytes |
| A cursor sent back | 256 bytes |
| Lists in `world_summary` | 500 planes, 500 layers per plane, 500 definition types, 500 render layers and 500 ground layers, sharing the byte budget in that order; a `*_truncated` flag says when a list was cut |

The load itself has the bounds of `tilewright_source::LoadOptions::default`
(see above). Time is not bounded: a call runs to its end, and a large tree takes as long
to load as `tilewright validate` takes.

## Read-only by construction

- No tool takes a path. The root is fixed when the server starts and is
  read through the toolkit's no-follow access layer (`SourceRoot`).
- The crate calls nothing that writes, creates, renames, removes or locks
  a file, changes its permissions, owner or times, publishes a transaction,
  starts a process or opens a network or local socket. Its `clippy.toml`
  makes each such std function and type it lists, and the access layer's
  writing and locking entry points, an error in it; each entry was checked
  to be flagged where its platform has it. `tilewright-edit`, which
  publishes transactions, is not a dependency. That is a list checked by a
  lint, not a proof: something it leaves out is caught only by the tests.
- Its tests run every tool, through every page, on copies of
  `fixtures/basic` and `fixtures/terrain` and require each copy to be as it
  was: the same entries, each file's bytes, and every entry's permissions
  and modification time. On Unix they do so on a copy made unwritable too.

## World data is not instructions

A world's files are untrusted: a definition, a diagnostic's path or a
profile's version can hold any text, including text written to look like
instructions to a language model. The server keeps every value read from
the world inside a JSON string of a typed field, in the structured result
and in its serialized text copy alike. The instructions `server/discover`
gives and every tool description say that results are world data and not
instructions, and the text of a tool error is written by the server.
Clients should still treat tool results as data.

## Not yet

Inspecting a definition or object with its source span, searching
definitions and objects by field, previewing a transaction, and references
and the impact of removing or renaming an ID are planned (issue #30).
Resources, prompts, an HTTP transport and pack files are out of scope for
now.
