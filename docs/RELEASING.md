# Releasing tilewright-world

Releases work as in tilewright-renderer and tilewright-editor. Every merge
to `main` is released once the first release exists. Releases are immutable
GitHub releases tagged `vX.Y.Z`, and every asset has a SHA-256 digest and a
build provenance attestation. `.github/workflows/release.yml` does the work.

## What a release contains

| Asset | Contents |
| --- | --- |
| `tilewright-X.Y.Z-linux-amd64.tar.gz` | The CLI for Linux x86_64, statically linked (musl) |
| `tilewright-X.Y.Z-linux-arm64.tar.gz` | The CLI for Linux ARM64, statically linked (musl) |
| `tilewright-X.Y.Z-macos-amd64.tar.gz` | The CLI for macOS on Intel |
| `tilewright-X.Y.Z-macos-arm64.tar.gz` | The CLI for macOS on Apple silicon |
| `tilewright-X.Y.Z-windows-amd64.zip` | The CLI for Windows x86_64, with the C runtime linked in |
| `tilewright-world-X.Y.Z-source.tar.gz` | `git archive` of the released commit |
| `seraphinastudios-tilewright-pack-X.Y.Z.tgz` | The npm package `@seraphinastudios/tilewright-pack`, the browser pack reader, once `packages/tilewright-pack` exists |
| `SHA256SUMS` | The SHA-256 digest of every other asset |

Each CLI archive holds one directory, `tilewright-X.Y.Z-PLATFORM/`, with
`tilewright` (`tilewright.exe` on Windows), `LICENSE`, `PROVENANCE.md`,
`THIRD_PARTY_NOTICES.md` and `THIRD_PARTY_LICENSES.txt`. The last file holds
the license files of every crate the binary compiles in for that platform.
`tools/release/package.py` generates it from `cargo metadata`, and a release
stops if such a crate ships no license file.

The same release also tags the Go module
`github.com/seraphinastudios/tilewright-world/go` as `go/vX.Y.Z` at the
same commit, so `go get github.com/seraphinastudios/tilewright-world/go@vX.Y.Z`
gets the Go reader of that release.

A released binary reports both versions:

```console
$ tilewright --version
tilewright 0.1.0 (release v0.4.0)
```

The first number is the version of the crates, which packs record as
their `compiler`. As in the renderer, it is independent of release tags, and
it changes only when the crates' version is bumped. The second number is
the release. A build from source prints only the first number, unless
`TILEWRIGHT_RELEASE` was set when it was compiled.

Not released: no crate goes to crates.io, and the npm package is not
published to the npm registry until the owner turns that on (see below).

## Versions

`scripts/release/release-policy.cjs` holds the policy. `.releaserc.json`
configures the commit analyzer with it, as in the other two repositories:

- Every release from `main` bumps the minor version. Patch versions belong
  to maintenance branches named `MAJOR.MINOR.x`. A maintenance branch can
  only release patches of its own line, so a `feat` or breaking commit there
  stops the release.
- A squash merge's commit message is the pull request's Conventional
  Commits title. A commit without a type releases nothing.
- The first release is `0.1.0`. Releases stay at `0.x` until world format
  1.0 is frozen: until then a breaking change (`feat!:` or a
  `BREAKING CHANGE:` footer) bumps the minor version, as Cargo and Go treat
  `0.x` minor versions as incompatible. The pull request that freezes
  format 1.0 sets `PRE_1_0` to `false` in the policy and uses a `!` title,
  which makes its release `1.0.0`.
- Go tags use the release's number. The Go module stays at `v0` as long
  as releases do, as `go/README.md` promises. A release that does not
  change `go/` still tags it, which Go's minimal version selection makes
  harmless. After 1.0, a major release would need a `/v2` module path, so
  the Go numbering will be revisited then.

## When releases happen

| Event | What runs |
| --- | --- |
| Push to `main` or `MAJOR.MINOR.x` | The plan job. If the commits since the last release call for one, the whole release. |
| Push before any `vX.Y.Z` tag exists | The plan job only, which says the first release has to be started by hand. Merging never cuts the first release. |
| Run workflow (`workflow_dispatch`) on `main` or a maintenance branch | The same as a push, except that it can also cut the first release |
| Pull request that changes the release workflows, `scripts/release/`, `tools/release/`, `tools/build-wasm.sh`, `packages/tilewright-pack/`, `rust-toolchain.toml`, `Cargo.lock` or a `Cargo.toml` | The tests, plan, five builds, npm build and packaging under the version `0.0.0-check.N` (N is the pull request number), as a check. Nothing is attested or published. |

