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
# usage: TAG=vX.Y.Z GO_TAG=go/vX.Y.Z REVISION=SHA GH_REPO=OWNER/REPO GH_TOKEN=TOKEN \
#   GITHUB_OUTPUT=FILE tools/release/tags.sh
set -euo pipefail

if [[ $# -ne 0 ]]; then
  echo "usage: tools/release/tags.sh (configured through the environment)" >&2
  exit 2
fi
: "${TAG:?}" "${GO_TAG:?}" "${REVISION:?}" "${GH_REPO:?}" "${GH_TOKEN:?}" "${GITHUB_OUTPUT:?}"

missing=()
for tag in "${TAG}" "${GO_TAG}"; do
  # When a request fails, gh prints the error body instead of the filter's
  # output, so only a successful lookup says anything about the tag. A
  # failed lookup counts as a missing tag, which is safe: the push below
  # refuses a tag that exists at another commit.
  if ! tagged=$(gh api "repos/${GH_REPO}/git/ref/tags/${tag}" --jq '.object.type + " " + .object.sha' 2>/dev/null); then
    tagged=
  fi
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
  auth=$(printf 'x-access-token:%s' "${GH_TOKEN}" | base64 -w0)
  echo "::add-mask::${auth}"
  GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=http.https://github.com/.extraheader \
    GIT_CONFIG_VALUE_0="AUTHORIZATION: basic ${auth}" \
    git push --atomic origin "${missing[@]}"
fi

# GitHub finds only a published release by its tag.
if published=$(gh api "repos/${GH_REPO}/releases/tags/${TAG}" --jq '.draft == false' 2>/dev/null) && [[ ${published} == true ]]; then
  echo "::notice::${TAG} is already published; checking it"
  echo "published=true" >>"${GITHUB_OUTPUT}"
else
  gh api --paginate "repos/${GH_REPO}/releases" \
    | jq -r --arg tag "${TAG}" '.[] | select(.draft and .tag_name == $tag) | .id' \
    | while read -r draft; do
        echo "::notice::deleting draft release ${draft} left for ${TAG}"
        gh api --method DELETE "repos/${GH_REPO}/releases/${draft}" >/dev/null
      done
  echo "published=false" >>"${GITHUB_OUTPUT}"
fi
