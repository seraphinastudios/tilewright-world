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

- [World format](docs/spec/FORMAT.md): planes, layers, chunks, terrain grids,
  objects, zones, portals, anchors, definitions and the strict JSONC dialect.
- [Content pack](docs/spec/PACK.md): the SQLite pack and the binary chunk
  encoding.

Both are drafts until format 1.0 is frozen.

## Crates

| Crate | Purpose |
| --- | --- |
| `tilewright-jsonc` | Strict, lossless, bounded JSONC parser with byte spans and span edits |

The world model, validator, transactions, compiler and CLI follow.

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
