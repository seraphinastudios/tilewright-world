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
- Replies escape every control character, DEL and C1 included, and the
  line and paragraph separators U+2028 and U+2029, which some line
  splitters break at, so no value can break a line or reach a terminal as
  a control sequence.

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
| `revision.changed` | The `revision` argument is not the snapshot's revision: the tree was loaded again since, or the revision was never this snapshot's; the text ends with the current revision, or says the tree does not load, so the snapshot has none. From `inspect`, also: the item's file no longer holds the snapshot's item, so the tree changed on disk; call `refresh`. From `preview_transaction`, also: the tree on disk is no longer at the snapshot's revision, so the plan would not be the snapshot's; call `refresh` |
| `item.not_found` | `inspect` of an ID the snapshot does not hold |

## Tools

Every tool is marked read-only, non-destructive, idempotent and closed
world in its annotations, and has an input and an output schema;
`tools/list` gives them in a fixed order. A successful result has
`structuredContent`, which conforms to the output schema, and, as its text
content, the same JSON serialized, as the revision says a tool with
structured content should, so that a client that passes only the text to
a model passes the data. Either way, every value read from the world is a
JSON value inside a typed field: a string, except in `inspect`'s `value`,
which is the item's JSON.

| Tool | Arguments | Result |
| --- | --- | --- |
| `world_summary` | `revision` | The profile's `id` and `version`; each plane with its chunk count and its layers with theirs; each definition type with its count; the render and ground layers; the object count, inventory entries included, and the object uid count. Each list says whether it was cut (`planes_truncated`, `definition_types_truncated`, `render_layers_truncated`, `ground_layers_truncated`); `search_ids` reaches every plane, definition and render layer |
| `validate` | `limit`, `cursor`, `revision` | `valid`, `total`, and a page of `diagnostics` in the shape of `tilewright validate --json` (`path`, `start`, `end`, `line`, `column`, `code`, `message`), each with `truncated` |
| `search_ids` | `kind` (required), `prefix`, `contains`, `scope`, `limit`, `cursor`, `revision` | The IDs of one kind, sorted, as `tilewright diff` writes them; `scope` is a plane for anchors, objects, stairwells and zones, and a definition type for definitions |
| `search_definitions` | `type`, `field`, `equals`, `contains`, `limit`, `scan_limit`, `cursor`, `revision` | A page of `definitions`, each with `type`, `id`, `path` and the matched `field`, `value_kind` and `value`; see "Searching definitions and objects" |
| `search_objects` | `plane`, `layer`, `bbox`, `proto`, `uid_prefix`, `carried`, `field`, `equals`, `contains`, `limit`, `scan_limit`, `cursor`, `revision` | A page of `objects`, each with `uid`, `plane`, `layer`, `tile`, `proto`, `carried`, `carried_by`, `path`, `position` and the matched field; see "Searching definitions and objects" |
| `inspect` | `kind` and `id` (both required), `revision` | One item: its `path`, its `span` in that file, and its `value`; see "Inspecting an item" |
| `preview_transaction` | `transaction` (required), `excerpts`, `revision` | Whether the transaction `would_apply`, the files it would change with their sizes, digests and changed spans, the `resulting_revision`, the undo check and the refusals; see "Previewing a transaction" |
| `refresh` | none | Reloads the tree: its `revision`, `valid`, the diagnostic count, the `previous_revision`, and whether it `changed` |

Every result carries `revision`, the lowercase hex source digest that
`tilewright validate` prints and packs record. A tree that does not load
has no revision today: `validate` and `refresh` then give
`"revision": null`, and the other tools fail with `tree.invalid`.

Every tool but `refresh` takes an optional `revision`, 64 lowercase hex
digits, for a client that combines several calls: when the snapshot is
not at that revision, the call fails with `revision.changed`, whose text
ends with the current revision. A tree that does not load has no
revision, so any `revision` given fails then, `validate`'s included,
with text that says so; `validate` without one still answers. Arguments
are checked first (for `inspect`, the shape of its `id` too; for
`preview_transaction`, that `transaction` is an object of at most 256
operations), then `revision`, then whether the tree loads. The
`revision` is not part of a cursor's arguments: a cursor is bound to its
snapshot already. The
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
server holds at most one world, plus what a load holds while it runs
(a preview holds more while it runs; see "Previewing a transaction"). A
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

