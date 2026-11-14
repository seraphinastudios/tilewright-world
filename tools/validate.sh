#!/usr/bin/env bash
set -euo pipefail

repository=$(git rev-parse --show-toplevel)
cd "${repository}"

python3 -m unittest discover -s tools/crates -p 'test_*.py'
# The release tools' tests run in the Release workflow's plan job too, which
# no ruleset requires.
python3 -m unittest discover -s tools/release -p 'test_*.py'

# A crate version is published once and owned forever: it stays, or moves
# to the next patch or minor version (docs/RELEASING.md, "Crate versions").
# CI compares with the pull request's base or the pushed commit's parent.
crate_version_base=${CRATE_VERSION_BASE:-}
if [[ -z ${crate_version_base} || ${crate_version_base} =~ ^0+$ ]]; then
  crate_version_base=$(git rev-parse HEAD)
fi
python3 tools/crates/prepare.py check-version --base "${crate_version_base}"

cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --all-targets
cargo test --locked --workspace --doc

# The CLI validates the fixture and compiles it to identical packs in
# separate processes.
first=$(mktemp -d /tmp/tilewright-world-pack.XXXXXX)
second=$(mktemp -d /tmp/tilewright-world-pack.XXXXXX)
trap 'rm -rf -- "${first}" "${second}"' EXIT
cargo run --locked --quiet --package tilewright-cli -- validate fixtures/basic
cargo run --locked --quiet --package tilewright-cli -- compile fixtures/basic "${first}"
cargo run --locked --quiet --package tilewright-cli -- compile fixtures/basic "${second}"
pack=$(find "${first}" -name '*.twpack')
cmp "${pack}" "${second}/$(basename "${pack}")"
cargo run --locked --quiet --package tilewright-cli -- inspect "${pack}"
# A source tree and the pack compiled from it hold the same IDs.
cargo run --locked --quiet --package tilewright-cli -- diff fixtures/basic "${pack}"
# So does the terrain fixture, whose packs hold tiles its transitions derive.
cargo run --locked --quiet --package tilewright-cli -- validate fixtures/terrain
cargo run --locked --quiet --package tilewright-cli -- compile fixtures/terrain "${first}/terrain"
cargo run --locked --quiet --package tilewright-cli -- compile fixtures/terrain "${second}/terrain"
terrain=$(find "${first}/terrain" -name '*.twpack')
cmp "${terrain}" "${second}/terrain/$(basename "${terrain}")"

# The MCP server answers server/discover and tools/list on the fixture, and
# its standard output holds those two replies and nothing else
# (docs/MCP.md). It speaks MCP revision 2026-07-28, whose requests carry
# their protocol version in _meta.
meta='"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}'
mcp=$(printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{'"${meta}"'}}' \
  '{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{'"${meta}"'}}' |
  cargo run --locked --quiet --package tilewright-cli -- mcp --root fixtures/basic)
mcp_lines=()
while IFS= read -r line; do mcp_lines+=("${line}"); done <<<"${mcp}"
if [[ ${#mcp_lines[@]} -ne 2 ||
  ${mcp_lines[0]} != '{"jsonrpc":"2.0","id":1,"result":{"resultType":"complete","supportedVersions":["2026-07-28"],'* ||
  ${mcp_lines[1]} != '{"jsonrpc":"2.0","id":2,"result":{"resultType":"complete","tools":['*'"name":"search_ids"'* ]]; then
  printf 'tools/validate.sh: unexpected MCP replies:\n%s\n' "${mcp}" >&2
  exit 1
fi

# A transaction replays to the same bytes in separate processes, applies,
# and its inverse restores the fixture byte for byte.
cp -R fixtures/basic "${first}/source"
for output in "${first}/sealed.jsonc" "${second}/sealed.jsonc"; do
  cargo run --locked --quiet --package tilewright-cli -- tx rebase fixtures/transactions/orchard.jsonc "${first}/source" "${output}"
done
cmp "${first}/sealed.jsonc" "${second}/sealed.jsonc"
# Applying is refused, with nothing written, where publishing does not check
# everything (docs/spec/TRANSACTIONS.md section 9), as where the scratch
# tree is on a container's overlay root: then the round trip runs with
# --unchecked-access, and says so. On CI, whose scratch tree is on a local
# file system, it must apply without it.
apply() {
  local refusal
  if refusal=$(cargo run --locked --quiet --package tilewright-cli -- tx apply "$@" 2>&1); then
    printf '%s\n' "${refusal}"
  elif [[ -z ${CI:-} && ${refusal} == *"pass --unchecked-access to accept that"* ]]; then
    echo "tools/validate.sh: the scratch tree's file system is not checked here, so the round trip runs with --unchecked-access" >&2
    cargo run --locked --quiet --package tilewright-cli -- tx apply --unchecked-access "$@"
  else
    printf '%s\n' "${refusal}" >&2
    return 1
  fi
}
apply "${first}/sealed.jsonc" "${first}/source" "${first}/undo.jsonc"
cargo run --locked --quiet --package tilewright-cli -- validate "${first}/source"
apply "${first}/undo.jsonc" "${first}/source" "${first}/redo.jsonc"
diff -r fixtures/basic "${first}/source"

# The Go pack reader, whose tests run the shared fixtures in fixtures/packs.
if ! command -v go >/dev/null 2>&1; then
  echo "tools/validate.sh: go is not on PATH; install the Go toolchain go/go.mod names" >&2
  exit 1
fi
(
  cd go
  go mod tidy -diff
  go vet ./...
  go test ./...
  unformatted=$(gofmt -l .)
  if [[ -n ${unformatted} ]]; then
    echo "gofmt would change: ${unformatted}" >&2
    exit 1
  fi
)

# The WebAssembly pack reader: built for wasm32, then run in Node against
# the same fixtures, and its package's license file kept current.
if ! command -v node >/dev/null 2>&1; then
  echo "tools/validate.sh: node is not on PATH; install the Node.js packages/tilewright-pack/.node-version names" >&2
  exit 1
fi
tools/build-wasm.sh --check
(
  cd packages/tilewright-pack
  node --test "test/*.test.js"
  node scripts/licenses.mjs --check
)

# Exercise the real unpublished crate graph after the source/Go/browser checks.
# The candidate directory itself must be absent and outside the checkout.
candidate_parent=$(mktemp -d /tmp/tilewright-world-candidates.XXXXXX)
trap 'rm -rf -- "${first}" "${second}" "${candidate_parent}"' EXIT
# macOS exposes /tmp through /private/tmp; retain the original cleanup path.
candidate_output_parent=$(cd -- "${candidate_parent}" && pwd -P)
python3 tools/crates/prepare.py prepare "${candidate_output_parent}/candidates"
python3 tools/crates/prepare.py verify "${candidate_output_parent}/candidates" --build

git diff --check
