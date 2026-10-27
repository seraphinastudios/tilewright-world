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
  objects, terrain transitions, and the strict JSONC dialect.
- [Content pack](docs/spec/PACK.md): the SQLite pack and the binary chunk
  encoding.
- [ID diff](docs/spec/DIFF.md): the ID kinds, rename and allowlist rules,
  and the JSON report of `tilewright diff`.
- [Transactions](docs/spec/TRANSACTIONS.md): reviewable edits addressed by
  identity, with structural operations, preconditions, rebasing, a staged
  apply that rolls back on failure, and exact inverses.

All are drafts until format 1.0 is frozen.

## Crates

| Crate | Purpose |
| --- | --- |
| `tilewright-jsonc` | Strict, lossless, bounded JSONC parser with byte spans, span edits and canonical JSON output |
| `tilewright-source` | Loads and validates a source tree into a typed world model, reporting every problem with file, span and a stable code, and resolves terrain transitions |
| `tilewright-pack` | Deterministic pack compiler, the `twc1+zstd` chunk codec, and a verifying pack reader |
| `tilewright-diff` | Compares the IDs of two worlds, source trees or packs, and reports removals, additions and renames |
| `tilewright-edit` | Transactions: lossless structural edits, preview, rebase, staged apply with rollback, and inverses |
| `tilewright-cli` | The `tilewright` command |
| `tilewright-pack-wasm` | The pack reader for WebAssembly, behind the browser package |

## Go pack reader

[`go/`](go/README.md) is a pure-Go module,
`github.com/seraphinastudios/tilewright-world/go`, whose package `twpack`
reads and verifies packs for Go servers and tools. Its versions are tags of
the form `go/vX.Y.Z`. It and the Rust reader run the same conformance
fixtures, [`fixtures/packs`](fixtures/packs/README.md).

## Browser pack reader

[`packages/tilewright-pack`](packages/tilewright-pack/README.md) is the npm
package `@seraphinastudios/tilewright-pack`: the Rust reader compiled to
WebAssembly (`crates/tilewright-pack-wasm`) with a TypeScript-typed ESM API,
for games that read packs in the browser. It runs the same conformance
fixtures. `tools/build-wasm.sh` builds it.

## Command line

```sh
cargo run --locked --package tilewright-cli -- validate fixtures/basic
cargo run --locked --package tilewright-cli -- validate --json fixtures/basic
cargo run --locked --package tilewright-cli -- compile fixtures/basic dist
cargo run --locked --package tilewright-cli -- inspect dist/<sha256>.twpack
cargo run --locked --package tilewright-cli -- diff --allow allow.jsonc baseline.twpack fixtures/basic
cargo run --locked --package tilewright-cli -- tx preview edit.jsonc fixtures/basic
cargo run --locked --package tilewright-cli -- tx apply edit.jsonc fixtures/basic undo.jsonc
```

`validate` prints one line per problem (`path:line:column: code: message`), or
a JSON array with `--json`, and a summary of planes, chunks, stairwells and
definitions when the tree is valid. `compile` writes `<sha256>.twpack` atomically and
never replaces a different file. `inspect` verifies a pack and prints its
metadata and counts. `diff` reports the IDs removed, added and renamed
between two worlds, each a source root or a pack, and exits 1 when one was
removed or renamed without the allowlist accepting it. `tx preview` carries
out a transaction in memory and lists the files it would change; `tx
rebase` replays it on the tree as it is and records the new base; `tx
apply` writes its inverse, then applies it: with every guarantee on Linux,
and as a best effort on macOS and Windows, where it is supported only in
trees no one else can change and without access control lists of their
own, and runs only with `--unchecked-access`, which accepts that those
are not checked, as it does on Linux for trees on any file system but
ext2/3/4, XFS, Btrfs, tmpfs, F2FS, ZFS and bcachefs, a container's overlay
root included; there, keep the tree on a bind-mounted local volume (see
SECURITY.md and docs/spec/TRANSACTIONS.md section 9).
`fixtures/basic` is a small synthetic world that uses every feature of the
format except terrain transitions, which `fixtures/terrain` shows.

## Development

Rust 1.97.1, Go 1.27.2 and, for the browser package's tests, Node.js
26.10.0 are pinned. `tools/validate.sh` also builds the browser package with
the wasm tools that `tools/build-wasm.sh` pins and downloads once.

```sh
tools/validate.sh
```

Development is primarily agent-driven, under maintainer direction, review,
tests and repository validation. Direct human-written code contributions are
welcome under the same controls. Commits and pull-request titles use
Conventional Commits.

## License

MIT. Game content handled by the toolkit keeps its own credits and licenses.
