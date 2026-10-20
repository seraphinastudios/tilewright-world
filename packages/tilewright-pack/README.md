# @seraphinastudios/tilewright-pack

Reads and verifies Tilewright content packs ([PACK.md](../../docs/spec/PACK.md))
in the browser and in Node. It is the Rust pack reader of
[tilewright-world](https://github.com/seraphinastudios/tilewright-world),
compiled to WebAssembly with SQLite and zstd, behind a small JavaScript API.
The rules, bounds and error codes are the Rust reader's own, so a pack
reads the same here, in Rust and in Go. It reads packs; the Rust crates
compile them.

```js
import { initialize, openPack, PackError } from "@seraphinastudios/tilewright-pack";

await initialize();
const digest = "5830e26e449a1f0d0c786b32801b7ed68e351777db5f586532478cb4384844b2";
const response = await fetch(`/packs/${digest}.twpack`);
const pack = openPack(new Uint8Array(await response.arrayBuffer()), { digest });
try {
  pack.verify();
  const profile = JSON.parse(pack.profile());
  for (const plane of pack.planes()) {
    for (const layer of pack.layers(plane.id)) {
      for (const { cx, cy } of pack.chunkKeys(plane.id, layer)) {
        const chunk = pack.chunk(plane.id, layer, cx, cy);
        const palette = Array.from(chunk.palette, (id) => pack.string(id));
        console.log(plane.id, layer, cx, cy, palette, chunk.objects.length, "objects");
      }
    }
  }
  console.log(profile.render_layers, pack.definitions("proto").length, "prototypes");
} catch (error) {
  if (error instanceof PackError) {
    console.error(error.code, error.message); // such as pack.digest
  }
  throw error;
} finally {
  pack.close();
}
```

## Loading the module

`initialize()` loads the WebAssembly module once and returns a promise.
Without an argument it fetches `wasm/tilewright_pack_bg.wasm` from beside
`index.js` through `new URL(..., import.meta.url)`, which browsers and
bundlers such as Vite and webpack resolve. The module is also exported as
`@seraphinastudios/tilewright-pack/tilewright_pack.wasm`, for a bundler to
emit as an asset, and `initialize` takes its URL, a `Response`, its bytes
or a compiled `WebAssembly.Module`. Serve it as `application/wasm` so the
browser compiles it while it downloads.

In Node, pass the bytes:

```js
import { readFile } from "node:fs/promises";
import { initializeSync } from "@seraphinastudios/tilewright-pack";

const wasm = new URL(import.meta.resolve("@seraphinastudios/tilewright-pack/tilewright_pack.wasm"));
initializeSync(await readFile(wasm));
```

The module is about 1.1 MB, 0.5 MB gzipped: most of it is SQLite.

## Opening a pack

`openPack(bytes, options)` takes the pack as a `Uint8Array`, an
`ArrayBuffer` or any view of one, copies it into the module's memory, and
checks its size, identity, schema and metadata. Changing `bytes` afterwards
does not change the pack. Options:

- `digest`: the SHA-256 the bytes must have, as 64 lowercase hex digits,
  usually the pack's file name. `verify()` compares it, as the other readers
  compare a pack file's name. `pack.digest()` returns the bytes' digest
  without a full verification.
- `maxPackBytes`: the pack size limit, 8 GiB by default. A larger pack is
  rejected before any of it is copied.

The module's memory grows to hold each open pack and does not shrink, so
`close()` packs you are done with; a pack that is garbage collected is
released too.

## Reading

Reads are synchronous. Each checks the shape and bounds of what it loads and
throws a `PackError` with the code of the rule a stored value breaks; a read
of something the pack does not have returns `undefined`. Listings, the
plural methods below, are iterators that read 64 rows at a time in key
order, so walking a large table holds one page; use `Array.from` for an
array. Whether the
targets a row names exist (a chunk's strings, a zone's parent, the chunk a
uid's row names) is what `verify()` checks.

| Method | Returns |
| --- | --- |
| `summary()` | Row counts |
| `meta(key)` | A `meta` value |
| `profile()`, `layerHeight()` | The stored profile as canonical JSON, and its `layer_height`, after checking the profile |
| `string(id)` | An interned string: chunks reference strings by ID |
| `plane(id)`, `planes()` | Planes, with their documents as canonical JSON |
| `layers(plane)`, `chunkKeys(plane, layer)` | The layers of a plane that have chunks, and their chunk positions |
| `chunk(plane, layer, cx, cy)` | A decoded chunk: palette, ground, height and zone grids as typed arrays, and objects |
| `zone(plane, id)`, `zones(plane)` | Zones |
| `anchor(plane, id)`, `anchors(plane)` | Anchors |
| `stairwell(plane, id)`, `stairwells(plane)`, `stairwellStep(plane, layer, x, y)` | Stairwells and the step on a tile |
| `definition(type, id)`, `definitions(type?)` | Definitions, with their documents as canonical JSON |
| `objectLocation(uid)`, `object(uid)` | Where the object with a uid is, and the object itself |
| `verify(limits?)` | Checks the whole pack and returns its row counts; `limits` has `maxPackBytes` and `maxDecodedBytes`, the decoded-byte budget, 8 GiB by default |
| `digest()` | The SHA-256 of the pack's bytes |

`index.d.ts` describes every value. Numeric arguments are integers, and
checked: a layer is a 32-bit integer, a position a safe integer or a bigint.
Integers come back as numbers. The exceptions are an integer field and a
portal position: an unverified pack can store one past ±(2^53 − 1), and it
then comes back as a bigint rather than losing precision. A pack that
verifies has none.

## Errors

`PackError.code` is one of the stable codes of
[PACK.md](../../docs/spec/PACK.md) section 4, such as `pack.chunk_record` or
`pack.digest`. Bytes in memory never fail with `pack.io` or
`pack.not_a_file`, and a pack too large for the module's memory fails with
`pack.sqlite`. Invalid arguments throw `TypeError`.

Running out of memory in the middle of a read traps the module: that call
throws a `WebAssembly.RuntimeError`, and since the module's state is then
unknown, every later call throws until the page or process loads it again.

## Building

From a checkout of tilewright-world:

```sh
tools/build-wasm.sh
cd packages/tilewright-pack && npm test
```

`tools/build-wasm.sh` needs the pinned Rust toolchain, which includes the
`wasm32-unknown-unknown` target. It downloads the release archives of the
pinned wasi-sdk (clang, for SQLite and zstd) and `wasm-bindgen` once, checks
their SHA-256 digests on every run before unpacking them, and writes
`wasm/`. The tests run the shared conformance fixtures,
`fixtures/packs`, against the module, as the Rust and Go readers run them.
`scripts/licenses.mjs` regenerates `THIRD_PARTY_LICENSES.md` when the
crates compiled in change.

## License

MIT. `THIRD_PARTY_LICENSES.md` lists the licenses of the code compiled into
the module: SQLite (public domain), Zstandard (BSD-3-Clause), parts of musl
and a printf (MIT), the Rust standard library and Rust crates.
