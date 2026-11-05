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
| `seraphinastudios-tilewright-pack-X.Y.Z.tgz` | The npm package `@seraphinastudios/tilewright-pack`, the browser pack reader (`packages/tilewright-pack`), built by `tools/build-wasm.sh` |
| `tilewright-world-X.Y.Z-crate-candidates.tar.gz` | The five `.crate` archives at the crate version, and `candidate.json`. The first release at a crate version publishes them to crates.io (see "Crate versions") |
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
it changes only when the crates' version is bumped (see "Crate versions").
The second number is the release. A build from source prints only the first
number, unless `TILEWRIGHT_RELEASE` was set when it was compiled.

The five public crates are on crates.io. The first release at a crate
version publishes that release's exact crate archives there, through the
"Publish crates" workflow (`.github/workflows/publish-crate.yml`), once the
owner has turned crates.io publication on (see
[Turning on crates.io publication](#turning-on-cratesio-publication)). Until
then no release publishes them. The npm package is not published to the npm
registry until the owner turns that on (see below).

## Preparing Rust crate candidates locally

`tools/crates/candidates.json` lists five intended public crates:
`tilewright-jsonc`, `tilewright-source`, `tilewright-pack`, `tilewright-diff`
and `tilewright-edit`. Their Cargo metadata limits publication to crates.io;
`tilewright-cli` and `tilewright-pack-wasm` have `publish = false`. All five
use the workspace crate version and exact internal dependency versions.
The public crates inherit both the MIT SPDX identifier and the root `LICENSE`
file explicitly, so Cargo includes the complete copyright and license notice
in each archive; the verifier requires those exact notice bytes.
The GitHub release workflow bundles these exact candidates as a checksummed,
attested release asset after building the packaged graph, and its source
revalidation exercises local candidates through `tools/validate.sh`. Neither
step publishes anything: the "Publish crates" workflow uploads an owning
release's bundle (see "Crate versions").

Use a clean committed checkout, the pinned Rust toolchain and a credential-free
Cargo home with the locked dependencies already cached. In the documented
isolated CPU build environment, with the checkout and cache mounted, run:

```sh
python3 -m unittest discover -s tools/crates -p 'test_*.py'
python3 tools/crates/prepare.py prepare /absolute/fresh/world-candidates
python3 tools/crates/prepare.py verify /absolute/fresh/world-candidates --build
```

The output parent must already exist. The output directory must be absent,
outside the checkout, and have no symlink components. Existing outputs are
never reused or overwritten; a failed run can leave an incomplete directory
without `candidate.json`. Use another fresh path after investigating a failure.
Credential environment variables and Cargo credential files are refused. Cargo
uses `--offline` throughout; a missing dependency cache is a failure, not a
reason to request credentials or contact the registry.

Preparation passes all five crates to Cargo's multi-package `package` command
twice with separate target directories (`--locked --offline --no-verify
--registry crates-io`). Cargo's package graph supplies the unpublished internal
dependencies while producing registry manifests. The two sets of `.crate`
bytes must match. The output contains exactly those five archives and
`candidate.json`, which records the clean source revision, toolchain, package
versions, archive SHA-256 digests and per-file inventory with sizes and digests.
The verifier checks archive identity, internal dependency versions, notices,
source revision and safe file paths before using the files. The manifest is
local candidate evidence; it is neither a release nor a provenance attestation,
and its checksums alone do not authenticate a download's origin.

`verify --build` extracts only checked regular files into a fresh temporary
directory. It compiles each packaged crate's tests with all features and builds
a separate consumer of the five exact candidate versions. Temporary Cargo
`patch.crates-io` arguments connect only that extracted graph; production
manifests and candidate archives are unchanged. The packaged tests use source
repository fixtures at runtime, so this check compiles them without running
them. The normal `tools/validate.sh` aggregate runs the source fixture suite and,
after the Go and browser checks, performs actual candidate preparation and
`verify --build` in a fresh temporary directory outside the checkout. It removes
that directory on exit, preserving the same credential-free/offline boundary.
This proves the local packaged graph can build, not that it was downloaded
from crates.io or that the editor works against registry releases. The
"Publish crates" workflow checks that before and after it uploads a crate
version (see "The pipeline").

## New crate names

crates.io refuses trusted-publishing tokens for a crate name that does not
exist yet, so the "Publish crates" workflow publishes only new versions of
the five existing names. A new public crate needs, in one reviewed pull
request, a policy change (`tools/crates/candidates.json` and the lists in
`tools/crates/prepare.py`) and a first-publication procedure written for it.
`tools/crates/owner_publish.py` and
[docs/CRATE-FIRST-PUBLICATION.md](CRATE-FIRST-PUBLICATION.md) published the
five names at 0.1.0. As they stand they cannot publish another name: the
helper is bound to the five names and requires each to have exactly one
version on crates.io. A change to the policy lists also makes older release
trees fail verification, so publish any pending owned crate version first
(see "Crate versions").

## Versions

`scripts/release/release-policy.cjs` holds the policy, with release rules
of the form the commit analyzer of semantic-release takes, as in the other
two repositories. `scripts/release/commits.cjs` reads commit messages as
that analyzer does with the conventionalcommits preset, so the same
commits release the same versions. It has no dependencies, so the release
planner runs only code from this repository, and
`scripts/release/test-commits.cjs` checks it against the analyzer's
answers. As in the analyzer, a `BREAKING CHANGE:` note after a line such
as `---` (a Markdown rule in a pull request body) is not read, so use a `!`
title instead. Two differences are deliberate. The analyzer read the
contents of `-name-` lines such as `-type-` or `-revert-` as commit
properties, which could change a release or make it fail, and
`commits.cjs` ignores them. And where the analyzer failed on a
`BREAKING CHANGE:` note that runs into a trailing `-name-` line, the note
counts.

- Every release from `main` bumps the minor version. Patch versions belong
  to maintenance branches named `MAJOR.MINOR.x`. A maintenance branch can
  only release patches of its own line, so a `feat` or breaking commit there
  stops the release. Releases from maintenance branches are off for now
  (see "Repository settings it relies on").
- Pull requests are squash-merged. The commit's title is the pull
  request's Conventional Commits title, and its message body is the pull
  request's description. The title gives the commit its type. A
  `BREAKING CHANGE:` (or `BREAKING-CHANGE:`) line anywhere in the
  description counts too, in any letter case and also after `*` or `|`,
  unless a `---` or other `-name-` line above it hides it. A commit
  without a type releases nothing, unless it reverts a commit or carries
  such a note.
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

### Crate versions

The five public crates share the workspace crate version
(`workspace.package.version` in `Cargo.toml`) and pin each other exactly
(`=X.Y.Z`), so they always move together. The crate version is independent
of the release version, and only a pull request that changes it on purpose
moves it.

- While the crates are at 0.x, a breaking change to a public API, including
  a public dependency moving to a new major or 0.x minor version, takes the
  next minor version. Anything else takes the next patch version.
- `tools/validate.sh`, in the required "World validation" check, runs
  `tools/crates/prepare.py check-version` against the pull request's base
  (or, on `main`, the commit before): the crate version stays, or moves to
  exactly the next patch or the next minor version. It never moves backward
  or skips a version, and it stays below 1.0.0 while `PRE_1_0` is true in
  `scripts/release/release-policy.cjs`. The pull request that freezes 1.0
  changes that check on purpose.
- The first release from `main` at a crate version owns that version of all
  five crates, for good. The plan job's "Decide crate version ownership"
  step finds it from git tags alone: a release owns crate version V when
  the previous release tag's crate version is lower than V, or there is no
  previous release. Later releases carry archives at the same version with
  other bytes (the revision in `.cargo_vcs_info.json`, the sibling checksums
  in each packaged `Cargo.lock`), but crates.io only ever gets the owning
  release's. Never upload a later release's archive for a published name and
  version. The plan job stops a release whose crate version is lower than
  the previous release's. Any other failure there only leaves the crate
  version undecided: the release goes ahead, and its "Start crate
  publication" job fails afterwards.
- Crate content that changes after a version was released reaches crates.io
  only with the next crate version. The plan job's summary and each later
  release's notes name the crates whose packaged content changed since the
  owning release: their files outside `tests/` and `benches/`, their
  `Cargo.toml`, the workspace dependency requirements they inherit, and the
  root `README.md` and `LICENSE`, which every crate packages. `Cargo.lock`
  and `rust-toolchain.toml` do not reach a library's users and are left
  out. The report stops nothing; bump the crate version when those changes
  should be published.
- While crates.io publication is on, merging a crate version bump publishes
  the five crates at it: the bump's release owns the version, and its
  "Start crate publication" job starts the "Publish crates" workflow.
  Merge everything meant for that version before the bump or with it. A
  merge released after it carries the same version without publishing it.
  If a waiting run is cancelled for a newer merge, the newer merge's release
  carries both and owns the version.
- Publish a pending owned version before changing the workspace's members
  or the crate lists in `tools/crates/candidates.json`. The publisher runs
  `main`'s code against the owning release's own policy files, and it
  refuses a release tree whose members or lists differ from `main`'s.
- To give up a crate version that must never reach crates.io, for example
  one with a defect found before it was published, add it and its owning
  release to the `abandoned` list in `main`'s
  `tools/crates/candidates.json`, as `{"version": "0.2.0", "release":
  "v0.7.0"}`, in a reviewed pull request, and bump the crate version to the
  next patch version. The publisher refuses any listed version and any
  listed release. Only `main`'s list counts.

## When releases happen

| Event | What runs |
| --- | --- |
| Push to `main` | The plan job. If the commits since the last release call for one, the whole release. |
| Push before any `vX.Y.Z` tag exists | The plan job only, which says the first release has to be started by hand. Merging never cuts the first release. |
| Run workflow (`workflow_dispatch`) on `main` | The same as a push, except that it can also cut the first release |
| Run workflow on a maintenance branch `MAJOR.MINOR.x` | The plan job only, which says what it would release; publishing from maintenance branches is off |
| A release from `main` that first carries a crate version | Its "Start crate publication" job starts the "Publish crates" workflow for it while crates.io publication is on, and otherwise prints the command that would. See "Crate versions". |
| Pull request that changes the release workflows, `scripts/release/`, `tools/release/`, `tools/crates/`, `tools/build-wasm.sh`, `packages/tilewright-pack/`, `rust-toolchain.toml`, `Cargo.lock` or a `Cargo.toml` | The tests, plan, five builds, npm build and packaging under the version `0.0.0-check.N` (N is the pull request number), as a check. A pull request into another pull request's branch is planned as one into `main`. Nothing is attested or published: the plan job never plans publication for a pull request, and the attest, release and npm jobs refuse to run for one. |

To cut the first release, run the Release workflow on `main`, either from
the Actions tab or with:

```sh
gh workflow run release.yml --ref main -R seraphinastudios/tilewright-world
```

GitHub keeps one waiting run per branch. If a merge lands while a run you
started is still waiting for an earlier Release run on `main`, GitHub
cancels the waiting run in favour of the merge's run, and the cancelled run
says so in the Actions tab. A merge's run cannot stand in for two kinds of
run: the first-release dispatch, because a merge never cuts the first
release, and a re-run of failed jobs that would finish an unpublished
release, because a merge's run stops at that release. Start such a run
again once no Release run is queued on `main`. Before the first release,
runs only plan, so they take a minute or two.

The same holds the other way round. A re-run of an older run joins the
same queue, and if a merge's run is waiting there, GitHub cancels that
run. Before you re-run an older run's jobs, check that no Release run is
waiting on `main`. Otherwise, run the workflow on `main` once the re-run
is done, so the cancelled merge is released.

Releases of a branch run one at a time. If several merges land while a
release runs, the next run releases them together. Each branch queues on
its own, so a maintenance release never displaces a release of `main`.

## The pipeline

| Job | Runner | Permissions | Does |
| --- | --- | --- | --- |
| Plan release | ubuntu-24.04 | `contents: read` | Runs the policy, planner and packaging tests, then `scripts/release/plan.mjs`, which finds the last release tag the branch contains, analyzes the commits since it, and decides the version and whether to publish. The planner and its tests run only code from this repository. The job checks that the plan is one JSON object for the run's own commit and branch, and that the version and previous tag are release versions. Every later job builds the run's commit, the release job takes the branch from the run's ref and the tags from the version, and `tags.sh` checks the version and previous tag again. Except for pull requests, stops if that last tag has no published, immutable release. |
| Revalidate | ubuntu-24.04 | `contents: read` | Runs `validate.yml`, so `tools/validate.sh`, on the released commit |
| Build TARGET | one per platform | `contents: read` | `tools/release/build.sh`: builds with the pinned toolchain and `--locked`, then runs the binary to validate, compile and verify `fixtures/basic` |
| Build the npm package | ubuntu-24.04 | `contents: read` | `tools/build-wasm.sh`, then `npm pack` at the release version. All the outside code it runs is pinned: the Rust toolchain and the crates in `Cargo.lock` (their build scripts and macros run, as in the CLI builds), the Node that `.node-version` names, and wasi-sdk and wasm-bindgen, checked by SHA-256. The job can only read the repository. Its tarball reaches the release only through packaging, which checks the package's name, version and module |
| Prepare crate candidates | ubuntu-24.04 | `contents: read` | Fetches the locked dependencies into a fresh credential-free Cargo home, then runs offline two-pass packaging and builds the exact candidate graph; no registry writes |
| Package release assets | ubuntu-24.04 | `contents: read` | `tools/release/package.py assemble`: archives, license bundles, the source archive, the revision-checked crate candidate bundle and `SHA256SUMS`; checks that all five platforms compiled the fixture to the same pack |
| Attest build provenance | ubuntu-24.04 | `contents: read`, `id-token: write`, `attestations: write` | Verifies the assets, then `actions/attest-build-provenance` for every asset and each bare binary |
| Publish the GitHub release | ubuntu-24.04 | `contents: write` | Creates `vX.Y.Z` and `go/vX.Y.Z` at the commit in one atomic push, which fails if either exists elsewhere. Then it publishes the release on the tag with every asset at once, and checks that the release is immutable, that both tags point at the commit, and that GitHub's asset digests equal `SHA256SUMS`. Last, it has `sum.golang.org` record the new Go version, which only warns if the proxy is slow. |
| Start crate publication | ubuntu-24.04 | `contents: read`, `actions: write` | For a release from `main`, once it is out: checks the `crates-io-release` environment's settings. If the release owns its crate version, starts the "Publish crates" workflow for it, or, while crates.io publication is off, prints the command that would. Otherwise it only warns if crates.io lacks the crate version, which another release owns. Fails if the plan could not decide the crate version. Checks out nothing |
| Publish to npm | ubuntu-24.04 | `contents: read`, `attestations: read`, `id-token: write`, environment `npm` | Publishes the release's npm tarball for a release from `main`; skipped unless npm publication is on |

The "Publish crates" workflow (`.github/workflows/publish-crate.yml`) runs
only when dispatched on `main`, with the inputs `operation` (`verify` or
`publish`), `release_tag`, `source_revision` (the commit the tag points at)
and `bundle_sha256` (the crate archive bundle's digest). Every job checks
those first. Runs for one release and operation queue one at a time and are
never cancelled. It always runs `main`'s code, which reads the release's
policy files (`Cargo.toml`, each crate's `Cargo.toml`,
`rust-toolchain.toml`, `LICENSE` and `tools/crates/candidates.json`) from
the release's tree as data. So `main` can publish a release made at an
older crate version.

| Job | Permissions | Does |
| --- | --- | --- |
| Verify the release's crate archives | `contents: read`, `attestations: read` | Checks that the release is published, immutable and stable; that `vX.Y.Z` and `go/vX.Y.Z` point at the revision, which is on `main`; that the bundle matches the dispatched digest, GitHub's asset digest and `SHA256SUMS`; GitHub's release attestation and the bundle's build provenance from `release.yml` on `main` at the revision, on a GitHub-hosted runner; the archives' bytes against the release tree's policy; the abandoned list; and that the release owns its crate version. A release that does not stops here, successfully, naming the owner. For the owner, it plans the upload from crates.io and the upload records, and stops if crates.io has a newer crate version and anything is left to upload |
| Resolve the archives against crates.io | `contents: read`, `attestations: read` | Builds a consumer of the five crates with the bundle's toolchain, the release's archives standing in for crates.io's, and every other dependency resolved fresh from crates.io, as a consumer's would be |
| Publish to crates.io | `contents: write`, `attestations: read`, `id-token: write`, environment `crates-io-release` | Only for `operation=publish` while crates.io publication is on. Verifies again, has `rust-lang/crates-io-auth-action` exchange the job's OIDC token for a crates.io token, then uploads the archives crates.io lacks in dependency order (`jsonc`, `source`, `pack`, `edit`, `diff`), each after its dependencies are on crates.io, then checks all five |
| Build a crates.io consumer | `contents: read`, `attestations: read` | Once the upload step has seen all five on crates.io: resolves a consumer of the five from crates.io, checks that its lockfile holds the release's checksums, and builds it |

Only the upload step receives the crates.io token, and it holds no other
GitHub token than the one that creates upload records. The auth action
revokes the crates.io token when the job ends; it would expire after 30
minutes anyway. The OIDC token, though, can be requested in every step of
the publish job, so that job runs nothing but `main`'s standard-library
Python, the runner's GitHub CLI and three pinned actions (checkout, the auth
action and upload-artifact): no Cargo, npm or pip. Cargo runs only in the
resolve and accept jobs, which hold no credentials.

The upload does not use `cargo publish`, which would package the source
again: it sends each archive's exact bytes to the registry's upload
endpoint, with metadata taken from the archive's own `Cargo.toml`.
Before each upload the publisher creates an attempt record,
`refs/tags/crate-upload-attempt/NAME/VERSION/N`: an annotated tag at the
release's commit whose message (canonical JSON) names the release, the run
that built the bundle, the archive's digest, the upload metadata's digest
and the publishing run. Creating the reference is create-only, so no two
runs can make the same attempt. The publisher then writes a `reserved`
receipt, checks that crates.io still lacks the version, sends one upload,
and writes a receipt of the answer.

A refused record, `refs/tags/crate-upload-refused/NAME/VERSION/N`, says
that attempt N is known not to have published, and why. Its reason is
`not-sent` when the run stopped after the attempt record and before the
upload was sent, for example because the receipt could not be written or a
crates.io read failed. It is `http-401`, `http-403` or `http-429` when
crates.io refused the upload definitely. For a trusted-publishing token,
crates.io answers 401 and 403 before it reads the archive. Its only 429 for
such a token is the daily limit of new versions for a crate, which it checks
inside its database transaction, after reading the archive but before
storing it, and rolls back. So after any of the three nothing was
published. The run writes the refused record as soon as it knows, before
anything else, retrying a few times; a record that already exists counts
only if it is exactly this run's. If the record cannot be written either,
the attempt stays unresolved.

After any answer, the run waits up to two minutes for crates.io's API,
index and download to show the version with the archive's checksum,
published by this run through trusted publishing and owned by `zoeyrose`. A
read that fails during that wait counts as "not yet". Then it goes on to
the next crate, or stops after a refusal. A later run makes attempt N+1
only if attempt N has a refused record and crates.io still lacks the
version, and there are at most three attempts. An attempt without a refused
record while crates.io lacks the version is unresolved: no run can tell
whether crates.io will still accept it, so the publisher stops there for
good. Records are never deleted, moved or reused. The receipts, which
hold no response text, are kept for 90 days as the run's artifact
`crate-put-outcomes-RUN-ATTEMPT`. The first publication's intents,
`refs/tags/crate-upload-intent/NAME/0.1.0`, are only read, to check that
they name the owning release.

The build runners are `ubuntu-24.04` (x86_64 Linux), `ubuntu-24.04-arm`
(ARM64 Linux), `macos-15-intel`, `macos-15` (Apple silicon) and
`windows-2025`. Each target builds natively, so its binary runs on the same
runner, and there are no cross toolchains. The Linux targets use musl, so
the binaries run on any distribution. Their C code (SQLite, Zstandard) is
compiled with Ubuntu's `musl-gcc`. GitHub has said `macos-15-intel` is its
last Intel macOS image. When it is retired, build `x86_64-apple-darwin` on
`macos-15` with `rustup target add`, or drop the target.

Every action is pinned to a full commit SHA and is on the organization's
allowlist, and no job restores a cache. The release planner has no
dependencies. The WebAssembly build tools are pinned and checked by
SHA-256 in `tools/build-wasm.sh`. They run in the npm build job and in
Revalidate, which both only read the repository. No job uses a secret.

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
database (`sum.golang.org`) pins a version's content the first time anyone
fetches it. The release job asks for the new version right after
publishing, so that happens at release time. Its last step looks the
version up on `proxy.golang.org`, then on `sum.golang.org`, retrying each
for a few minutes. It checks that the proxy resolved `go/vX.Y.Z` to the
release commit, or, after a start-over (see "When a release fails"), to an
earlier commit with the same `go/` directory and root `LICENSE`, which
is all the module zip holds. It then writes the recorded `h1:` checksums
to the run's summary. The `go` command checks every download against them.

## Repository settings it relies on

The pipeline changes no settings. It relies on:

- **Squash merges only**, with the pull request's title as the commit
  title and its description as the commit message (see "Versions").

- **Immutable releases**, which are on. A published release, its tag and its
  assets cannot change. The release job checks `immutable: true` after
  publishing and fails loudly if it is not.
- **The organization ruleset "Tilewright 04 - Immutable release tags"**. It
  covers `refs/tags/v[0-9]*` and `refs/tags/go/v[0-9]*` in this repository,
  tilewright-editor and tilewright-renderer, and has no bypass list. Its
  rules forbid deleting such a tag and moving it to any other commit
  (non-fast-forward and "Restrict updates"), and allow creating one. This
  is what protects `go/vX.Y.Z`, which has no release. Once a release is
  published, immutable releases also lock its `vX.Y.Z` tag. On top of
  that, the release job has the Go checksum database record each new Go
  version as soon as the release is out (see "Verifying a download").
- **The Actions allowlist**, for checkout, setup-node, upload-artifact,
  download-artifact, attest-build-provenance and the `actions/attest` it
  calls, and `actions/setup-go`, which Revalidate runs through
  `validate.yml`. `dependency-review.yml` also needs
  `actions/dependency-review-action`, and `publish-crate.yml`
  `rust-lang/crates-io-auth-action`, which are on it.
- **The environment `crates-io-release`**, for the "Publish to crates.io"
  job. It admits only the branch `main`, through exactly one deployment
  branch policy, `{"name": "main", "type": "branch"}`, and administrators
  cannot bypass it (`can_admins_bypass` false). It has no reviewers, wait
  timer, secrets or variables: publication is fully automatic. Every
  release's "Start crate publication" job warns if it is set up otherwise,
  or is missing while crates.io publication is on.
- **The repository variable `TILEWRIGHT_CRATES_PUBLISH`**: `true` turns
  crates.io publication on. "Start crate publication" dispatches the
  "Publish crates" workflow only then, and the publish job and its upload
  step check it again.
- **Five crates.io trusted publishers**, one for each public crate, and no
  other. crates.io accepts a config without an environment for any run of a
  workflow file of that name, from any branch, which would bypass the
  environment's branch rule. So each crate has exactly one config, and its
  fields are exactly:

  | Field | Value |
  | --- | --- |
  | `repository_owner` | `seraphinastudios` |
  | `repository_owner_id` | `314492348` |
  | `repository_name` | `tilewright-world` |
  | `workflow_filename` | `publish-crate.yml` |
  | `environment` | `crates-io-release` |

  The workflow file name and the environment name are load-bearing:
  renaming either, or renaming or transferring the repository or the
  organization, breaks publication until all five publishers are set up
  again. `tools/crates/test_publish.py` checks the names.
- **The organization ruleset "Tilewright 05 - World crate upload records"**,
  which the owner creates before the first crate version bump (see
  "Turning on crates.io publication"). It covers
  `refs/tags/crate-upload-attempt/**` and `refs/tags/crate-upload-refused/**`
  in this repository only, with the rules "Restrict deletions" and
  "Restrict updates" and no bypass list. Creating such a tag stays allowed,
  so the publish job's `GITHUB_TOKEN` creates records, and nobody can
  delete or move one. It is not part of "Tilewright 04", which also covers
  the renderer, whose stranded upload intents may need deleting. The first
  publication's `refs/tags/crate-upload-intent/*` are not covered.

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
   branch rule is what keeps such a run from publishing. Create the
   environment before step 5: a job that names an environment that does
   not exist creates it, with no branch rule. Required reviewers are
   possible too, but a release run that waits for approval holds the
   release queue of `main`, so later releases wait until it is approved
   or rejected.
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

The "Publish crates" workflow publishes the five crates at a crate version
from the release that owns it, through crates.io trusted publishing: no
crates.io token is stored anywhere. Nothing is published until the owner
has done the following, in this order. These are owner steps: agents do not
change GitHub or crates.io settings, and do not dispatch the workflow
without the owner's go-ahead.

1. **Create the environment `crates-io-release`** in this repository, before
   any `operation=publish` dispatch: a job that names a missing environment
   creates it with no branch rule. Settings, Environments, New environment,
   `crates-io-release`. Under "Deployment branches and tags" choose
   "Selected branches and tags" and add the branch rule `main`. Untick
   "Allow administrators to bypass configured protection rules". Add no
   required reviewers, wait timer, secrets or variables. Check it:

   ```sh
   gh api repos/seraphinastudios/tilewright-world/environments/crates-io-release \
     --jq '{name, can_admins_bypass, deployment_branch_policy}'
   gh api repos/seraphinastudios/tilewright-world/environments/crates-io-release/deployment-branch-policies \
     --jq '[.branch_policies[] | {name, type}]'
   ```

   The first must show `can_admins_bypass` false and
   `{"custom_branch_policies": true, "protected_branches": false}`, the
   second exactly `[{"name": "main", "type": "branch"}]`.
2. **Add a GitHub trusted publisher to each of the five crates** on
   crates.io, signed in as `zoeyrose`: in the crate's Settings, Trusted
   Publishing, Add, GitHub, enter the repository owner `seraphinastudios`,
   the repository name `tilewright-world`, the workflow filename
   `publish-crate.yml` and the environment `crates-io-release`. Do this for
   `tilewright-jsonc`, `tilewright-source`, `tilewright-pack`,
   `tilewright-diff` and `tilewright-edit`. Leave "Trusted Publishing only"
   off for now (step 7).
3. **Check every config, field for field.** In the same signed-in browser,
   open `https://crates.io/api/v1/trusted_publishing/github_configs?crate=NAME`
   for each of the five names. Each must list exactly one config, with
   `repository_owner` `seraphinastudios`, `repository_owner_id`
   `314492348`, `repository_name` `tilewright-world`, `workflow_filename`
   `publish-crate.yml` and `environment` `crates-io-release`. Delete any
   other config, above all one without an environment. A missing or wrong
   config for one crate shows only when that crate's upload gets HTTP 403,
   partway through a crate version's cohort.
4. **Protect the upload records** with a ruleset for this repository alone,
   before the first crate version bump merges, so its records are protected
   from the start:

   ```sh
   gh api --method POST orgs/seraphinastudios/rulesets --input - <<'EOF'
   {
     "name": "Tilewright 05 - World crate upload records",
     "target": "tag",
     "enforcement": "active",
     "bypass_actors": [],
     "conditions": {
       "ref_name": {
         "include": ["refs/tags/crate-upload-attempt/**", "refs/tags/crate-upload-refused/**"],
         "exclude": []
       },
       "repository_name": {"include": ["tilewright-world"], "exclude": []}
     },
     "rules": [{"type": "deletion"}, {"type": "update"}]
   }
   EOF
   ```

   Or, in the organization's Settings, Repository, Rulesets: New ruleset,
   New tag ruleset, named as above, enforcement Active, no bypass list,
   target repositories by name `tilewright-world` only, target tags
   `crate-upload-attempt/**` and `crate-upload-refused/**`, and only the
   rules "Restrict deletions" and "Restrict updates". Leave "Restrict
   creations" off, or the publish job could not create records. Do not add
   these patterns to "Tilewright 04". Check it:
   `gh api orgs/seraphinastudios/rulesets --jq '.[] | select(.name | startswith("Tilewright 05")) | .id'`,
   then `gh api orgs/seraphinastudios/rulesets/ID --jq '{target, enforcement, bypass_actors, conditions, rules}'`.
5. **Set the repository variable last**, as the statement that steps 1 to 4
   are done:
   `gh variable set TILEWRIGHT_CRATES_PUBLISH --body true -R seraphinastudios/tilewright-world`.
6. **Dry runs.** First without credentials, for v0.1.0, which owns 0.1.0:

   ```sh
   gh workflow run publish-crate.yml --ref main -R seraphinastudios/tilewright-world \
     -f operation=verify -f release_tag=v0.1.0 \
     -f source_revision=70a86f25f9f6c3077991309a65137afb7a08a54d \
     -f bundle_sha256=f3c57f44a6525b3c05113dd2cc5c13845e0f1292a397f8b6c62cb724b40af669
   ```

   Expect the verify job's summary to show all five crates present at
   0.1.0, each with its first-publication intent and nothing to upload, and
   the resolve job to build. Do the same for the latest release, with its
   revision and bundle digest (its "Start crate publication" summary has
   the command): expect it to say that v0.1.0 owns 0.1.0, and nothing more.
   Then the credentialed canary: the v0.1.0 command with
   `-f operation=publish`. Expect the environment to admit the run, the
   OIDC exchange to succeed, no upload and no record, and "Build a
   crates.io consumer" to build 0.1.0 from crates.io. That proves the
   workflow file, the environment and at least one trusted publisher. Only
   a real upload proves all five, which step 3 covers.
7. **After the first crate version this workflow publishes**, once crates.io
   shows all five crates at it with `trustpub_data` naming GitHub, this
   repository and the publish run: decide whether to turn on "Trusted
   Publishing only" for the five crates, which stops any API token from
   publishing their versions. Check first, under crates.io's Account
   Settings, API Tokens, that no token with publish scope for
   `tilewright-*` remains, and revoke any that does.

From then on, merging a crate version bump publishes the five crates at it.
To turn publication off, delete the variable or set it to anything else.
Releases that own a crate version while it is off can be published later
with the command in "When a crate publication fails".

## When a release fails

- **Anything before "Publish the GitHub release"**: nothing was published.
  Fix the cause. The next merge releases everything since the last
  release, or re-run the failed jobs. A failed first release is not
  retried by a merge: run the workflow on `main` again.
- **"Publish the GitHub release" failed before publishing**: the tags may
  exist, and a draft release may be left, but nothing is published. Re-run
  the failed jobs within 30 days, while the run's artifacts last. The re-run
  deletes drafts left for the tag and publishes the release afresh.
  The re-run accepts tags the failed attempt created at the same commit.
  The ruleset keeps such tags from being deleted, so the plan job of every
  later run on that branch stops until that release is published.
  Nothing re-runs the merges whose runs stopped there, so once the release
  is published, run the workflow on `main` to release them.
  A re-run runs the workflow of the run's own commit, so it cannot pick
  up a fix to the workflow, and it fails once the artifacts have expired.
  If a re-run cannot succeed, nothing has been published, and the owner
  can start over:
  1. Check whether
     `https://proxy.golang.org/github.com/seraphinastudios/tilewright-world/go/@v/list`
     lists the version. If it does, the Go proxy holds the module's
     content at that tag. The commit the new run releases must then have
     the same `go/` directory and root `LICENSE`. The module zip includes
     that `LICENSE` because `go/` has no license file of its own.
  2. The tag ruleset has no bypass list, and it also covers
     tilewright-editor and tilewright-renderer. Add `tilewright-world` to
     its excluded repositories, delete both tags, then remove the
     exclusion. Deleting the tags on GitHub leaves any local copies, which
     a plain `git fetch` does not move: delete them too
     (`git tag -d vX.Y.Z go/vX.Y.Z`).
  3. Run the workflow on `main` again. If the failure needed a fix to the
     workflow, merge the fix after step 2 instead, and its merge's run
     cuts the release. If it was the first release, though, a merge does
     not cut it: run the workflow on `main` once the fix has merged. Pull
     request checks skip the unfinished-release check, so the fix's checks
     can pass while the tags still exist.
- **"Publish the GitHub release" failed its checks after publishing**: the
  release exists. Do not delete an immutable release. Read the error, fix
  the cause, then re-run the failed jobs: the re-run skips publishing,
  checks the existing release again, and goes on to npm and "Start crate
  publication".
- **The release came out mutable** (immutable releases were off when it was
  published): GitHub cannot make it immutable afterwards, so its checks
  keep failing, and the plan job of every later run on the branch stops at
  it. To repair it, turn immutable releases back on, and turn the release
  back into a draft, which a mutable release allows:
  `gh api --method PATCH repos/seraphinastudios/tilewright-world/releases/RELEASE_ID -F draft=true`.
  Then re-run the failed jobs within 30 days. The re-run accepts the tags,
  deletes the draft and publishes the release afresh, now immutable, from
  the same attested assets. Then run the workflow on `main` to release the
  merges whose runs stopped at it. After 30 days, delete the release
  instead and start over as described above.
- **The Go checksum step warned or failed**: the release is published and
  the job went on. If the warning says `proxy.golang.org` or
  `sum.golang.org` did not answer, record the version by hand:
  `GOPROXY=https://proxy.golang.org go mod download -json github.com/seraphinastudios/tilewright-world/go@vX.Y.Z`.
  If it warns that git could not compare the proxy's commit with the
  release's, compare the two commits the warning names, by SHA, not by
  tag, since a local `vX.Y.Z` can still name the old commit:
  `git fetch origin COMMIT REVISION && git diff --stat COMMIT REVISION -- go LICENSE`.
  If that prints nothing, the content is the same and nothing is wrong;
  record the version by hand if the step did not. If it prints anything,
  tell the owner, as for the failure below. If the step failed
  because the proxy serves the version from another commit whose `go/` or
  `LICENSE` differ, tell the owner. Go users get that other commit's module
  for this version, and the checksum database pins it. Only a start-over
  that ignored its step 1 leads there. A start-over that kept `go/` and
  `LICENSE` the same only gets a notice.
- **"Publish to npm" failed**: re-run it. It checks the release again and
  publishes from the release's commit. If npm already has that version,
  the job compares npm's copy with the release's tarball and succeeds if
  they are the same. It cannot set a dist-tag, so if an earlier attempt
  published the version without one, it prints the `npm dist-tag add`
  command to run. npm's registry can take a few minutes to list a new
  version: if the re-run fails because the version already exists, re-run
  it again later.

## When a crate publication fails

A crate publication never holds up a release: it starts once the GitHub
release and the Go tag are out, in a run of its own, and nothing waits for
it. To publish an owning release's crate version, or finish publishing it,
dispatch the "Publish crates" workflow for that release:

```sh
tag=vX.Y.Z
repository=seraphinastudios/tilewright-world
gh workflow run publish-crate.yml --ref main -R "$repository" \
  -f operation=publish -f release_tag="$tag" \
  -f source_revision="$(gh api "repos/$repository/commits/$tag" --jq .sha)" \
  -f bundle_sha256="$(gh api "repos/$repository/releases/tags/$tag" \
    --jq ".assets[] | select(.name == \"tilewright-world-${tag#v}-crate-candidates.tar.gz\") | .digest | ltrimstr(\"sha256:\")")"
```

The owning release's "Start crate publication" summary has the same
command filled in. `-f operation=verify` checks everything and uploads
nothing. Dispatch the workflow directly: do not re-run the release run
instead. A re-run of all its jobs plans no release and so starts nothing,
and any re-run joins the release queue, where it can cancel a waiting
merge's run. Upload records are never deleted: each run reads them and goes
on as far as they allow. The run's summary and the receipts artifact
`crate-put-outcomes-RUN-ATTEMPT` show each attempt.

Every later release's "Start crate publication" warns while crates.io lacks
the owned crate version. The owning release's publication is possibly
still in progress then: check its "Publish crates" run first.

| What failed | What happened | What to do |
| --- | --- | --- |
| "Start crate publication" says the plan could not decide the crate version | The release is out; nothing was started | Read the warning of the plan job's "Decide crate version ownership" step. Once the cause is fixed on `main`, dispatch for the owning release as above |
| "Start crate publication" could not dispatch | The release is out; nothing was started | Dispatch as above |
| "Verify the release's crate archives" | Nothing was uploaded or recorded | Read its error, which names the check. Fix a cause on `main`, then dispatch again. A changed or unattested release is not publishable at all. Refusals about upload records, a newer crate version or the abandoned list are below |
| "Resolve the archives against crates.io" | Nothing was uploaded | The crates at this version would not resolve or build for a consumer. The release's archives cannot change: fix it in the next crate version, and consider abandoning this one (see "Crate versions") |
| "Exchange the job's OIDC token for a crates.io token" | Nothing was uploaded or recorded | No trusted publisher matched the run. Check steps 1 to 3 of "Turning on crates.io publication", then dispatch again |
| The upload got HTTP 401, 403 or 429 | crates.io refused attempt N without publishing it. The run created the refused record (reason `http-401`, `http-403` or `http-429`) right away, and stopped. The crates before it in dependency order are published | 403: the crate's trusted publisher is missing or wrong (check step 3), or the crates.io token expired. 401: crates.io could not read the token. 429: crates.io's daily limit of new versions for a crate; wait until it passes. Then dispatch again, which makes attempt N+1. After three attempts with refused records the publisher stops for that crate version; bump to the next patch version instead |
| The upload timed out, got a server error or another answer | Attempt N is unresolved: there is no refused record. If crates.io showed the version from this run within two minutes, the run went on; otherwise it stopped | Wait, then dispatch again: if crates.io now has the version with the release's archive, the run skips it and goes on. If it still does not, no run can tell whether crates.io will accept that upload, so the publisher refuses that crate version for good. After looking into it, bump to the next patch version, which publishes all five again |
| The run stopped after an attempt record and before its upload: a receipt could not be written, a crates.io read failed, or crates.io showed the version meanwhile | Nothing was sent. The run created the refused record with reason `not-sent` and stopped | Dispatch again: crates.io's version is skipped if it is there with the release's archive, and otherwise the run makes attempt N+1 |
| An attempt record with no `put` receipt and no refused record: the run was killed or cancelled before it could write one, or the refused record could not be written | The log says which step stopped, or that the record could not be written. The attempt is unresolved | As for an unresolved attempt: bump to the next patch version |
| crates.io has the version with other bytes, yanked, or without `zoeyrose` as an owner | Nothing was uploaded for it | Find out why. Publish the content as a new crate version |
| crates.io has a newer crate version | That cohort is stale and is not published, by design | Nothing. A run that had started uploading finishes its cohort; a later run for that release refuses. So an older cohort can stay partly published. Its published crates are dependency-closed, since a crate is uploaded only after its dependencies are on crates.io |
| "Confirm the five crates on crates.io", after the upload step succeeded | crates.io's index or download is slow | Dispatch again later; present crates are skipped |
| "Build a crates.io consumer" | The crates are published | crates.io's crates at this version do not resolve or build for consumers. Fix that in the next crate version |
