# Tilewright world

Tilewright world is the game-neutral world format and toolkit for isometric and
top-down tile games. Worlds are continuous planes with stacked layers, stored
as 32 × 32 chunks of strict JSONC that review and merge well in git. A
deterministic compiler turns the source tree into one SQLite content pack that
servers, clients, bots and the browser load at startup.

Tilewright is maintained by Seraphina Studios alongside
[tilewright-renderer](https://github.com/seraphinastudios/tilewright-renderer)
and [tilewright-editor](https://github.com/seraphinastudios/tilewright-editor).
Each game describes itself through a game profile, so the toolkit carries no
game-specific code.

## Specifications

- [World format](docs/spec/FORMAT.md): planes, stacked layers, chunks,
  terrain grids, objects with stable uids, zones, portals, anchors,
  stairwells, definitions, typed field schemas with nested lists and
  objects, and the strict JSONC dialect.
- [Content pack](docs/spec/PACK.md): the SQLite pack and the binary chunk
  encoding.
- [ID diff](docs/spec/DIFF.md): the ID kinds, rename and allowlist rules,
  and the JSON report of `tilewright diff`.

All are drafts until format 1.0 is frozen.

## Crates

| Crate | Purpose |
| --- | --- |
| `tilewright-jsonc` | Strict, lossless, bounded JSONC parser with byte spans, span edits and canonical JSON output |
| `tilewright-source` | Loads and validates a source tree into a typed world model, reporting every problem with file, span and a stable code |
| `tilewright-pack` | Deterministic pack compiler, the `twc1+zstd` chunk codec, and a verifying pack reader |
| `tilewright-diff` | Compares the IDs of two worlds, source trees or packs, and reports removals, additions and renames |
| `tilewright-cli` | The `tilewright` command |

Transactions for structural edits follow.

## Command line

```sh
cargo run --locked --package tilewright-cli -- validate fixtures/basic
cargo run --locked --package tilewright-cli -- validate --json fixtures/basic
cargo run --locked --package tilewright-cli -- compile fixtures/basic dist
cargo run --locked --package tilewright-cli -- inspect dist/<sha256>.twpack
cargo run --locked --package tilewright-cli -- diff --allow allow.jsonc baseline.twpack fixtures/basic
```

`validate` prints one line per problem (`path:line:column: code: message`), or
a JSON array with `--json`, and a summary of planes, chunks, stairwells and
definitions when the tree is valid. `compile` writes `<sha256>.twpack` atomically and
never replaces a different file. `inspect` verifies a pack and prints its
metadata and counts. `diff` reports the IDs removed, added and renamed
between two worlds, each a source root or a pack, and exits 1 when one was
removed or renamed without the allowlist accepting it. `fixtures/basic` is a
small synthetic world that uses every feature of the format.

## Development

Rust 1.97.1 is pinned.

```sh
tools/validate.sh
```

Development is primarily agent-driven, under maintainer direction, review,
tests and repository validation. Direct human-written code contributions are
welcome under the same controls. Commits and pull-request titles use
Conventional Commits.

## License

MIT. Game content handled by the toolkit keeps its own credits and licenses.
