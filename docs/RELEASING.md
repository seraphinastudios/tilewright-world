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
`THIRD_PARTY_NOTICES.md`, `THIRD_PARTY_LICENSES.txt` and
`RUST_LIBRARY_COPYRIGHT.html`:

- `THIRD_PARTY_LICENSES.txt` holds the license files of every crate the
  binary compiles in for that platform. `tools/release/package.py`
  generates it from `cargo metadata`, and a release stops if such a crate
  ships no license file. In the Linux archives it also holds musl's
  copyright notice (`tools/release/licenses/musl-COPYRIGHT`), because those
  binaries link musl statically.
- `RUST_LIBRARY_COPYRIGHT.html` holds the copyright notices of the Rust
  standard library, which every binary links in. It is the
  `share/doc/rust/COPYRIGHT-library.html` that the pinned Rust toolchain
  ships, unchanged, and a release stops if that toolchain is not the one
  `rust-toolchain.toml` names.

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
  stops the release. Releases from maintenance branches are off for now
  (see "Repository settings it relies on").
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
| Push to `main` | The plan job. If the commits since the last release call for one, the whole release. |
| Push before any `vX.Y.Z` tag exists | The plan job only, which says the first release has to be started by hand. Merging never cuts the first release. |
| Run workflow (`workflow_dispatch`) on `main` | The same as a push, except that it can also cut the first release |
| Run workflow on a maintenance branch `MAJOR.MINOR.x` | The plan job only, which says what it would release; publishing from maintenance branches is off |
| Pull request that changes the release workflows, `scripts/release/`, `tools/release/`, `tools/build-wasm.sh`, `packages/tilewright-pack/`, `rust-toolchain.toml`, `Cargo.lock` or a `Cargo.toml` | The tests, plan, five builds, npm build and packaging under the version `0.0.0-check.N` (N is the pull request number), as a check. Nothing is attested or published. |

To cut the first release, run the Release workflow on `main`, either from
the Actions tab or with:

```sh
gh workflow run release.yml --ref main -R seraphinastudios/tilewright-world
```

If a merge lands while that run is still waiting for an earlier Release run
on `main`, GitHub cancels the waiting run in favour of the merge's run,
which does not cut a first release. The cancelled run says so in the
Actions tab. Start the workflow again once no Release run is queued on
`main`. Before the first release, those runs only plan, so they take a
minute or two.

Releases of a branch run one at a time. If several merges land while a
release runs, the next run releases them together. Each branch queues on
its own, so a maintenance release never displaces a release of `main`.

## The pipeline

| Job | Runner | Permissions | Does |
| --- | --- | --- | --- |
| Plan release | ubuntu-24.04 | `contents: read` | Runs the policy, planner and packaging tests, then `scripts/release/plan.mjs`, which finds the last release tag the branch contains, analyzes the commits since it, and decides the version and whether to publish. Stops if that last tag has no published release. |
| Revalidate | ubuntu-24.04 | `contents: read` | Runs `validate.yml`, so `tools/validate.sh`, on the released commit |
| Build TARGET | one per platform | `contents: read` | `tools/release/build.sh`: builds with the pinned toolchain and `--locked`, then runs the binary to validate, compile and verify `fixtures/basic` |
| Build the npm package | ubuntu-24.04 | `contents: read` | `tools/build-wasm.sh`, then `npm pack` at the release version (only when `packages/tilewright-pack/package.json` exists) |
| Package release assets | ubuntu-24.04 | `contents: read` | `tools/release/package.py assemble`: archives, license bundles, the source archive and `SHA256SUMS`; checks that all five platforms compiled the fixture to the same pack |
| Attest build provenance | ubuntu-24.04 | `contents: read`, `id-token: write`, `attestations: write` | Verifies the assets, then `actions/attest-build-provenance` for every asset and each bare binary |
| Publish the GitHub release | ubuntu-24.04 | `contents: write` | Creates `vX.Y.Z` and `go/vX.Y.Z` at the commit in one atomic push, which fails if either exists elsewhere. Then it publishes the release on the tag with every asset at once, and checks that the release is immutable, that both tags point at the commit, and that GitHub's asset digests equal `SHA256SUMS`. |
| Publish to npm | ubuntu-24.04 | `contents: read`, `attestations: read`, `id-token: write`, environment `npm` | Publishes the release's npm tarball for a release from `main`; skipped unless npm publication is on |

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
  --cert-identity https://github.com/seraphinastudios/tilewright-world/.github/workflows/release.yml@refs/heads/main \
  --source-ref refs/heads/main --source-digest COMMIT
