# Provenance

All code, documentation, tests and fixtures in this repository are MIT work by
Seraphina Studios. No outside implementation, tests or fixtures are copied or
adapted. Before any outside source is used as implementation reference or
reused, confirm its license is compatible with MIT and record the exact source,
revision, destination, transformation and notice here. GPL/AGPL material is
never copied, adapted or used as a dependency.

## Recorded material

- `packages/tilewright-pack/scripts/musl-COPYRIGHT` is musl libc's
  `COPYRIGHT` file at tag `v1.2.5`
  (https://git.musl-libc.org/cgit/musl/plain/COPYRIGHT?h=v1.2.5), MIT,
  copied unchanged. It is the license notice for the musl code that
  `sqlite-wasm-rs` compiles into the WebAssembly reader, and
  `scripts/licenses.mjs` reproduces it in the package's
  `THIRD_PARTY_LICENSES.md`. No musl code is copied into this repository.
