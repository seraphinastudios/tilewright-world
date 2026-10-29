# First publication of the World crates

First publication is a separate owner decision about a concrete immutable source
release. Merging this tooling, creating GitHub releases and observing public
registry 404 responses do not authorize uploads or reserve names. Registry
publishing automation remains deferred. Never place registry tokens in chat,
command arguments, environment variables, build tools, CI or committed files.

The exact five public coordinates are `tilewright-jsonc`, `tilewright-source`,
`tilewright-pack`, `tilewright-diff` and `tilewright-edit`, all at the workspace
crate version. The source release version can differ. `tilewright-cli` and
`tilewright-pack-wasm` remain private. Do not use `cargo publish` to publish an
already verified candidate: it packages again and can execute build hooks.

## Select and review a concrete source release

After the candidate release support and this helper have passed review and
merged to `main`, select a successful `release.yml` source release. The first
source release requires a separate owner-authorized manual dispatch of the
Release workflow on `main`; ordinary pushes deliberately skip that initial
publication. This helper does not dispatch workflows or create releases. Do not use
PR artifacts, maintenance branches or an unreviewed `latest` selection. Record
these exact fields in an owner-local decision JSON file outside the checkout:

```json
{
  "repository": "seraphinastudios/tilewright-world",
  "release_id": 123,
  "tag": "v0.4.0",
  "revision": "0000000000000000000000000000000000000000",
  "run_id": 456,
  "attempt": 1,
  "bundle_sha256": "0000000000000000000000000000000000000000000000000000000000000000",
  "manifest_sha256": "0000000000000000000000000000000000000000000000000000000000000000"
}
```

The numbers and hashes above are placeholders, not a publication decision.
The source revision must contain the reviewed helper. Review the release ID,
source and Go tags, producing run/attempt, package inventory and notice files,
versions and exact digests. Download `SHA256SUMS` and
`tilewright-world-SOURCE_VERSION-crate-candidates.tar.gz` from that release.
Obtain the manifest digest from the manifest in that bundle, then retain the
bundle and decision together. This review pins bytes; the helper independently
authenticates their release origin.

## Run the read-only preflight

Use Python 3.11 or later, a recent official GitHub CLI supporting immutable
release verification, and a clean checkout of the exact source revision:

```sh
python3 tools/crates/owner_publish.py \
  --decision /absolute/owner-local/decision.json \
  --bundle /absolute/owner-local/tilewright-world-SOURCE_VERSION-crate-candidates.tar.gz \
  --checksums /absolute/owner-local/SHA256SUMS
```

The helper performs only public service reads and official local verification.
It never invokes Cargo, reads a registry credential or writes a registry object.
It checks:

- The pinned immutable, published, stable release in the World repository;
  both source and Go tags; source membership on `main`; the exact successful
  release workflow run and attempt.
- The bundle and checksum asset digests, checksum inventory, official immutable
  release attestation and official build provenance verification. The verified
  certificate must bind the source, repository ID, workflow and producing run
  and attempt; a workflow-controlled predicate claiming a run is insufficient.
- The canonical byte-only bundle verifier, manifest digest, all five archive
  inventories, exact internal dependencies, source revision and license notices.
- Public crates.io name metadata, sparse index and, for existing coordinates,
  the downloaded archive's checksum. Registry disagreement, yanked versions,
  unexpected versions or changed bytes fail closed.
- The release and local checkout again after verification to detect drift.

`absent` is a point-in-time observation, never a reservation. `exact-existing`
means public bytes agree; it does not establish account identity or authorize
another upload. Service errors, rate limits, missing evidence and incompatible
verifier output stop the preflight. There are no fallback registry endpoints,
authenticated redirects or automatic request retries.

## Identity and execution boundary

The current primary crates.io API contract has no supported API-token identity
lookup. `GET /api/v1/me` is an internal browser-cookie endpoint and refuses
API-token authentication. The public `/api/v1/tokens/current` operation revokes
a token; it is not an identity query. Do not supply browser cookies, invent a
whoami request, or assume an authenticated GitHub CLI proves which crates.io
account owns a pasted registry token.

The owner has chosen an explicit human confirmation of the token's account,
matching the renderer's first-publication procedure. This is a human assertion,
not programmatic token authentication. The helper checks the public account
`zoeyrose` with crates.io user ID `437663` (distinct from GitHub user ID
`3865595`), verifies the existing GitHub CLI account and repository write
permission, then checks public crate ownership after each upload. A token from
the wrong account could publish a new crate under that account before the
ownership check stops the batch. Use a newly created short-lived `publish-new`
token from the intended account, limited to the five exact crate names.
Do not widen token scope to work around an identity error.

## Owner execution, only after separate concrete-release approval

Code review and merging this helper do not authorize execution. After the owner
has approved the exact release decision, the owner runs the following from a
clean checkout of that reviewed source revision, in their own POSIX terminal:

```sh
python3 tools/crates/owner_publish.py \
  --decision /absolute/owner-local/decision.json \
  --bundle /absolute/owner-local/tilewright-world-SOURCE_VERSION-crate-candidates.tar.gz \
  --checksums /absolute/owner-local/SHA256SUMS \
  --execute
```

The default command remains read-only. `--execute` repeats the full preflight,
refuses CI or noninteractive use and registry token environment variables, then
shows the exact pending coordinates and pinned release. It requires typing
`PUBLISH AS zoeyrose` once and reads the one-use token through hidden terminal
input. Cancellation before intent creation produces no upload reservation. The
registry token stays in the Python process: it is never supplied to Cargo, a
subprocess, environment, command arguments, files or logs. OS core dumps are
disabled before token input; Python does not guarantee secure memory erasure.
The GitHub CLI uses its existing owner-managed authentication for intent writes
and the repeated execution-phase release checks. This avoids exhausting GitHub's
unauthenticated hourly quota during a five-crate batch; default preflight still
uses public repository evidence reads.
This helper does not configure credentials or retrieve GitHub tokens.