```

`COMMIT` is the commit the release's tag points at; the release notes
print this command with it filled in. The check shows that the file was
built by `release.yml` running on `main` at that commit. Pin the ref: any
writer to the repository can run a modified `release.yml` on another
branch, and its attestations name the same workflow path with that
branch's ref. Only `main` changes solely through reviewed pull requests.
For a release from a maintenance branch, use that branch's ref instead,
and see below. The attestations are public in the repository's
Attestations page and in Sigstore's transparency log.

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
  forbids deleting or moving `refs/tags/v[0-9]*` and `refs/tags/go/v[0-9]*`,
  so it also protects the Go tags, which have no GitHub release of their
  own. The Go checksum database also rejects changed content for any
  version that was fetched once.
- **The Actions allowlist**, for checkout, setup-node, upload-artifact,
  download-artifact and attest-build-provenance.

Repository writers are trusted. Anyone who can push a branch can run
workflow code of their own with `contents: write` and create tags and
releases outside this pipeline, and no workflow file can prevent that.
What tells pipeline releases apart is their provenance: verify it against
`release.yml@refs/heads/main` as shown above, as npm publication does.
Closing the gap fully needs a ruleset that lets only a dedicated GitHub
App create `refs/tags/v*` and `refs/tags/go/v*`, with the app's credential
in an environment limited to `main`, and `release.yml` creating tags and
releases with that credential instead of `GITHUB_TOKEN`.

Only the default branch is protected by rulesets. A maintenance branch
`MAJOR.MINOR.x` is not, so any writer could push a release to one.
Releases therefore come only from `main`: a maintenance branch plans its
release but never publishes it. To turn maintenance releases on, protect
`[0-9]*.[0-9]*.x` with the same rulesets as `main`, then, in one pull
request, set `MAINTENANCE_RELEASES` to `true` in
`scripts/release/release-policy.cjs` and add the branch pattern to the push
trigger in `release.yml`. npm publication stays limited to `main` either
way.

## Turning on npm publication

The "Publish to npm" job publishes a release's npm tarball, byte for byte,
in the same run that released it, so the provenance npm records names the
release's commit. It first checks that the release is immutable, that the
tarball matches `SHA256SUMS` and GitHub's asset digest, that its build
provenance was signed by `release.yml` running on `main` at the release's
commit, and that it holds `@seraphinastudios/tilewright-pack` at the
release's version. It uses npm trusted publishing (OIDC), so no npm token or
secret is involved. The job is skipped unless the repository variable
`TILEWRIGHT_NPM_PUBLISH` is `true`, and it runs only for releases from
`main`.

To turn it on:

1. Create the `seraphinastudios` organization on npmjs.com.
2. npm configures trusted publishers in a package's settings, so the
   package has to exist first. Publish one release's tarball by hand after
   verifying it as above:
   `npm publish seraphinastudios-tilewright-pack-X.Y.Z.tgz --access public`.
3. Create the repository environment `npm`, and under "Deployment branches
   and tags" allow only `main`. This is required. Anyone who can push a
   branch can run a modified `release.yml` on it, and the environment's
   branch rule is what keeps such a run from publishing. Add required
   reviewers as well if each publication should wait for approval.
4. In the package's settings on npmjs.com, add a trusted publisher:
   GitHub Actions, organization `seraphinastudios`, repository
   `tilewright-world`, workflow `release.yml`, environment `npm`. Under
   "Allowed actions", allow `npm publish`. The job publishes directly, and
   configurations created after 3 September 2026 allow only
   `npm stage publish` unless direct publishing is selected.
5. Set the repository variable `TILEWRIGHT_NPM_PUBLISH` to `true`.

From then on, each release from `main` is published to npm with the
`latest` dist-tag. Releases cut before that are not published
automatically. Publish one by hand after verifying its tarball as above.
To turn publishing off again, delete the variable or set it to anything
else.

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
- **"Publish the GitHub release" failed before publishing**: the tags may
  exist, and a draft release may be left, but nothing is published. Re-run
  the failed jobs within 30 days, while the run's artifacts last. The re-run
  deletes drafts left for the tag and publishes the release afresh.
  The re-run accepts tags the failed attempt created at the same commit.
  The ruleset keeps such tags from being deleted, so the plan job of every
  later run on that branch stops until that release is published.
- **"Publish the GitHub release" failed its checks after publishing**: the
  release exists. Do not delete an immutable release. Read the error, fix
  the cause, then re-run the failed jobs: the re-run skips publishing,
  checks the existing release again, and goes on to npm.
- **The release came out mutable** (immutable releases were off when it was
  published): GitHub cannot make it immutable afterwards, so its checks
  keep failing, and the plan job of every later run on the branch stops at
  it. To repair it, turn immutable releases back on, delete the release
  and its two tags (which takes the owner's bypass of the tag ruleset), and
  re-run the failed jobs within 30 days. The re-run creates both tags and
  the release again from the same attested assets.
- **"Publish to npm" failed**: re-run it. It checks the release again and
  publishes from the release's commit. If npm already has that version,
  the job compares npm's copy with the release's tarball and succeeds if
  they are the same. It cannot set a dist-tag, so if an earlier attempt
  published the version without one, it prints the `npm dist-tag add`
  command to run. npm's registry can take a few minutes to list a new
  version: if the re-run fails because the version already exists, re-run
  it again later.
