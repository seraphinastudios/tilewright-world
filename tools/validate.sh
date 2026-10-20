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

git diff --check