To cut the first release, run the Release workflow on `main`, either from
the Actions tab or with:

```sh
gh workflow run release.yml --ref main -R seraphinastudios/tilewright-world
```

Releases run one at a time. If several merges land while a release runs,
the next run releases them together.

## The pipeline

| Job | Runner | Permissions | Does |
| --- | --- | --- | --- |
| Plan release | ubuntu-24.04 | `contents: read` | Runs the policy, planner and packaging tests, then `scripts/release/plan.mjs`, which finds the last release tag the branch contains, analyzes the commits since it, and decides the version and whether to publish |
| Revalidate | ubuntu-24.04 | `contents: read` | Runs `validate.yml`, so `tools/validate.sh`, on the released commit |
| Build TARGET | one per platform | `contents: read` | `tools/release/build.sh`: builds with the pinned toolchain and `--locked`, then runs the binary to validate, compile and verify `fixtures/basic` |
| Build the npm package | ubuntu-24.04 | `contents: read` | `tools/build-wasm.sh`, then `npm pack` at the release version (only when `packages/tilewright-pack/package.json` exists) |
| Package release assets | ubuntu-24.04 | `contents: read` | `tools/release/package.py assemble`: archives, license bundles, the source archive and `SHA256SUMS`; checks that all five platforms compiled the fixture to the same pack |
| Attest build provenance | ubuntu-24.04 | `contents: read`, `id-token: write`, `attestations: write` | Verifies the assets, then `actions/attest-build-provenance` for every asset and each bare binary |
| Publish the GitHub release | ubuntu-24.04 | `contents: write` | Checks the tag is new, publishes the release with every asset at once, then checks that it is immutable, that its tag points at the commit, and that GitHub's asset digests equal `SHA256SUMS` |
| Tag the Go module | ubuntu-24.04 | `contents: write` | Creates `go/vX.Y.Z` at the commit, or accepts it if it is already there |
| Start npm publication | ubuntu-24.04 | `actions: write` | Dispatches `publish-npm.yml`; skipped unless npm publication is on |

The build runners are `ubuntu-24.04` (x86_64 Linux), `ubuntu-24.04-arm`
(ARM64 Linux), `macos-15-intel`, `macos-15` (Apple silicon) and
`windows-2025`. Each target builds natively, so its binary runs on the same
runner, and there are no cross toolchains. The Linux targets use musl, so
the binaries run on any distribution. Their C code (SQLite, Zstandard) is
compiled with Ubuntu's `musl-gcc`. GitHub has said `macos-15-intel` is its
last Intel macOS image. When it is retired, build `x86_64-apple-darwin` on
`macos-15` with `rustup target add`, or drop the target.

Every action is pinned to a full commit SHA and is on the organization's
allowlist. The planner's npm tools are locked with integrity hashes in
`scripts/release/package-lock.json` and installed with `npm ci
--ignore-scripts` in a job with read-only permissions. No job uses a secret.