## Inspecting an item

`inspect` takes a `kind` and an `id`, written as `search_ids` writes
them: `definition` (`type/id`), `plane` (its ID), `zone`, `anchor` and
`stairwell` (`plane/id`), and `object` (its uid; an object without a uid
cannot be inspected yet, but `search_objects` gives its file and
position). Render layers are not items.

The loaded world keeps no source spans, so `inspect` reads the item's one
file again: `defs/<type>/<id>.jsonc`, `planes/<plane>/plane.jsonc`, the
plane's `zones.jsonc`, `anchors.jsonc` or `stairwells.jsonc`, or the
object's chunk. It reads it through the same no-follow access layer as a
load, with ROOT opened afresh, and parses it with the strict JSONC parser
under the format's limits, as a load does. In it, the item is the whole
document, the first entry of the list with the item's `id` (a list that
repeats an ID does not load), or, for an object, the entry its position names: an index into
the chunk's `objects`, then into each `inventory` down to it, so an
inventory entry's span is its own.

The result gives:

- `path`, root-relative;
- `span`: `start` and `end`, byte offsets of the item's value in the file
  as read now (end exclusive; leading comments and spacing not included),
  and `start_line`, `start_column`, `end_line`, `end_column`, 1-based,
  lines ended by LF and columns counted in characters (Unicode scalar
  values, not bytes and not the UTF-16 units some editors count), as
  diagnostics count them. A file checked
  out with CR LF line endings has other byte offsets than an LF one, and
  the same lines and columns;
- `value`: the item's JSON as the file holds it, members in source order
  and numbers spelled as in the source, each string cut to 4 KiB (counted
  in `truncated_strings`). When the whole value would pass the byte budget,
  it is cut short at the budget, every array and object closed, and
  `value_truncated` is true; a key longer than 4 KiB is never cut, the
  value stops before it. `canonical_bytes` is the length of the item's full
  canonical JSON, so a client knows what it did not get; the span gives
  the rest in the file.

**What is checked against the snapshot.** The library keeps no per-file
digests, so the server checks the item instead: a definition, plane, zone
or stairwell must have the same canonical JSON (as `tilewright-jsonc`
writes it) as the snapshot's; an anchor, and an object with its inventory,
which the library keeps without a canonical form, must have every value
the library keeps equal: for an object its keys, uid, tile, prototype,
render layer, sub-layer, `z`, override fields (numbers by value, list and
object values by canonical JSON), portal, inventory entries one by one, and
`custom` by canonical JSON. When the file is missing, a link, unreadable,
not UTF-8 or JSONC, or the item differs, the call fails with
`revision.changed`, and `refresh` loads the tree as it now is.

So the value returned is the snapshot's item, and the span is right for
the file as it is on disk now. The file may differ from the snapshot's
bytes elsewhere: in spacing and comments, inside the item too, or in its
other items; and for an anchor or object, in how the same values are
spelled (`1.0` for `1`, or `"z": 0` written out). The check does not
prove the rest of the file or of the tree unchanged: only `refresh` does.

## Previewing a transaction

`preview_transaction` carries a transaction out in memory, as
`tilewright tx preview` does (docs/spec/TRANSACTIONS.md), and reports it.
Nothing is written, locked or published: the plan is dropped when the call
returns. Applying a transaction stays a reviewed `tilewright tx apply` or
editor action.

