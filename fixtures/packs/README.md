# Pack reader conformance fixtures

Every pack reader runs these fixtures: the Rust reader in
`crates/tilewright-pack/tests/conformance`, opening each pack both as a
file and from its bytes, the Go reader in `go/twpack/conformance_test.go`,
and the WebAssembly reader, the Rust reader compiled for the browser, in
`packages/tilewright-pack/test/conformance.test.js`. They are the content pack compiled from
`fixtures/basic`, named by its digest, and broken copies of it. Each broken
copy breaks one rule of [PACK.md](../../docs/spec/PACK.md) section 3, and
`manifest.json` gives the error code ([PACK.md](../../docs/spec/PACK.md)
section 4) that opening, single reads and full verification must report.

## Regenerating

The files are generated, never edited. The generator is
`crates/tilewright-pack/tests/conformance/generate.rs`; it compiles the
source tree, then changes copies with SQL, a following `VACUUM` unless the
case needs the pages kept, and byte edits. The fixture source, the compiler
version and the bundled SQLite and zstd fix every byte, so a change to any of
them changes the files. `cargo test` fails while the files differ from what
the generator writes; regenerate them with:

```sh
TILEWRIGHT_UPDATE_PACK_FIXTURES=1 cargo test --locked -p tilewright-pack --test conformance fixtures_are_current
```

Then commit the changed files and check `manifest.json`'s diff: a code that
changes is a reader change.

## The manifest

`manifest.json` has:

- `format`: `tilewright.pack-conformance/1`.
- `codes`: every code a reader reports, in order. Readers check that they
  know exactly these.
- `valid`: packs that open and verify, with what reading them returns: the
  verification summary, every `meta` value, the layer height, every string
  in ID order, every plane, zone, anchor, stairwell, stairwell step and
  definition row, every `object_uids` row, and for each chunk its palette
  as strings, its object count (inventories included) and the SHA-256 of
  its rendering (below).
- `cases`: one object per broken pack:
  - `file` and `rule`, what the copy breaks;
  - `open_limits`: `max_pack_bytes` to open with, when not the default;
  - `open`: the code opening fails with, or absent when it succeeds;
  - `reads`: single reads, each an `op` with its arguments and the `error`
    code it fails with, or `null` when it must succeed. A read can have a
    `rule` of its own;
  - `verify_limits`: `max_pack_bytes` and `max_decoded_bytes` to verify
    with, when not the defaults;
  - `verify`: the code full verification fails with.

Reads are `chunk` (`plane`, `layer`, `cx`, `cy`), `string` (`id`),
`stairwell` (`plane`, `id`), `stairwell_step` (`plane`, `layer`, `x`, `y`),
`layer_height` and `profile`, which both check the whole stored profile,
`object_location` (`uid`), which reads a uid's `object_uids` row, `object`
(`uid`), which also decodes the row's chunk and checks that the object at
its index has the uid, and `plane` (`id`), `zone` (`plane`, `id`), `anchor`
(`plane`, `id`) and `definition` (`type`, `id`). Every reader runs them
all. A read of a row that does not exist succeeds and finds nothing.

`pack.io` and `pack.not_a_file` have no fixture, since a missing file or a
directory cannot be committed as one; each reader tests them itself, along
with packs too large to commit, such as chunk data past its bound.

## Chunk rendering

A decoded chunk renders as lines of text, each ending in a newline (LF),
with numbers in decimal and fields separated by one space:

```text
chunk_size <chunk size>
palette <each palette string ID>
ground <each cell>                  one line per ground grid, in order
heights <each height>
zones <each cell>
object <depth> <tile> <proto> <layer> <sub_layer> <z>
```

Grids are row-major, and a line whose list is empty ends in the space after
its word. Each object, in record order with its inventory entries after it
at one more depth (0 for a chunk's own objects), has its `object` line, then
`uid <uid>` if it has a uid, then `portal location <plane> <layer> <x> <y>`
or `portal anchor <plane> <anchor>` if it has a portal, then one line per field in order, then
`custom <canonical JSON>` if it has custom data. An inventory entry's tile
is 65535. A field line is `field <name ID> ` and then `null`,
`bool true`, `bool false`, `integer <value>`, `number <16 lowercase hex
digits of the IEEE 754 bits>`, `string <string ID>` or `json <canonical
JSON>`.
