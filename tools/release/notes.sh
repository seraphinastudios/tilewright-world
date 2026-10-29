#!/usr/bin/env bash
# Prints the top of a release's notes: the assets with their SHA-256
# digests, the Go module tag and how to verify a download. GitHub's generated
# notes follow it.
#
# usage: TAG=vX.Y.Z GO_TAG=go/vX.Y.Z GH_REPO=OWNER/REPO BRANCH=main REVISION=SHA \
#   tools/release/notes.sh ASSETS
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: tools/release/notes.sh ASSETS" >&2
  exit 2
fi
assets=$1
: "${TAG:?}" "${GO_TAG:?}" "${GH_REPO:?}" "${BRANCH:?}" "${REVISION:?}"
version=${TAG#v}

describe() {
  case $1 in
    *-linux-amd64.tar.gz) echo "tilewright for Linux x86_64 (static)" ;;
    *-linux-arm64.tar.gz) echo "tilewright for Linux ARM64 (static)" ;;
    *-macos-amd64.tar.gz) echo "tilewright for macOS x86_64" ;;
    *-macos-arm64.tar.gz) echo "tilewright for macOS on Apple silicon" ;;
    *-windows-amd64.zip) echo "tilewright for Windows x86_64" ;;
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
