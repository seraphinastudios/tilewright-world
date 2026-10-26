#!/usr/bin/env bash
# Builds the WebAssembly pack reader into packages/tilewright-pack/wasm.
#
#   tools/build-wasm.sh          build
#   tools/build-wasm.sh --check  lint the reader for wasm32 first
#
# It needs a C compiler for wasm32 (SQLite and zstd are C) and the
# wasm-bindgen command whose version matches the crate in Cargo.lock. Both
# are pinned below. Their release archives are downloaded once into
# ${TILEWRIGHT_WASM_TOOLS:-${XDG_CACHE_HOME:-~/.cache}/tilewright-world/wasm-tools},
# and on every build checked by SHA-256 and unpacked into target/wasm-tools.
# One build runs at a time in a checkout.
# Set CC_wasm32_unknown_unknown and AR_wasm32_unknown_unknown to use another
# clang with the WebAssembly target instead of wasi-sdk.
set -euo pipefail

repository=$(git rev-parse --show-toplevel)
cd "${repository}"

check=false
case "${1-}" in
  "") ;;
  --check) check=true ;;
  *)
    echo "usage: tools/build-wasm.sh [--check]" >&2
    exit 2
    ;;
esac

wasi_sdk_release=34
wasi_sdk_version=34.0
wasm_bindgen_version=0.2.129
tools=${TILEWRIGHT_WASM_TOOLS:-${XDG_CACHE_HOME:-${HOME}/.cache}/tilewright-world/wasm-tools}
package=packages/tilewright-pack

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)
    wasi_sdk_platform=x86_64-linux
    wasi_sdk_sha256=b761e3a0721dbae9c09a0059e5fdb2bf917d1b4a8a7b430fb3b5aafb0984b2c4
    wasm_bindgen_platform=x86_64-unknown-linux-musl
    wasm_bindgen_sha256=82d12bb940e2d4e72e0d5605387fc1b8ca179044e012b620f0ce4e7440e8320e
    ;;
  Linux-aarch64 | Linux-arm64)
    wasi_sdk_platform=arm64-linux
    wasi_sdk_sha256=f7e243dff54d60bcc576e94d6166b69f410f2500ae4a9ceef34315be10e77971
    wasm_bindgen_platform=aarch64-unknown-linux-musl
    wasm_bindgen_sha256=2ed4351c35dd9440308bbb02767d47ea278efe851a52465300f3c94f5b6c2a87
    ;;
  Darwin-x86_64)
    wasi_sdk_platform=x86_64-macos
    wasi_sdk_sha256=87d27fa8adc68dee59bfbf2e22a6d34ef717c34d6bf1d8af2a56fc929d9ce0eb
    wasm_bindgen_platform=x86_64-apple-darwin
    wasm_bindgen_sha256=7e028879a68ec53dae14048b55cc5215f6df75b17a9c1426fac8fa086d3202fd
    ;;
  Darwin-arm64)
    wasi_sdk_platform=arm64-macos
    wasi_sdk_sha256=9c59398106b417f8f14913380fdf0097a8cc0ff4af9eb3ce0065a859e88d49e9
    wasm_bindgen_platform=aarch64-apple-darwin
    wasm_bindgen_sha256=81d4a23d56b3c3eb8187658329116d50e0b228a93b343825fb71f70179051cd1
    ;;
  *)
    echo "tools/build-wasm.sh: no pinned tools for $(uname -s)-$(uname -m)" >&2
    exit 1
    ;;
esac

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d ' ' -f 1
  else
    shasum -a 256 "$1" | cut -d ' ' -f 1
  fi
}

# One build at a time per checkout: a second fails rather than share its
# tools and output. The lock is a directory, which mkdir creates or fails
# on in one step; it records its holder, and goes when the build exits.
mkdir -p "${repository}/target"
lock=${repository}/target/wasm-build.lock
if ! mkdir "${lock}" 2>/dev/null; then
  holder=$(cat "${lock}/pid" 2>/dev/null || echo unknown)
  echo "tools/build-wasm.sh: another build of the WebAssembly reader holds ${lock} (process ${holder}); remove it if that process has ended" >&2
  exit 1
fi
echo "$$" >"${lock}/pid"
unpacked=${repository}/target/wasm-tools
trap 'rm -rf -- "${unpacked}" "${lock}"' EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

# This build's tools, unpacked afresh. The path stays the same between
# builds, so Cargo does not rebuild the C code for a new compiler path.
rm -rf -- "${unpacked}"
mkdir "${unpacked}"

