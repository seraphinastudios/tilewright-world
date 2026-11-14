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

- **Local crate candidate preparation and verification.**
  `tools/crates/prepare.py` adapts the two-pass Cargo multi-package packaging
  approach in `tools/crate_prepare.py` (`package_twice`) and the isolated
  extraction/local graph override approach in `tools/verify-packages.sh`
  from tilewright-renderer, revision
  `cd8dc43578119b4e44772fadaac07d85053d4d6b`
  (<https://github.com/seraphinastudios/tilewright-renderer>). Both are MIT,
  Copyright (c) 2026 Seraphina Studios; the root `LICENSE` preserves that
  notice and the full license. The implementation here is narrowed to
  credential-free offline local candidates, adds archive/manifest checks,
  excludes all registry, release, upload and attestation logic, and uses
  Python extraction and a standalone consumer instead of the shell verifier.
  Tests in `tools/crates/test_prepare.py` and candidate policy are new MIT work.

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
- **Automatic crate publication.** `tools/crates/publish.py` adapts parts of
  tilewright-renderer's `tools/crate_release.py` at revision
  `dafd2187aaed965299e326f8dd0aeee1478da65f`
  (<https://github.com/seraphinastudios/tilewright-renderer>), MIT, Copyright
  (c) 2026 Seraphina Studios; the root `LICENSE` preserves that notice and
  the full license. Adapted, and rewritten for this repository: the
  `PutOutcome` and `put_once` upload client (one PUT, no proxies or
  redirects, a bounded answer, fixed outcome labels), here with 401 and 403
  as definite refusals besides 429; the per-version `registry_state`
  (absent, indeterminate or present across the API, sparse index and
  download), here also requiring the crate's owner and treating a download
  not yet served as indeterminate; the upload receipts, here also written
  before each upload; the immutable release snapshot (`release_snapshot`,
  here `snapshot`); the publication boundary and the authenticated,
  redirect-free GitHub reads and create-only writes; and the offline
  `gh attestation verify --bundle` step. The ownership rule, the attempt and
  refused records, reading the policy from the release's tree and the
  workflows are new; the release verification combines the adapted parts
  above with checks written for this repository's bundle. `.github/workflows/publish-crate.yml` follows the shape of the
  renderer's workflow of the same name. Tests in
  `tools/crates/test_publish.py` are new MIT work; no renderer tests are
  copied.
- **crates.io upload behaviour.** The refusal classes and the trusted
  publisher matching rely on facts read in crates.io's
  `src/controllers/krate/publish.rs` and its trusted-publishing token
  exchange at revision `e0edaf98831e94b4a6d0969b957bda0094a45f3d` of
  <https://github.com/rust-lang/crates.io> (MIT, Copyright (c) 2014 The
  Rust Project Developers): that for a trusted-publishing token the 401 and
  403 answers come before the archive is read, and the only 429, the daily
  limit of new versions for a crate, comes inside the database transaction
  before the archive is stored and is rolled back; and that a config without
  an environment matches any run of its workflow file. No
  implementation, tests or source text are copied or adapted.

- **Model Context Protocol.** `crates/tilewright-mcp` and `docs/MCP.md`
  implement MCP revision 2026-07-28 from its specification, consulted on
  2026-11-14 at <https://modelcontextprotocol.io/specification/2026-07-28>:
  the pages Overview (`basic/index`, messages, error codes, `_meta` and
  statelessness), Versioning and Compatibility (`basic/versioning`), stdio
  (`basic/transports/stdio`, with its backward-compatibility section),
  Cancellation (`basic/patterns/cancellation`), Discovery
  (`server/discover`), Tools (`server/tools`), Pagination
  (`server/utilities/pagination`), Caching (`server/utilities/caching`),
  the Key Changes page (`changelog`) and the Schema Reference (`schema`),
  whose source is `docs/specification/2026-07-28` in
  <https://github.com/modelcontextprotocol/modelcontextprotocol>, last
  changed at revision `38353251fab3bdeead03dd8283b9e66076ce1a7f`. That
  repository's `LICENSE` puts new specification contributions under
  Apache-2.0 and earlier ones under MIT. Revision 2025-11-25, at
  revision `9850cae8923b0ee3d19a4476f1010b97aa161a55` of the same
  repository, was read for what changed. JSON-RPC 2.0's message rules and
  error codes are from <https://www.jsonrpc.org/specification>. Only
  protocol facts (message shapes, method, field and `_meta` key names,
  error codes) are used; no specification text, schema file or SDK code is
  copied or adapted, and the server and its tests are new MIT work.

## Owner publication API contract research

The owner-first-publication work consults the primary crates.io OpenAPI
contracts and `src/controllers/user/me.rs` at revision
`8ba796b818baaee0bbd6a813b0524949ce511ca7` of
<https://github.com/rust-lang/crates.io>. This source is available under MIT
(`LICENSE-MIT`, Copyright (c) 2014 The Rust Project Developers). Only endpoint
authentication and response contract facts inform the new helper and its
documentation; no implementation, tests, fixtures or source text are copied
or adapted. The upload framing reference is the Cargo Book's
[Registry Web API](https://doc.rust-lang.org/cargo/reference/registry-web-api.html).

The owner ceremony and create-once intent design in `tools/crates/owner_publish.py`
consult the bootstrap flow in tilewright-renderer's `tools/crate_release.py`,
revision `322f2828ccc323d2cf5286f7392d1bbfd9c97041`
(<https://github.com/seraphinastudios/tilewright-renderer>), MIT, Copyright (c)
2026 Seraphina Studios. Its notice and license are preserved by the root
`LICENSE`. The World helper is a new, narrowed implementation: one manually
confirmed first-publication batch, fixed public owner, verified candidate
bundle and global intent fencing, without trusted-publisher automation or
registry credential discovery. No renderer implementation or tests are copied.

New-crate pacing also consults the renderer's bootstrap pacing at revision
`5855fd2ab824eecbbae47ea2ceb1da9b9fb8b44c` (same repository, license and notice),
and the crates.io rate-limit configuration contract at the crates.io revision
above. Only policy/ordering facts inform the new World pacing and mocked-clock
tests; no implementation or tests are copied.