Builds are reproducible where practical. The toolchain is pinned by
`rust-toolchain.toml`, dependencies by `Cargo.lock` (`--locked`), the
checkout and Cargo home paths are remapped out of the binaries, and
incremental compilation is off. Archives have fixed modes, owners and
timestamps (the commit's), and gzip headers carry no name or time.
`package.py` is deterministic for identical binaries, which its tests check.
The C compilers come from the runner images, so a rebuild on another machine
can differ.

## Verifying a download

Check digests against `SHA256SUMS`:

```sh
sha256sum --check --ignore-missing SHA256SUMS      # Linux
shasum -a 256 --check --ignore-missing SHA256SUMS  # macOS
```

Check build provenance with the GitHub CLI. This works for any asset,
`SHA256SUMS` included, and for the extracted `tilewright` binary:

```sh
gh attestation verify tilewright-X.Y.Z-linux-amd64.tar.gz \
  --repo seraphinastudios/tilewright-world \
  --signer-workflow seraphinastudios/tilewright-world/.github/workflows/release.yml
```

It shows that the file was built by this repository's release workflow from
the commit the release names. The attestations are public in the
repository's Attestations page and in Sigstore's transparency log.

Immutable releases also carry GitHub's own release attestation. With a
recent GitHub CLI, check it with:

```sh
gh release verify vX.Y.Z -R seraphinastudios/tilewright-world
gh release verify-asset vX.Y.Z tilewright-X.Y.Z-linux-amd64.tar.gz -R seraphinastudios/tilewright-world
```

Go users get the module through the Go module proxy, and the Go checksum
database (`sum.golang.org`) pins its content the first time anyone fetches a
version.

## Repository settings it relies on

The pipeline changes no settings. It relies on:

- **Immutable releases**, which are on. A published release, its tag and its
  assets cannot change. The release job checks `immutable: true` after
  publishing and fails loudly if it is not.
- **The organization ruleset "Tilewright 04 - Immutable release tags"**. It
  forbids deleting or moving `refs/tags/v[0-9]*`. It does not cover the Go
  tags `go/v*`, which have no GitHub release of their own, so an
  administrator could still move or delete one. Adding `refs/tags/go/v[0-9]*`
  to that ruleset would protect them too. The Go checksum database already
  rejects changed content for any version that was fetched once.
- **The Actions allowlist**, for checkout, setup-node, upload-artifact,
  download-artifact and attest-build-provenance.

## Turning on npm publication

`.github/workflows/publish-npm.yml` publishes the npm tarball of a release,
byte for byte. It first checks that the release is immutable, that the
tarball matches `SHA256SUMS` and GitHub's asset digest, that its build
provenance was signed by `release.yml`, and that it holds
`@seraphinastudios/tilewright-pack` at the release's version. It uses npm
trusted publishing (OIDC), so no npm token or secret is involved. Its job is
skipped unless the repository variable `TILEWRIGHT_NPM_PUBLISH` is `true`.

To turn it on:

1. Create the `seraphinastudios` organization on npmjs.com.
2. npm configures trusted publishers in a package's settings, so the
   package has to exist first. Publish one release's tarball by hand after
   verifying it as above: `npm publish seraphinastudios-tilewright-pack-X.Y.Z.tgz --access public`.
3. In the package's settings on npmjs.com, add a trusted publisher:
   GitHub Actions, organization `seraphinastudios`, repository
   `tilewright-world`, workflow `publish-npm.yml`, environment `npm`.
4. Optionally create the repository environment `npm` with required
   reviewers. Without that, GitHub creates it on first use with no
   protection.
5. Set the repository variable `TILEWRIGHT_NPM_PUBLISH` to `true`.

From then on, each release dispatches `publish-npm.yml`. To publish an
earlier release, run it by hand with that release's tag. The newest release
gets npm's `latest` dist-tag, and older ones get `maintenance-MAJOR.MINOR`.
To turn publishing off again, delete the variable or set it to anything else.

## Turning on crates.io publication

Crates are not published, and the pipeline contains nothing for it. To
start, follow the renderer, whose crates go to crates.io from verified
source releases (its `docs/CRATE-RELEASE.md`):

1. Decide which crates are public, mark the rest `publish = false`, and keep
   the workspace version independent of release tags. Bump it, and every
   exact `=X.Y.Z` internal requirement, whenever packaged content changes.
2. Port the renderer's `tools/crate_prepare.py`, `tools/crate_release.py`,
   `policy/crate-release.json`, `publish-crate.yml` and
   `reconcile-crates.yml`. The release then attaches the exact `.crate`
   archives and a manifest, which the attestation covers, and the publisher
   uploads those bytes.
3. Publish each crate once by hand, then configure crates.io trusted
   publishing for `publish-crate.yml` in an environment
   `crates-io-release`. The publisher authenticates with
   `rust-lang/crates-io-auth-action`, which is on the allowlist.

## When a release fails

- **Anything before "Publish the GitHub release"**: nothing was published.
  Fix the cause. The next merge releases everything since the last
  release, or re-run the failed jobs.
- **"Publish the GitHub release" failed before publishing**: no tag exists.
  If a draft release was left behind, delete it, then re-run the failed
  jobs.
- **"Publish the GitHub release" failed its checks after publishing**: the
  release exists. Do not delete it. Read the error, for example immutable
  releases being off, and fix the setting. A release that came out mutable
  stays as it is for the owner to review.
- **"Tag the Go module" failed**: re-run it. It creates the tag or accepts
  the one already at the commit.
- **"Start npm publication" or `publish-npm.yml` failed**: run
  `publish-npm.yml` by hand with the release tag.
