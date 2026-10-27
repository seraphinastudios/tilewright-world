# Third-party notices

## Rust crates

Builds of the toolkit compile in source code from these projects through
their Rust bindings. Their licenses apply to the compiled code they
contribute.

| Component | Pulled in by | License |
| --- | --- | --- |
| [SQLite](https://sqlite.org/) | `rusqlite` with the `bundled` feature (`libsqlite3-sys`) | Public domain |
| [Zstandard](https://github.com/facebook/zstd) | `zstd` (`zstd-sys`) | BSD-3-Clause (upstream offers BSD-3-Clause or GPL-2.0; used under BSD-3-Clause) |

Rust crate dependencies keep their own licenses as recorded in `Cargo.lock`
and their crate metadata. No outside source is copied into this repository.

## WebAssembly reader (`packages/tilewright-pack`)

The npm package `@seraphinastudios/tilewright-pack` ships the Rust pack
reader compiled to WebAssembly by `crates/tilewright-pack-wasm`. On that
target `rusqlite` takes SQLite from `sqlite-wasm-rs` instead of
`libsqlite3-sys`, and these are compiled in besides the crates above:

| Component | Pulled in by | License |
| --- | --- | --- |
| [SQLite](https://sqlite.org/) 3.53.0 | [`sqlite-wasm-rs`](https://github.com/Spxg/sqlite-wasm-rs) 0.5.5 (MIT), through `rusqlite`'s `ffi-sqlite-wasm-rs` feature | Public domain |
| Parts of [musl](https://musl.libc.org/) libc (string, stdlib, math and stdio functions) | `sqlite-wasm-rs` | MIT |
| [printf](https://github.com/eyalroz/printf) | `sqlite-wasm-rs` | MIT |
| `rsqlite-vfs` 0.1.1, SQLite's in-memory file system | `sqlite-wasm-rs` | MIT |
| `hashbrown` 0.16.1, and through it `foldhash` 0.2.0, and `thiserror` 2.0.21 | `rsqlite-vfs` | MIT OR Apache-2.0 (`foldhash`: Zlib) |
| `wasm-bindgen` 0.2.129, `wasm-bindgen-shared` 0.2.129 and `js-sys` 0.3.106; through `wasm-bindgen-shared` `unicode-ident` 1.0.27; and through `js-sys` `futures-core`, `futures-task`, `futures-util`, `pin-project-lite` and `slab` | `tilewright-pack-wasm` | MIT OR Apache-2.0 (`slab`: MIT; `unicode-ident`: (MIT OR Apache-2.0) AND Unicode-3.0) |

The package carries `THIRD_PARTY_LICENSES.md`, the license text of every
crate and C library compiled into the module. Its
`scripts/licenses.mjs` writes it from the crates `Cargo.lock` resolves for
the module, and `tools/validate.sh` checks that it is current.

Building the module also uses these tools, whose release archives
`tools/build-wasm.sh` downloads once and checks by SHA-256 every time it
unpacks them. Nothing of them is compiled in, except the
JavaScript glue `wasm-bindgen` generates, which the package ships:

| Tool | Version | License |
| --- | --- | --- |
| [wasi-sdk](https://github.com/WebAssembly/wasi-sdk) (clang and `llvm-ar`, for SQLite and zstd) | 34.0 | Apache-2.0 WITH LLVM-exception |
| [wasm-bindgen](https://github.com/wasm-bindgen/wasm-bindgen) command | 0.2.129 | MIT OR Apache-2.0 |

Node.js runs the package's tests only; the package has no npm
dependencies.

## Go module (`go/`)

The Go pack reader depends on these modules, pinned in `go/go.mod` and
`go/go.sum`. Their licenses apply to the code they contribute to a program
that imports the reader.

| Module | Version | License | Compiled in |
| --- | --- | --- | --- |
| [modernc.org/sqlite](https://gitlab.com/cznic/sqlite) | v1.60.1 | BSD-3-Clause; SQLite 3.53.4, transpiled to Go, is public domain | Yes, without its `vec` package (sqlite-vec, MIT), which the reader does not import |
| [modernc.org/libc](https://gitlab.com/cznic/libc) | v1.77.1 | BSD-3-Clause; carries code from musl libc (MIT), Go (BSD-3-Clause), go-netdb (MIT) and NixOS/nixpkgs (MIT), as its `LICENSE-3RD-PARTY.md` records | Yes |
| [modernc.org/mathutil](https://gitlab.com/cznic/mathutil) | v1.7.1 | BSD-3-Clause | Yes |
| [modernc.org/memory](https://gitlab.com/cznic/memory) | v1.12.1 | BSD-3-Clause; carries code from Go and mmap-go (both BSD-3-Clause) | Yes |
| [github.com/klauspost/compress](https://github.com/klauspost/compress) | v1.20.3 | BSD-3-Clause; `zstd/internal/xxhash` is MIT. Its Apache-2.0 `gzhttp` package is not compiled in | Yes: `zstd` and the packages it uses |
| [github.com/dustin/go-humanize](https://github.com/dustin/go-humanize) | v1.0.1 | MIT | Yes |
| [github.com/google/uuid](https://github.com/google/uuid) | v1.6.0 | BSD-3-Clause | Yes, except on Windows |
| [github.com/remyoudompheng/bigfft](https://github.com/remyoudompheng/bigfft) | v0.0.0-20230129092748-24d4a6f8daec | BSD-3-Clause | Yes |
| [golang.org/x/sys](https://cs.opensource.google/go/x/sys) | v0.48.0 | BSD-3-Clause | Yes |
| [github.com/mattn/go-isatty](https://github.com/mattn/go-isatty) | v0.0.24 | MIT | On macOS, Windows and the BSDs |
| [github.com/ncruces/go-strftime](https://github.com/ncruces/go-strftime) | v1.0.0 | MIT | On macOS, Windows and the BSDs |

These modules are in the module graph, and so in `go/go.sum`, only because
the modules above require them for their own tools and tests. Nothing of
them is compiled into the reader:

| Module | Version | License |
| --- | --- | --- |
| github.com/google/pprof | v0.0.0-20260802141513-ef3492d7dac3 | Apache-2.0 and BSD-3-Clause |
| github.com/hashicorp/golang-lru/v2 | v2.0.7 | MPL-2.0 |
| golang.org/x/mod, golang.org/x/sync, golang.org/x/tools | v0.41.0, v0.23.0, v0.50.0 | BSD-3-Clause |
| modernc.org/cc/v4, ccgo/v4, fileutil, gc/v2, gc/v3, goabi0, opt, sortutil, strutil, token | v4.29.7, v4.36.1, v1.4.0, v2.6.5, v3.1.5, v0.2.0, v0.2.0, v1.2.1, v1.2.1, v1.1.0 | BSD-3-Clause |

No dependency is GPL or AGPL licensed. Zstandard is also offered under
GPL-2.0 and is used under BSD-3-Clause. To list what a build compiles in, run
`go list -deps -f '{{with .Module}}{{.Path}} {{.Version}}{{end}}' ./...` in
`go/`, with `GOOS` set for other platforms.
