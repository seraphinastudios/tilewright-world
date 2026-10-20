# Tilewright pack reader for Go

`github.com/seraphinastudios/tilewright-world/go` is a small MIT Go module
that reads Tilewright content packs ([PACK.md](../docs/spec/PACK.md)). Its
package `twpack` opens a pack, reads its metadata, rows and chunks, and
verifies all of it. It is pure Go, with no cgo: SQLite comes from
`modernc.org/sqlite` and zstd from `github.com/klauspost/compress/zstd`. It
reads packs; the Rust crates compile them.

```go
import (
	"context"
	"fmt"

	"github.com/seraphinastudios/tilewright-world/go/twpack"
)

// load opens and verifies a pack, then reads every chunk of every plane.
func load(ctx context.Context, path string) error {
	pack, err := twpack.Open(ctx, path, twpack.Limits{})
	if err != nil {
		return err
	}
	defer pack.Close()
	if _, err := pack.Verify(ctx, twpack.Limits{}); err != nil {
		return err
	}
	profile, err := pack.Profile(ctx)
	if err != nil {
		return err
	}
	for plane, err := range pack.Planes(ctx) {
		if err != nil {
			return err
		}
		for layer, err := range pack.Layers(ctx, plane.ID) {
			if err != nil {
				return err
			}
			for key, err := range pack.ChunkKeys(ctx, plane.ID, layer) {
				if err != nil {
					return err
				}
				chunk, _, err := pack.Chunk(ctx, plane.ID, layer, key.CX, key.CY)
				if err != nil {
					return err
				}
				fmt.Println(plane.ID, layer, key.CX, key.CY, len(chunk.Ground), "of",
					len(profile.GroundLayers), "ground layers,", len(chunk.Objects), "objects")
			}
		}
	}
	return nil
}
```

This is the package example, so `go test` compiles it.

The module needs Go 1.26 or later; `go.mod` sets `toolchain go1.27.2` for
developing it.

## Versions

The module lives in the `go/` directory of this repository, so its versions
are tags of the form `go/vX.Y.Z`, such as `go/v0.1.0`. A consumer pins one
with `go get github.com/seraphinastudios/tilewright-world/go@v0.1.0`. Tags
follow semantic versioning and stay at `v0` until pack format 1.0 is
frozen. Releases are cut separately from this module's development.

## API

- `Open(ctx, path, limits)` opens a pack and checks its size, schema,
  identity, free pages, length and `meta` before anything else runs.
  `Limits` holds the pack-size limit and the verification decode budget,
  each 8 GiB when zero.
- `Meta`, `Summary`, `String`, `Profile` and `LayerHeight` read the
  metadata, row counts, interned strings and the checked game profile.
- `Plane`, `Planes`, `Layers`, `ChunkKeys` and `Chunk` look up planes, the
  layers with chunks, a layer's chunk positions, and decoded chunks.
  `DecodeChunk` and `DecodeRecord` decode stored chunk data directly.
- `Zone`, `Zones`, `Anchor`, `Anchors`, `Stairwell`, `Stairwells` and
  `StairwellStep` read zones, anchors and stairwells.
- `Definition`, `Definitions` and `DefinitionsOfType` read definitions.
- `Verify(ctx, limits)` checks the whole pack and counts its objects;
  `FileDigest` hashes the file.

Lists are iterators (`iter.Seq2[T, error]`) that hold one row at a time.
A `Pack` may be used from several goroutines; its methods share one
connection, which serializes them.

Errors are `*twpack.Error` values with a stable `Code`, such as
`pack.free_pages`, which `twpack.CodeOf(err)` returns. The codes are listed
in [PACK.md](../docs/spec/PACK.md) section 4 and are the ones the Rust
reader reports. An error from a canceled or expired context is the
context's own error.

## Untrusted packs

The reader follows section 3 of PACK.md, as the Rust reader does:

- The file is checked against the pack-size limit before SQLite opens it,
  and again by its pages once it is open, by verification before the
  integrity check, and while hashing.
- The database is opened read-only (`mode=ro`) and immutable, in SQLite's
  defensive mode, with untrusted schemas disabled (`trusted_schema`) and
  queries only. `Open` reads back `trusted_schema` and `query_only`, and a
  test checks that defensive mode is on.
- Every read checks a value's type and size in SQL before loading it, and no
  read loads a whole table. Chunk data is checked against the encoding's
  bound in SQL, must be exactly one zstd frame that records a content size
  of at most 16 MiB, and decodes into a buffer of that size.
- JSON stored in a pack must be canonical and within the source format's
  limits, which are enforced while parsing.

Single reads check the shape and bounds of what they load. Whether the
targets a row or record names exist (a chunk's strings and prototypes, a
plane's default zone, a zone's parent, a step's stairwell) is checked by
`Verify`, which is the full verification of PACK.md, including the integrity
check and the file name digest.

What differs from the Rust reader:

- `modernc.org/sqlite` opens every connection with read-write flags, and
  `mode=ro` in the URI restricts it to read-only, which SQLite enforces
  the same way. Defensive mode is set through the driver's `_defensive`
  parameter, which calls `sqlite3_db_config` before any statement runs, and
  `trusted_schema` through its pragma, which is the same setting as
  `SQLITE_DBCONFIG_TRUSTED_SCHEMA`.
- zstd frames are decoded by `klauspost/compress` rather than the reference
  library. The reader finds the frame's length by walking its block headers
  as the reference library does, so both accept exactly one frame with a
  recorded content size, and both reject reserved header bits, windows past
  zstd's 64-bit limit, bad blocks and bad checksums. Either may describe a
  corrupt frame differently, but both reject it as `pack.chunk_record`.
- A decoded `Chunk` holds every object, which takes several times the memory
  of its record; a record is at most 16 MiB. `Verify` checks objects as they
  decode instead of keeping them.

## Development

```sh
go vet ./...
go test ./...
gofmt -l .
```

`tools/validate.sh` runs all three. The tests run the shared conformance
fixtures in [`fixtures/packs`](../fixtures/packs/README.md), which the Rust
tests generate, so they run from a checkout of this repository.