- **Argument.** `transaction` is the transaction document as a JSON object
  (`format`, `operations`, and optionally `title`, `actor`, `base`,
  `result` and `custom`), not a path and not a string. It is part of the
  request, so it is within the message limits (64 KiB, depth 32 for the
  whole message, 1,000 items an array, 10,000 values). The depth leaves
  26 nested arrays or objects inside an operation's `value` (tested; 27
  are refused). A deeper or larger one, a duplicate key,
  or an integer past 2^53 − 1 is refused as JSON before the tool runs. It
  may have at most 256 operations; more is `arguments.invalid`. Its bytes,
  as the request holds them, are what is planned, so spans in refusals
  index them. `excerpts` (default `false`) adds each file's changed text.
- **Order.** The arguments are checked, then `revision`, then whether the
  snapshot loads (`tree.invalid` otherwise). Then the document is read as
  a transaction: a document that is not one (`jsonc.*`, `tx.shape`) is
  refused there, with `refused_at: "transaction"`, without reading the
  tree. Otherwise it is planned on the tree with the library's
  `plan_in`: every precondition (the `base` digests, `expect` and `test`
  values, cell expectations, what each operation needs), the library's
  limits, and validation of the tree it would leave. A failure is
  `refused_at: "plan"`. A plan is then given the library's undo check
  (`Plan::check_undo`, section 8.1), as `tilewright tx preview` does; a
  refusal there is `refused_at: "undo"`, and the plan's files and resulting
  revision are still reported.
- **The snapshot.** The library plans on the tree on disk; the snapshot
  keeps no file text. So after planning, the server loads the tree again,
  with the files the plan read laid over it as it read them (the tree as
  it is now, after a refusal), and requires its revision to be the
  snapshot's. Otherwise the call fails with `revision.changed`, and
  `refresh` loads the tree as it now is. So a result is a plan of the
  snapshot's files, with one exception: the revision covers only the
  files the loader reads. A transaction may name other files under the
  root, such as a file under `profile/` that is neither the profile nor a
  schema it names (`profile/extra.jsonc`). For those, the preview reads
  the disk as it is at the call, the same revision can give another
  answer once such a file changes, and a transaction that changes only
  such files has a `resulting_revision` equal to `revision`; `refresh`
  does not see a change to them either. A change made and undone while
  the call runs is not seen. The tool error `revision.changed` and the planner's `tx.conflict`
  differ: the first says the tree moved from under the snapshot, the
  second that the transaction's own preconditions (its `base` digests, an
  `expect` or a `test`) do not hold on the snapshot.
- **What it guarantees.** A preview is a plan against the snapshot, not a
  promise that applying the transaction later succeeds: the tree can
  change before then, applying checks every precondition again, and
  publishing can be refused for reasons of its own (permissions, file
  systems whose access it cannot check, a staging name in the way;
  TRANSACTIONS.md section 9), none of which a preview checks.
- **Result.** `would_apply` is true when nothing refused it. `refused_at`
  is `null`, `"transaction"`, `"plan"` or `"undo"`. `operations` is the
  transaction's operation count (`null` when it is not a transaction).
  `resulting_revision` is the source digest the tree would have, from the
  validating load that planning makes anyway, so it costs nothing more
  (`null` when planning failed). `undo_check` is `passed`, `refused` or
  `not_run`. `files_changed` counts the files it would create, modify or
  delete, and `files_consulted` those it read or checked but leaves as
  they are.
- **Files.** `files` lists the changed files in path order, at most 200
  (`files_truncated`), each with its root-relative `path`; `change`
  (`created`, `modified` or `deleted`); `before_bytes` and `after_bytes`;
  `before_sha256` and `after_sha256`, the lowercase hex SHA-256 of the
  bytes, as `base` and `result` record them (each `null` where the file is
  absent); and `changed`, the smallest byte range of the new text that
  holds every change: `start` and `end` (end exclusive) and `start_line`,
  `start_column`, `end_line` and `end_column`, counted as `inspect`
  counts them, and `removed_bytes`, the length of the old text it
  replaces. A created file's range is the whole file; a deleted file has
  `changed: null`. The range is one range: edits far apart in a file give
  a range that covers what lies between them.
