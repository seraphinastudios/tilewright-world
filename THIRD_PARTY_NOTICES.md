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
and their crate metadata. No outside source code is copied into this
repository; `PROVENANCE.md` records the one outside license notice it keeps.
Each release archive of the CLI carries `THIRD_PARTY_LICENSES.txt`, which
`tools/release/package.py` assembles from the license files of every crate
that binary compiles in.

## Linked into the CLI binaries

The toolchain links these into the released `tilewright` binaries besides
the crates. Each release archive carries their notices.

| Component | Platforms | License | Notice in the archive |
| --- | --- | --- | --- |
| The Rust standard library | All | Apache-2.0 OR MIT, with some parts under other permissive licenses | `RUST_LIBRARY_COPYRIGHT.html`, the pinned toolchain's `COPYRIGHT-library.html` |
| [musl](https://musl.libc.org/), the C library | Linux (static) | MIT | In `THIRD_PARTY_LICENSES.txt`, from `tools/release/licenses/musl-COPYRIGHT` (see `PROVENANCE.md`) |

The Windows binaries link Microsoft's C runtime statically, under the
license terms of Microsoft's build tools. The macOS binaries link the
system libraries dynamically.

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

No dependency is GPL or AGPL licensed. To list what a build compiles in, run
`go list -deps -f '{{with .Module}}{{.Path}} {{.Version}}{{end}}' ./...` in
`go/`, with `GOOS` set for other platforms.

## Release tooling (`scripts/release/`)

The release planner runs these npm packages in CI, locked in
`scripts/release/package-lock.json`. Nothing of them is distributed.

| Package | Version | License |
| --- | --- | --- |
| [@semantic-release/commit-analyzer](https://github.com/semantic-release/commit-analyzer) | 13.0.1 | MIT |
| [conventional-changelog-conventionalcommits](https://github.com/conventional-changelog/conventional-changelog) | 9.3.1 | ISC |

Their 28 dependencies in the lockfile are MIT, ISC, BSD-2-Clause
(`uglify-js`) or BSD-3-Clause (`source-map`) licensed.
