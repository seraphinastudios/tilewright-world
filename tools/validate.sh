#!/usr/bin/env bash
set -euo pipefail

repository=$(git rev-parse --show-toplevel)
cd "${repository}"

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

# A transaction replays to the same bytes in separate processes, applies,
# and its inverse restores the fixture byte for byte.
cp -R fixtures/basic "${first}/source"
for output in "${first}/sealed.jsonc" "${second}/sealed.jsonc"; do
  cargo run --locked --quiet --package tilewright-cli -- tx rebase fixtures/transactions/orchard.jsonc "${first}/source" "${output}"
done
cmp "${first}/sealed.jsonc" "${second}/sealed.jsonc"
cargo run --locked --quiet --package tilewright-cli -- tx apply "${first}/sealed.jsonc" "${first}/source" "${first}/undo.jsonc"
cargo run --locked --quiet --package tilewright-cli -- validate "${first}/source"
cargo run --locked --quiet --package tilewright-cli -- tx apply "${first}/undo.jsonc" "${first}/source" "${first}/redo.jsonc"
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

git diff --check