- **Excerpts.** By default no file text is returned, only sizes, digests
  and ranges. With `excerpts: true`, `excerpt` is the new text of the
  changed range as a JSON string, cut to 4 KiB (`excerpt_truncated`), and
  counted in the byte budget like any value.
- **Refusals.** `refusals` lists at most 500 (`refusals_total` counts them
  all; the library reports at most 1,000, the last a note that there may
  be more; `refusals_truncated`), in the shape of a `validate` diagnostic
  with `on_transaction`: a problem of the transaction has the path
  `transaction` and its span in the argument as
  sent, or no position (`0`) when it concerns the whole transaction, such
  as the undo check (no diagnostic of the edited tree has the path
  `transaction`: an entry of that name in the root makes the tree fail to
  load, and one made after the snapshot is a change to the tree, so the
  call fails with `revision.changed` before any result); a problem of the edited tree has the loader's code
  and the root-relative path, with its span in the file as the transaction
  would leave it. Codes are the library's: `tx.shape`, `tx.conflict`,
  `tx.result`, `tx.path`, `tx.limit`, `tx.io`, `jsonc.*` and the loader's.
  There is no cursor: a preview is not paged, since paging would plan the
  transaction again.
- **Budget.** The files come first and the refusals share what they leave
  of the byte budget; `byte_budget_reached` says either list stopped
  there. `truncated_strings` counts every string cut.
- **Cost.** A call reads the files the transaction touches, within the
  library's bounds (256 MiB read, 4 GiB of work, 1,000 diagnostics), loads
  the whole tree to validate the result, loads it once more when the
  transaction addresses an object by uid, and once more for the snapshot
  check; the undo check reads nothing from disk but does about the work of
  planning again in each of its rounds, at most four. The library's read
  and work budgets are its constants, not options, so the server cannot
  set tighter ones: one call can do up to 4 GiB of work for the plan and
  again for each undo round, and 256 operations do not bound it (36
  operations on one large file reach the work budget). Requests are
  handled one at a time and a preview cannot be cancelled, so a client
  can keep the server busy for many seconds per call. Measured on a
  release build (AMD Ryzen 9 9950X3D):
  - typical: on a tree of 205 chunks and 100,000 objects that `refresh`
    loads in 0.37 s, placing an object took 0.32 s and editing one by uid
    0.51 s;
  - five files of 51 MiB each, edited at the read limit: 4.0 s and about
    2.1 GB, as `tilewright tx preview` takes (1.9 GB);
  - the worst measured, 36 `set` operations on one 59 MB file of 220,000
    zones (a request of about 3.7 KB): 12.9 s and 1.47 GB; 40 such
    operations are refused at the work budget after 4.9 s; two such files
    with 34 operations took 15.7 s and 2.56 GB.

  Memory grows with how the touched files parse, about 25 times their
  size for files dense with values, so near the read limit it can reach
  several gigabytes (extrapolated, not run). While a preview runs, the
  server holds the snapshot's world, the plan's world, and the touched
  files before and after; during the snapshot check, a third world is
  loaded beside them.

## Searching definitions and objects

A search walks its candidates in a fixed order, examines each against its
filters, and returns the matches on a page. Field values are the ones the
definition or object sets itself: an object's prototype's fields are not
resolved, so a field only its prototype sets is not found.

- **Matching.** With `field`, an item matches when it sets that field and,
  with `equals` or `contains` (not both), its value matches. `equals` is a
  string, number, boolean or null: a scalar value is equal to it (numbers
  by value, so `5` equals `5.0`); a list, object or JSON value matches when
  a scalar anywhere inside it is equal (keys are not looked at). `contains`
  is a string: a string value containing it, or a list, object or JSON
  value with such a string inside. Matching is by bytes, case-sensitive.
- **`search_definitions`.** Candidates are the definitions sorted by type,
  then ID, by bytes; `type` keeps one type. Without `field`, `equals` (a
  string) and `contains` match the definition's ID without its type, as
  in `chest` for `proto/chest`; a `/` in them is refused with
  `arguments.invalid`, since no such ID holds one: give the type as
  `type`.
