#!/usr/bin/env bash
# Creates the release tag and the Go tag at the release revision, and says
# whether the release is already published, for the release job of
# .github/workflows/release.yml.
#
# Both tags are created in one atomic push, so neither exists without the
# other. The push does not force, so it fails if a tag exists at another
# commit, and the release cannot end up on a tag someone else created. A
# re-run accepts tags a failed attempt left at the revision. If the release
# is not published yet, drafts a failed attempt left for its tag are
# deleted, since drafts are private and mutable and the release is created
# afresh.
#
# Appends published=true or published=false to GITHUB_OUTPUT.
#
# usage: VERSION=X.Y.Z TAG=vX.Y.Z GO_TAG=go/vX.Y.Z REVISION=SHA GH_REPO=OWNER/REPO \
#   GH_TOKEN=TOKEN GITHUB_OUTPUT=FILE tools/release/tags.sh
set -euo pipefail

if [[ $# -ne 0 ]]; then
  echo "usage: tools/release/tags.sh (configured through the environment)" >&2
  exit 2
fi
: "${VERSION:?}" "${TAG:?}" "${GO_TAG:?}" "${REVISION:?}" "${GH_REPO:?}" "${GH_TOKEN:?}" "${GITHUB_OUTPUT:?}"
# The version and tags come from the plan job, which runs third-party npm
# code; this job runs none. The tags must be the version's, which the
# assets carry.
if [[ ! ${VERSION} =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ || ${TAG} != "v${VERSION}" || ${GO_TAG} != "go/${TAG}" || ! ${REVISION} =~ ^[0-9a-f]{40}$ ]]; then
  echo "::error::invalid release version, tags or revision: $(printf '%q ' "${VERSION}" "${TAG}" "${GO_TAG}" "${REVISION}")"
  exit 1
fi

missing=()
for tag in "${TAG}" "${GO_TAG}"; do
  # matching-refs lists the refs that start with a name, and an empty list
  # when there are none, so a failed request stops the script instead of
  # passing for a missing tag. (A failed lookup of the tag itself prints
  # gh's error body, not the --jq output.)
  tagged=$(gh api --paginate "repos/${GH_REPO}/git/matching-refs/tags/${tag}" \
    | jq -r --arg ref "refs/tags/${tag}" '.[] | select(.ref == $ref) | .object.type + " " + .object.sha')
  if [[ -z ${tagged} ]]; then
    missing+=("${REVISION}:refs/tags/${tag}")
  elif [[ ${tagged} != "commit ${REVISION}" ]]; then
    echo "::error::${tag} exists and does not point at ${REVISION}"
    exit 1
  else
    echo "::notice::${tag} already points at ${REVISION}"
  fi
done
if [[ ${#missing[@]} -gt 0 ]]; then
  # The token reaches git through the environment, not the command line.
  # A tag someone creates after the lookup still stops the push, which
  # does not force.
  auth=$(printf 'x-access-token:%s' "${GH_TOKEN}" | base64 -w0)
  echo "::add-mask::${auth}"
  GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=http.https://github.com/.extraheader \
    GIT_CONFIG_VALUE_0="AUTHORIZATION: basic ${auth}" \
    git push --atomic origin "${missing[@]}"
fi

# Every release, drafts included, from the list, so that a failed request
# stops the script instead of passing for an unpublished release.
releases=$(gh api --paginate "repos/${GH_REPO}/releases?per_page=100" | jq -c -s 'add // []')
if jq -e --arg tag "${TAG}" 'any(.[]; .tag_name == $tag and .draft == false)' <<<"${releases}" >/dev/null; then
  echo "::notice::${TAG} is already published; checking it"
  echo "published=true" >>"${GITHUB_OUTPUT}"
else
  jq -r --arg tag "${TAG}" '.[] | select(.draft and .tag_name == $tag) | .id' <<<"${releases}" \
    | while read -r draft; do
        echo "::notice::deleting draft release ${draft} left for ${TAG}"
        gh api --method DELETE "repos/${GH_REPO}/releases/${draft}" >/dev/null
      done
  echo "published=false" >>"${GITHUB_OUTPUT}"
fi
