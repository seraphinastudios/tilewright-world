#!/usr/bin/env bash
# Prints the top of a release's notes: the assets with their SHA-256
# digests, the Go module tag and how to verify a download. GitHub's generated
# notes follow it.
#
# usage: TAG=vX.Y.Z GO_TAG=go/vX.Y.Z GH_REPO=OWNER/REPO BRANCH=main REVISION=SHA \
#   [CRATE_VERSION=X.Y.Z CRATE_OWNER=true|false CRATE_OWNER_TAG=vX.Y.Z CRATE_CHANGED='NAME ...'] \
#   tools/release/notes.sh ASSETS
#
# The CRATE_ variables are the plan job's crate version outputs. Notes
# cannot change once the release is out, and the tags are already pushed
# when they are written, so these never stop the script: a missing or
# malformed value only gives neutral wording.
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: tools/release/notes.sh ASSETS" >&2
  exit 2
fi
assets=$1
: "${TAG:?}" "${GO_TAG:?}" "${GH_REPO:?}" "${BRANCH:?}" "${REVISION:?}"
version=${TAG#v}
release_version='(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)'
crate_version=${CRATE_VERSION:-}
crate_owner=${CRATE_OWNER:-}
crate_owner_tag=${CRATE_OWNER_TAG:-}
if [[ ! ${crate_version} =~ ^${release_version}$ ]]; then
  crate_version=
fi
if [[ ! ${crate_owner_tag} =~ ^v${release_version}$ ]]; then
  crate_owner_tag=
fi
changed=()
read -r -a words <<<"${CRATE_CHANGED:-}"
for crate in "${words[@]}"; do
  if [[ ${crate} =~ ^tilewright-[a-z]+$ && ${#changed[@]} -lt 5 ]]; then
    changed+=("${crate}")
  fi
done

describe() {
  case $1 in
    *-linux-amd64.tar.gz) echo "tilewright for Linux x86_64 (static)" ;;
    *-linux-arm64.tar.gz) echo "tilewright for Linux ARM64 (static)" ;;
    *-macos-amd64.tar.gz) echo "tilewright for macOS x86_64" ;;
    *-macos-arm64.tar.gz) echo "tilewright for macOS on Apple silicon" ;;
    *-windows-amd64.zip) echo "tilewright for Windows x86_64" ;;
    *-crate-candidates.tar.gz) echo "Rust crate archives${crate_version:+ (crate version ${crate_version})}" ;;
    *-source.tar.gz) echo "Source code" ;;
    *.tgz) echo "npm package of the browser pack reader" ;;
    *) echo "Release asset" ;;
  esac
}

echo "## Assets"
echo
echo "| Asset | Contents | SHA-256 |"
echo "| --- | --- | --- |"
while read -r digest name; do
  echo "| \`${name}\` | $(describe "${name}") | \`${digest}\` |"
done <"${assets}/SHA256SUMS"
echo
echo "\`SHA256SUMS\` lists the same digests."
echo
# What the immutable notes say about crates.io is intent only; the "Publish
# crates" run and crates.io itself show the result.
if [[ -n ${crate_version} && ${crate_owner} == true ]]; then
  echo "This release owns crate version ${crate_version}. The \"Publish crates\" workflow uploads these exact archives to crates.io; see crates.io for the result."
elif [[ -n ${crate_version} && ${crate_owner} == false && -n ${crate_owner_tag} ]]; then
  echo "Crate version ${crate_version} was first released in ${crate_owner_tag}; crates.io receives that release's archives, not these."
  if [[ ${#changed[@]} -gt 0 ]]; then
    list=
    for crate in "${changed[@]}"; do
      list+="${list:+, }\`${crate}\`"
    done
    echo
    echo "Changed since ${crate_owner_tag}, so on crates.io only with a new crate version: ${list}."
  fi
else
  echo "The crate archive bundle is not by itself a crates.io publication. docs/RELEASING.md says which release's archives crates.io receives."
fi
echo
echo "The Go pack reader is tagged \`${GO_TAG}\` at the same commit:"
echo
echo '```sh'
echo "go get github.com/${GH_REPO}/go@v${version}"
echo '```'
echo
echo "## Verify a download"
echo
echo '```sh'
echo "sha256sum --check --ignore-missing SHA256SUMS"
echo "gh attestation verify tilewright-${version}-linux-amd64.tar.gz --repo ${GH_REPO} \\"
echo "  --cert-identity https://github.com/${GH_REPO}/.github/workflows/release.yml@refs/heads/${BRANCH} \\"
echo "  --source-ref refs/heads/${BRANCH} --source-digest ${REVISION}"
echo "gh release verify-asset ${TAG} tilewright-${version}-linux-amd64.tar.gz --repo ${GH_REPO}"
echo '```'
echo
echo "See [docs/RELEASING.md](https://github.com/${GH_REPO}/blob/${TAG}/docs/RELEASING.md)."
echo
