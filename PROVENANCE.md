# Provenance

All code, documentation, tests and fixtures in this repository are MIT work by
Seraphina Studios, except the outside material the records below name, which
keeps its own MIT-compatible license. No other outside implementation, tests
or fixtures are copied or adapted. Before any outside source is used as
implementation reference or reused, confirm its license is compatible with
MIT and record the exact source, revision, destination, transformation and
notice here. GPL/AGPL material is
never copied, adapted or used as a dependency.

## Records

- **Variant hash constants.** The mixing function of the terrain-transition
  variant hash (`docs/spec/FORMAT.md` section 13,
  `crates/tilewright-source/src/transitions/resolve.rs`) is the output
  function of SplitMix64. Source: Sebastiano Vigna's `splitmix64.c`
  (<https://prng.di.unimi.it/splitmix64.c>, 2015), whose shifts and
  multipliers (30, `0xbf58476d1ce4e5b9`, 27, `0x94d049bb133111eb`, 31) are
  David Stafford's "Mix13". Only these five constants are used; the function
  is written here from them, and no code is copied. Notice: the author
  dedicated `splitmix64.c` to the public domain under CC0 1.0.
- **musl's COPYRIGHT.** `packages/tilewright-pack/scripts/musl-COPYRIGHT` and
  `tools/release/licenses/musl-COPYRIGHT` are both musl libc's `COPYRIGHT`
  file at tag `v1.2.5`
  (<https://git.musl-libc.org/cgit/musl/plain/COPYRIGHT?h=v1.2.5>, SHA-256
  `f9bc4423732350eb0b3f7ed7e91d530298476f8fec0c6c427a1c04ade22655af`), MIT,
  copied unchanged. The first is the license notice for the musl code that
  `sqlite-wasm-rs` compiles into the WebAssembly reader, and
  `scripts/licenses.mjs` reproduces it in the package's
  `THIRD_PARTY_LICENSES.md`. `tools/release/package.py` reproduces the
  second in the Linux CLI release archives, whose binaries link musl
  statically. No musl code is copied into this repository.
- **Commit message parsing for the release planner.**
  `scripts/release/commits.cjs` adapts the parts of these npm packages that
  decide a release:
  - conventional-commits-parser 6.4.0 (MIT): the line-by-line reading of
    headers, `-field-` lines, breaking-change notes and reverts, with their
    regular expressions;
  - the parser options of conventional-changelog-conventionalcommits 9.3.1
    (ISC): the header, breaking-header, note and revert patterns;
  - conventional-commits-filter 5.0.0 (MIT): how a revert and the commit it
    reverts cancel out;
  - @semantic-release/commit-analyzer 13.0.1 (MIT): how the rules combine
    into one release level.

  All four are from <https://github.com/conventional-changelog/conventional-changelog>
  and <https://github.com/semantic-release/commit-analyzer>, as published
  on npm. The code was rewritten in CommonJS without dependencies, and
  only what decides a release was kept (no bodies, footers, mentions or
  reference lists). It answers as the packages did on every message
  tried, with two deliberate differences. First, the contents of
  `-name-` field lines are ignored. The parser copied them onto the commit,
  so a field such as `-type-`, `-revert-`, `-hash-` or `-tag-` changed the
  analyzer's answer, and `-notes-`, `-references-` or (with an @mention
  in the message) `-mentions-` made it throw.
  Second, a breaking-change note that runs into a trailing field line
  counts; the parser threw on it.
  `scripts/release/THIRD_PARTY_LICENSES.md` holds their notices.
