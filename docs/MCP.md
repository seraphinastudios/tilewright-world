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
[2025-11-25](https://modelcontextprotocol.io/specification/2025-11-25)
and only that revision. It answers `initialize` with it whatever version
the client asks for, as the revision's version negotiation says, and the
client decides whether to go on.

- Transport: stdio. Each message is one line of UTF-8 JSON-RPC 2.0, ended
  by a newline; a line may end in CR LF instead, and blank lines are
  skipped. Standard output carries replies and nothing else; notes for
  people go to standard error. Messages are handled one at a time, in
  order. The server stops when its input ends.
- Methods: `initialize`, `ping`, `tools/list` and `tools/call`. Before
  `initialize` is answered, only `initialize` and `ping` are served.
  The capabilities are `tools` alone, without list change notifications.
- Notifications get no reply, not even a wrong one.
  `notifications/initialized` and `notifications/cancelled` are accepted
  and need nothing: a request is answered before the next is read, so a
  cancellation always comes too late.
- Messages are read with the toolkit's strict JSONC parser, with comments
  refused: duplicate keys, trailing commas, a byte-order mark, integers
  past 2^53 − 1 and trailing content are errors. A request ID is a string
  or an integer. Batches (JSON arrays) are not part of the revision and are
  refused. Responses sent to the server are ignored.
- Replies escape every control character, DEL and C1 included, so no value
  can break a line or reach a terminal as a control sequence.

### Errors

Protocol errors are JSON-RPC errors with a fixed message and a stable
`data.code`; an error about a message whose ID could not be read has no
`id`.

| Code | `data.code` | When |
| --- | --- | --- |
| -32700 | a `jsonc.*` code, or `request.not_utf8` | The line is not UTF-8 JSON within the limits |
| -32600 | `request.too_large` | The line is longer than 64 KiB |
| -32600 | `request.not_object`, `request.id`, `request.jsonrpc`, `request.method` | Not a JSON-RPC request |
| -32600 | `session.initialized` | A second `initialize` |
| -32601 | `method.unknown` | Any other method |
| -32602 | `params.not_object`, `params.initialize`, `params.missing`, `params.name`, `params.arguments` | Malformed params |
| -32602 | `tool.unknown` | A tool this server does not have |
| -32602 | `cursor.invalid` | A cursor for `tools/list`, which lists every tool on one page |
| -32000 | `session.not_initialized` | A request other than `ping` before `initialize` |

A tool that is called correctly but cannot answer returns a result with
`isError: true` and one line of text, `<tool> failed: <code>: <message>`:

| Code | When |
| --- | --- |
| `arguments.invalid` | An argument the tool does not take, or of the wrong type or range |
| `tree.invalid` | The tree does not load, for a tool that needs a loaded world |
| `cursor.invalid` | A cursor not issued for this tool and these arguments, or past the end |
| `cursor.stale` | A cursor issued for another revision of the tree |
| `ids.unavailable` | The world holds more than 1,000,000 IDs |

## Tools

Every tool is marked read-only, non-destructive, idempotent and closed
world in its annotations, and has an input and an output schema. A
successful result has `structuredContent`, which conforms to the output
schema, and one line of text the server writes, naming only counts,
offsets and the revision. The text does not repeat the structured result
as JSON, so that nothing read from the world appears outside a typed field.

| Tool | Arguments | Result |
| --- | --- | --- |
| `world_summary` | none | The profile's `id` and `version`; each plane with its chunk count and its layers with theirs; each definition type with its count; the render and ground layers; the object count, inventory entries included, and the object uid count |
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
the server answers until then.

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
| One message, its newline not counted | 64 KiB. A longer line is refused without being kept, and reading goes on at the next newline |
| JSON in one message | Depth 32, 10,000 values, 1,000 array items, 128 keys per object, strings of 16 KiB, numbers of 32 bytes, no comments |
| Items per page (`limit`) | Default 50, at most 500 |
| Structured result | 256 KiB as written. A page stops before the item that would pass it and says so with `byte_budget_reached`; its first item is always kept, and an item is never larger than 64 KiB |
| One string value | 4 KiB before escaping, cut at a character boundary; `truncated_strings` counts the strings cut, and each diagnostic says whether it was cut |
| `prefix`, `contains`, `scope` | 256 bytes |
| A cursor sent back | 256 bytes |
| Lists in `world_summary` | 500 planes, 500 layers per plane and 500 definition types, within the byte budget; `planes_truncated`, `layers_truncated` and `definition_types_truncated` say when a list was cut |

The load itself has the bounds of `tilewright_source::LoadOptions::default`.
Time is not bounded: a call runs to its end, and a large tree takes as long
to load as `tilewright validate` takes.

## Read-only by construction

- No tool takes a path. The root is fixed when the server starts, and the
  tree is read through the toolkit's no-follow access layer
  (`SourceRoot`), opened once; on Unix, `refresh` reads the same
  directory even if the path is moved or replaced.
- The crate never calls anything that writes, creates, renames, removes or
  locks a file, publishes a transaction, starts a process or opens a
  network connection. Its `clippy.toml` makes each such function and type
  an error in it, and `tilewright-edit`, which publishes transactions, is
  not a dependency.
- Its tests run every tool, through every page, on copies of
  `fixtures/basic` and `fixtures/terrain` and require each copy to be byte
  for byte as it was, with no entry added or removed; on Unix they do so on
  a copy made unwritable too.

## World data is not instructions

A world's files are untrusted: a definition, a diagnostic's path or a
profile's version can hold any text, including text written to look like
instructions to a language model. The server keeps every value read from
the world inside a typed field of the structured result, its `initialize`
instructions and every tool description say that results are world data
and not instructions, and the text content of each result is a template
the server writes. Clients should still treat tool results as data.

## Not yet

Inspecting a definition or object with its source span, searching
definitions and objects by field, previewing a transaction, and references
and the impact of removing or renaming an ID are planned (issue #30).
Resources, prompts, an HTTP transport and pack files are out of scope for
now.