- **`search_objects`.** Candidates are the objects of each plane in ID
  order, each chunk in (layer, x, y) order, and each chunk's objects depth
  first: an object, then its inventory entries, then the next object.
  `carried: false` leaves inventory entries out (default `true`). `plane`
  and `layer` (the layer of the plane, not the render layer) keep one;
  `bbox` `[x0, y0, x1, y1]`, inclusive, keeps objects whose tile, in plane
  coordinates, is inside it, an inventory entry being at the tile of the
  object that carries it; `proto` keeps one prototype, `uid_prefix` objects
  whose uid starts with it. `equals` and `contains` need `field`. Planes,
  layers and chunks outside `plane`, `layer` and `bbox` are passed over
  without walking their objects.
- **Items.** A definition item has `type`, `id` and `path`. An object item
  has `uid` (or null), `plane`, `layer`, `tile`, `proto`, `carried`,
  `carried_by` (the uid of the object that carries it, or null when it has
  none or is not carried), `path` (its chunk) and `position` (its index in
  the chunk's `objects`, then in each inventory down to it). Both have
  `field`, `value_kind` (`null`, `boolean`, `integer`, `number`, `string`
  or `json`) and `value` for the field matched, or nulls; a `json` value is
  the field's canonical JSON as a string, cut like any string.
- **Scan budget.** A call examines at most `scan_limit` candidates (default
  and maximum 10,000), and stops after the candidate that brings it to
  8 MiB of list, object and JSON values parsed again for matching, so it
  parses at most 8 MiB plus that one value, which can be as large as its
  file. Then
  `scan_budget_reached` is true and `next_cursor` resumes at the first
  candidate not examined, so a page can hold fewer than `limit` matches, or
  none, and still have a next page. This is apart from `limit` and from the
  byte budget (`byte_budget_reached`), after which the cursor resumes at the
  match that did not fit. `offset` is the candidate the page started at,
  and `scanned` how many it examined. Paging on returns every match exactly
  once. A search does not count its matches, so it gives no `total`.
- **Cost of resuming.** A call resumes by passing over the candidates
  before its cursor's offset without examining them: whole chunks by their
  object counts, which the snapshot keeps, and the objects of the chunk the
  offset falls in one by one; `search_definitions` passes over the
  definitions one by one. One call stays cheap, but paging through a whole
  large world costs time that grows with the square of the pages within a
  chunk or definition list. Measured on a release build (AMD Ryzen 9
  9950X3D) on a world of ten chunks of 100,000 objects each: a filter that
  matches nothing pages through the 1,000,000 candidates in 101 calls and
  0.07 s, the slowest call 1.3 ms; matching every object, 2,001 calls and
  2.5 s at `limit` 500, and 20,001 calls and 9.6 s at `limit` 50, the
  slowest call under 5 ms.

## Limits

| Limit | Value |
| --- | --- |
| One message, its newline and a CR before it not counted | 64 KiB. A longer line is refused without being kept, and reading goes on at the next newline |
| JSON in one message | Depth 32, 10,000 values, 1,000 array items, 128 keys per object, strings of 16 KiB, numbers of 32 bytes, no comments |
| Items per page (`limit`) | Default 50, at most 500 |
| A tool call's `result` as sent | 256 KiB, counting the structured result and its text copy together, and the fields around them. A page stops before the item that would pass it and says so with `byte_budget_reached`. One item costs at most 128 KiB, both copies counted, so a page always holds its first item |
| One string value | 4 KiB before escaping, cut at a character boundary; `truncated_strings` counts the strings cut, and each diagnostic says whether it was cut |
| `prefix`, `contains`, `scope`, `type`, `field`, `equals` (a string), `plane`, `proto`, `uid_prefix` | 256 bytes |
| `id` of `inspect` | 512 bytes |
| Candidates a search examines in one call (`scan_limit`) | Default and at most 10,000, and 8 MiB of field values parsed again, plus the one value that reaches it (a value can be as large as its file) |
| `inspect`'s re-read | One file of at most 64 MiB, the format's limit, parsed under the format's limits |
| A previewed transaction | The message limits, and at most 256 operations; planned within the library's limits (TRANSACTIONS.md section 11), which the server cannot tighten: up to 256 MiB read and 4 GiB of work for the plan and for each undo round |
| Files and refusals of a preview | 200 files and 500 refusals, sharing the byte budget in that order; `excerpts` cut to 4 KiB |
| A cursor sent back | 256 bytes |
| Lists in `world_summary` | 500 planes, 500 layers per plane, 500 definition types, 500 render layers and 500 ground layers, sharing the byte budget in that order; a `*_truncated` flag says when a list was cut |

The load itself has the bounds of `tilewright_source::LoadOptions::default`
(see above). Time is not bounded: a call runs to its end, and a large tree takes as long
to load as `tilewright validate` takes.

## Read-only by construction

- No tool takes a path. The root is fixed when the server starts and is
  read through the toolkit's no-follow access layer (`SourceRoot`).
  `inspect` reads its one file again through it too, with
  `SourceRoot::read`, which opens the file read-only and follows no link.
  `preview_transaction` takes the transaction itself, not a path; the
  paths in it are the library's to confine (`tx.path` for one it may not
  touch, a link included), and planning reads through the same layer.
