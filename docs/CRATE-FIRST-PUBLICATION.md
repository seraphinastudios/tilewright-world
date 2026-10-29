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
merged to `main`, select a successful `release.yml` source release. Do not use
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

Execution tooling must retain this limitation explicitly. Any owner-confirmed
identity ceremony, exact-byte upload and durable cross-host intent requires its
own review before execution becomes available. An uncertain upload response
cannot be resolved by observing a later 404 and retrying: the registry may still
be processing the request. Preserve intent evidence and investigate publicly
visible checksums and ownership; conflicting or missing evidence stops recovery.

No publication command is currently provided by this helper. The remaining
owner action is to approve the exact release and the reviewed identity/execution
procedure, then execute that procedure locally. Preparation work is not that
approval.

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