### Account-wide new-name pacing

The registry's current primary implementation defaults to a five-name burst and
one new-name slot every 600 seconds per uploading account; operators can override
those limits. There is no supported remaining-quota lookup. See the pinned
[rate limiter](https://github.com/rust-lang/crates.io/blob/8ba796b818baaee0bbd6a813b0524949ce511ca7/src/rate_limiter.rs)
and [configuration contract](https://github.com/rust-lang/crates.io/blob/8ba796b818baaee0bbd6a813b0524949ce511ca7/src/config/rate_limits.rs).
World therefore defaults to `--burst 0`: wait 610 seconds before **every** pending
new name, including the first. Waiting happens before that crate's fresh source,
dependency and registry checks and before its durable intent. Cancelling a wait
leaves no new intent for that crate. Progress uses a monotonic clock and prints
at most once per minute.

Only if the owner knows the account's recent publication history may they supply
`--burst N` (0–5), their estimate of still-unused immediate slots. This is not a
quota probe or a guarantee. The helper reduces that estimate for already
published World names created in the preceding 50 minutes plus margin. Missing,
invalid or unavailable creation history selects zero immediate slots. World
cannot infer recent renderer or other crate publication from its own manifest;
keep zero when uncertain, after the renderer batch, or when resuming a recent
partial batch. Never assume each repository receives a separate five-name burst.

Run all publication batches for this account sequentially, with no other
publishers consuming its quota during the waits. Five pending World crates at
the default take at least 50 minutes 50 seconds of pacing, plus verification and
network time. For a combined thirteen-name rollout with a known unused initial
five-slot burst, eight refill waits total at least 81 minutes 20 seconds across
the sequential batches. Keep each batch's one-use token valid beyond its displayed
estimate, with ample margin for network/verification delays (for example, at
least 90 minutes for a default five-name World batch), then revoke it immediately
after the batch stops. This helper neither creates nor extends token lifetime.

A provider policy change, other account activity or exhausted quota can still
cause HTTP 429. The helper reports that status, stops without retrying, and keeps
the already-created intent. It never treats 429 or a subsequent 404 as permission
to remove an intent or repeat an attempted PUT; use the separate recovery process
below.

The allowlisted batch runs in dependency order. Before each upload, the helper
checks the source release and clean checkout again, verifies the crate is still
absent, and requires every internal dependency's exact published checksum and
intended owner. Existing exact coordinates are skipped only after checking their
public owner. The helper builds the documented Cargo API metadata from the
already verified normalized manifest and sends the exact held `.crate` bytes;
it never packages or builds while holding a token.

Immediately before each PUT it creates an annotated tag object and atomically
creates `refs/tags/crate-upload-intent/NAME/VERSION` in the World repository.
The tag message records the repository ID, release/run/attempt/source/bundle/
manifest pins, crate coordinate, archive and upload-metadata digests, and
intended owner. The create-reference operation fences competing processes and
hosts even if both observed an earlier 404. A failed or ambiguous intent creation
never permits an upload. Intent creation can leave an unreferenced tag object
on failure; the helper does not delete objects or reset reservations.

Every PUT is attempted at most once, to the fixed crates.io HTTPS endpoint;
redirects are refused, responses are bounded and server error text is never
printed. After HTTP success it performs up to twelve public visibility checks,
five seconds apart, then checks the exact downloaded archive and owner before
starting the next crate. Inconsistent API/index state, conflicting bytes, changed
source, ownership mismatch, unavailable service or ambiguous response stops the
batch. Already completed crates remain published. Revoke the one-use token after
success, cancellation or failure; the helper never revokes it automatically.

## Interrupted or partial publication

Keep the exact original decision and bundle. Run the default read-only preflight
to inspect public state. An exact existing archive can be reconciled with its
owner on a later explicit owner invocation; it will not be uploaded again. The
same invocation can then proceed to crates that have no prior intent, after the
owner's confirmation. A crate that remains absent but has any existing intent
is blocked, even if the previous process died before PUT. A 404 cannot establish
that an earlier request was never accepted or is no longer processing.

Never delete, move or reuse an intent to retry. There is no retry/reset override.
Unresolved intents, conflicting bytes or wrong owners need a separate owner
investigation and reviewed recovery decision; this helper cannot repair them.
GitHub repository writers remain trusted to preserve intent refs, just as they
are trusted to preserve the release workflow. This tool does not add ref
protection settings or claim its tags are repository-policy immutable.

## Protocol references

The [Cargo Registry Web API](https://doc.rust-lang.org/cargo/reference/registry-web-api.html)
defines archive-upload framing. The
[GitHub CLI attestation verifier](https://cli.github.com/manual/gh_attestation_verify)
describes certificate versus workflow-controlled claims, and
[immutable release asset verification](https://cli.github.com/manual/gh_release_verify-asset)
checks the official release attestation. The
[Sigstore verification result documentation](https://github.com/sigstore/sigstore-go/blob/main/docs/verification.md)
describes the certificate fields used to bind a workflow invocation.
The crates.io identity limitation is confirmed by its
[primary endpoint contract](https://github.com/rust-lang/crates.io/blob/8ba796b818baaee0bbd6a813b0524949ce511ca7/src/controllers/user/me.rs)
and public/internal OpenAPI schemas at that revision; no outside implementation
is copied. See [provenance](../PROVENANCE.md).