- The crate calls nothing that writes, creates, renames, removes or locks
  a file, changes its permissions, owner or times, publishes a transaction,
  starts a process or opens a network or local socket. Its `clippy.toml`
  makes each such std function and type it lists, and the access layer's
  writing and locking entry points, an error in it; each entry was checked
  to be flagged where its platform has it. `tilewright-edit` is a
  dependency for planning, which only reads; its publishing entry points
  (`Plan::publish`, `Plan::publish_with`, `Plan::unchecked_access`,
  `write_new`, and the `PublishOptions`, `Published` and `PublishError`
  types) are on the list, and must resolve, so a rename in the library
  fails the lint rather than dropping the entry. What the library itself
  calls is not checked by this crate's lint: that planning reads only is
  the library's contract (TRANSACTIONS.md section 6), and the tests check
  it. That is a list checked by a lint, not a proof: something it leaves
  out is caught only by the tests.
- Its tests run every tool, through every page, on copies of
  `fixtures/basic` and `fixtures/terrain` and require each copy to be as it
  was: the same entries, each file's bytes, and every entry's permissions
  and modification time. On Unix they do so on a copy made unwritable too.
  The previews among them are accepted, refused and malformed
  transactions, and a test of hostile transactions (paths out of the
  root, links, unknown operations, deep and malformed values) runs on a
  tree in a directory of its own and requires that directory unchanged
  too, so nothing is made beside the root either.

## World data is not instructions

A world's files are untrusted: a definition, a diagnostic's path or a
profile's version can hold any text, including text written to look like
instructions to a language model. The server keeps every value read from
the world inside a typed field, in the structured result and in its
serialized text copy alike: as a JSON string, or, for `inspect`'s
`value`, as the item's own JSON, whose strings are escaped like any
other. A transaction is client input, and the library's refusal messages
quote values from it and from the world (an identifier, a key, a cell's
value): they are returned only as the `message` strings of `refusals`,
like a diagnostic's, and never in the text of a tool error, which the
server writes. Unicode format characters, such as the bidirectional controls,
are valid JSON data and are sent as they are: a client that displays
results to people should show them safely. The instructions `server/discover`
gives and every tool description say that results are world data and not
instructions, and the text of a tool error is written by the server.
Clients should still treat tool results as data.

## Not yet

A preview cannot yet tell which of the files it reads or changes the
revision covers (see "Previewing a transaction"): that needs the loader's
list of the files it read, planned with the reference index.
References and the impact of removing or renaming an ID, inspecting an object without a uid, and searching fields
an object inherits from its prototype are planned or open (issue #30).
Resources, prompts, an HTTP transport and pack files are out of scope for
now.
