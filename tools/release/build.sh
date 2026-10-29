#!/usr/bin/env bash
# Builds the tilewright CLI for one release target with the toolchain that
# rust-toolchain.toml pins and the locked dependencies, runs it on the
# fixture, and stages it for tools/release/package.py:
#
#   OUTPUT/TARGET/tilewright[.exe]   the binary
#   OUTPUT/TARGET/fixture-pack       the file name (the digest) of the pack it
#                                    compiled from fixtures/basic
#
# usage: tools/release/build.sh TARGET VERSION OUTPUT
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo "usage: tools/release/build.sh TARGET VERSION OUTPUT" >&2
  exit 2
fi
target=$1
version=$2
output=$3

repository=$(git rev-parse --show-toplevel)
cd "${repository}"

if [[ ! ${version} =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$ ]]; then
  echo "invalid release version: ${version}" >&2
  exit 2
fi
case ${target} in
  x86_64-unknown-linux-musl | aarch64-unknown-linux-musl | x86_64-apple-darwin | aarch64-apple-darwin)
    executable=tilewright
    ;;
  x86_64-pc-windows-msvc)
    executable=tilewright.exe
    ;;
  *)
    echo "unsupported release target: ${target}" >&2
    exit 2
    ;;
esac
if [[ -e ${output}/${target} ]]; then
  echo "release build output already exists: ${output}/${target}" >&2
  exit 1
fi

# The toolchain rust-toolchain.toml names, and the target's standard library.
rustup toolchain install --no-self-update
rustup target add "${target}"
channel=$(sed -n 's/^channel = "\([^"]*\)"$/\1/p' rust-toolchain.toml)
if [[ $(rustc --version | cut -d' ' -f2) != "${channel}" ]]; then
  echo "rustc is not the pinned ${channel}: $(rustc --version)" >&2
  exit 1
fi

# Paths as rustc sees them, which on Windows are native paths.
native_path() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath --windows "$1"
  else
    printf '%s' "$1"
  fi
}

# Build paths are remapped so the binary records neither the checkout nor the
# Cargo home, and a build elsewhere produces the same paths. Windows binaries
# link the C runtime statically, so they need no Visual C++ redistributable.
cargo_home=${CARGO_HOME:-${HOME}/.cargo}
flags=(
  "--remap-path-prefix=$(native_path "${repository}")=/tilewright-world"
  "--remap-path-prefix=$(native_path "${cargo_home}")=/cargo"
)
case ${target} in
  *-windows-msvc) flags+=("-Ctarget-feature=+crt-static") ;;
  # The musl targets link statically; their C code compiles with musl-gcc.
  x86_64-unknown-linux-musl) export CC_x86_64_unknown_linux_musl=musl-gcc ;;
  aarch64-unknown-linux-musl) export CC_aarch64_unknown_linux_musl=musl-gcc ;;
esac
if [[ ${target} == *-linux-musl ]] && ! command -v musl-gcc >/dev/null 2>&1; then
  echo "musl-gcc is not installed (on Debian and Ubuntu: apt-get install musl-tools)" >&2
  exit 1
fi
CARGO_ENCODED_RUSTFLAGS=$(IFS=$'\x1f'; printf '%s' "${flags[*]}")
export CARGO_ENCODED_RUSTFLAGS
export CARGO_INCREMENTAL=0
# `tilewright --version` names the release it was built for.
export TILEWRIGHT_RELEASE="v${version}"

cargo build --locked --release --package tilewright-cli --target "${target}"
binary="target/${target}/release/${executable}"

# Every target is built on a runner of its own architecture, so the binary
# runs here: it reports its version and validates, compiles and verifies the
# fixture.
reported=$("${binary}" --version | tr -d '\r')
if [[ ${reported} != "tilewright "*" (release v${version})" ]]; then
  echo "unexpected version: ${reported} (expected release v${version})" >&2
  exit 1
fi
scratch=$(mktemp -d)
trap 'rm -rf -- "${scratch}"' EXIT
"${binary}" validate fixtures/basic
"${binary}" compile fixtures/basic "${scratch}"
packs=("${scratch}"/*.twpack)
if [[ ${#packs[@]} -ne 1 || ! -f ${packs[0]} ]]; then
  echo "compiling the fixture did not write exactly one pack" >&2
  exit 1
fi
"${binary}" inspect "${packs[0]}"

mkdir -p "${output}/${target}"
cp "${binary}" "${output}/${target}/${executable}"
basename "${packs[0]}" >"${output}/${target}/fixture-pack"
echo "built ${target}: $(sha256sum "${output}/${target}/${executable}" 2>/dev/null || shasum -a 256 "${output}/${target}/${executable}")"