# Unpacks the release archive <name>.tar.gz as ${unpacked}/<name>. The
# archive is downloaded once into ${tools}; on every build it is copied
# out, and the copy is checked against its pinned SHA-256 before it is
# unpacked, so a changed cache is never run.
install() {
  local name=$1 url=$2 expected=$3
  local archive=${tools}/${name}.tar.gz copy=${unpacked}/${name}.tar.gz
  if [[ -f ${archive} ]]; then
    cp "${archive}" "${copy}"
  else
    echo "tools/build-wasm.sh: downloading ${url}" >&2
    curl --fail --silent --show-error --location --retry 3 --proto '=https' --tlsv1.2 \
      --output "${copy}" "${url}"
  fi
  local found
  found=$(sha256 "${copy}")
  if [[ ${found} != "${expected}" ]]; then
    echo "tools/build-wasm.sh: ${name}.tar.gz has SHA-256 ${found}, not ${expected}" >&2
    if [[ -f ${archive} ]]; then
      echo "tools/build-wasm.sh: remove ${archive} to download it again" >&2
    fi
    exit 1
  fi
  if [[ ! -f ${archive} ]]; then
    mkdir -p "${tools}"
    local partial
    partial=$(mktemp "${tools}/.${name}.XXXXXX")
    cp "${copy}" "${partial}"
    mv "${partial}" "${archive}"
  fi
  mkdir "${unpacked}/${name}"
  tar -xzf "${copy}" -C "${unpacked}/${name}" --strip-components 1
  rm -- "${copy}"
}

if [[ -z ${CC_wasm32_unknown_unknown-} || -z ${AR_wasm32_unknown_unknown-} ]]; then
  wasi_sdk=wasi-sdk-${wasi_sdk_version}-${wasi_sdk_platform}
  install "${wasi_sdk}" \
    "https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-${wasi_sdk_release}/${wasi_sdk}.tar.gz" \
    "${wasi_sdk_sha256}"
  export CC_wasm32_unknown_unknown=${unpacked}/${wasi_sdk}/bin/clang
  export AR_wasm32_unknown_unknown=${unpacked}/${wasi_sdk}/bin/llvm-ar
fi
wasm_bindgen=wasm-bindgen-${wasm_bindgen_version}-${wasm_bindgen_platform}
install "${wasm_bindgen}" \
  "https://github.com/wasm-bindgen/wasm-bindgen/releases/download/${wasm_bindgen_version}/${wasm_bindgen}.tar.gz" \
  "${wasm_bindgen_sha256}"

# Paths of this machine stay out of the module, so builds of one commit on
# different machines match: Rust panic locations and C file names name the
# repository and the Cargo home by fixed prefixes. The C flags are parsed
# as shell words, so paths with spaces stay whole.
cargo_home=${CARGO_HOME:-${HOME}/.cargo}
quoted() {
  local escaped=${1//\'/\'\\\'\'}
  printf "'%s'" "${escaped}"
}
# Cargo builds into this checkout's target directory, whatever
# CARGO_TARGET_DIR or a Cargo config names: the module is taken from there,
# and its build paths stay under the remapped repository.
export CARGO_TARGET_DIR=${repository}/target
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=${repository}=/tilewright-world"$'\x1f'"--remap-path-prefix=${cargo_home}=/cargo"
export CC_SHELL_ESCAPED_FLAGS=1
CFLAGS_wasm32_unknown_unknown="-ffile-prefix-map=$(quoted "${repository}")=/tilewright-world -ffile-prefix-map=$(quoted "${cargo_home}")=/cargo"
export CFLAGS_wasm32_unknown_unknown

if [[ ${check} == true ]]; then
  # Only the reader's own crates: their dependencies are linted for the
  # host by tools/validate.sh.
  cargo clippy --locked --target wasm32-unknown-unknown --profile wasm \
    --package tilewright-pack --package tilewright-pack-wasm --no-deps -- -D warnings
fi
cargo build --locked --target wasm32-unknown-unknown --profile wasm --package tilewright-pack-wasm

# The generated JavaScript and WebAssembly go together, so wasm/ is replaced
# as a whole: an interrupted build leaves it missing, never mixed.
rm -rf -- "${package}"/.wasm.*
staged=$(mktemp -d "${package}/.wasm.XXXXXX")
chmod 755 "${staged}"
"${unpacked}/${wasm_bindgen}/wasm-bindgen" \
  --target web \
  --no-typescript \
  --remove-name-section \
  --remove-producers-section \
  --out-dir "${staged}" \
  --out-name tilewright_pack \
  target/wasm32-unknown-unknown/wasm/tilewright_pack_wasm.wasm
if [[ -e ${package}/wasm ]]; then
  mv "${package}/wasm" "${staged}.old"
fi
mv "${staged}" "${package}/wasm"
rm -rf -- "${staged}.old"

wasm=${package}/wasm/tilewright_pack_bg.wasm
echo "tools/build-wasm.sh: ${wasm}: $(wc -c <"${wasm}" | tr -d ' ') bytes, $(gzip -9 -c "${wasm}" | wc -c | tr -d ' ') gzipped" >&2
