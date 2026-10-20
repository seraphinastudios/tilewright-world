#!/usr/bin/env bash
# Builds the WebAssembly pack reader into packages/tilewright-pack/wasm.
#
#   tools/build-wasm.sh          build
#   tools/build-wasm.sh --check  lint the reader for wasm32 first
#
# It needs a C compiler for wasm32 (SQLite and zstd are C) and the
# wasm-bindgen command whose version matches the crate in Cargo.lock. Both
# are pinned below and downloaded once, checksum-verified, into
# ${TILEWRIGHT_WASM_TOOLS:-${XDG_CACHE_HOME:-~/.cache}/tilewright-world/wasm-tools}.
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

# Downloads a release archive, checks its SHA-256, and unpacks it as
# ${tools}/<name>. Nothing is installed unless the checksum matches.
install() {
  local name=$1 url=$2 expected=$3
  if [[ -d ${tools}/${name} ]]; then
    return
  fi
  mkdir -p "${tools}"
  local staging
  staging=$(mktemp -d "${tools}/.${name}.XXXXXX")
  echo "tools/build-wasm.sh: downloading ${url}" >&2
  curl --fail --silent --show-error --location --retry 3 --proto '=https' --tlsv1.2 \
    --output "${staging}/archive.tar.gz" "${url}"
  local found
  found=$(sha256 "${staging}/archive.tar.gz")
  if [[ ${found} != "${expected}" ]]; then
    echo "tools/build-wasm.sh: ${url} has SHA-256 ${found}, not ${expected}" >&2
    rm -rf -- "${staging}"
    exit 1
  fi
  mkdir "${staging}/unpacked"
  tar -xzf "${staging}/archive.tar.gz" -C "${staging}/unpacked" --strip-components 1
  # Another build may have installed it meanwhile.
  mv "${staging}/unpacked" "${tools}/${name}" 2>/dev/null || [[ -d ${tools}/${name} ]]
  rm -rf -- "${staging}"
}

if [[ -z ${CC_wasm32_unknown_unknown-} || -z ${AR_wasm32_unknown_unknown-} ]]; then
  wasi_sdk=wasi-sdk-${wasi_sdk_version}-${wasi_sdk_platform}
  install "${wasi_sdk}" \
    "https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-${wasi_sdk_release}/${wasi_sdk}.tar.gz" \
    "${wasi_sdk_sha256}"
  export CC_wasm32_unknown_unknown=${tools}/${wasi_sdk}/bin/clang
  export AR_wasm32_unknown_unknown=${tools}/${wasi_sdk}/bin/llvm-ar
fi
wasm_bindgen=wasm-bindgen-${wasm_bindgen_version}-${wasm_bindgen_platform}
install "${wasm_bindgen}" \
  "https://github.com/wasm-bindgen/wasm-bindgen/releases/download/${wasm_bindgen_version}/${wasm_bindgen}.tar.gz" \
  "${wasm_bindgen_sha256}"

# Paths of this machine stay out of the module, so builds of one commit on
# different machines match: Rust panic locations and C file names name the
# repository and the Cargo home by fixed prefixes.
cargo_home=${CARGO_HOME:-${HOME}/.cargo}
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=${repository}=/tilewright-world"$'\x1f'"--remap-path-prefix=${cargo_home}=/cargo"
export CFLAGS_wasm32_unknown_unknown="-ffile-prefix-map=${repository}=/tilewright-world -ffile-prefix-map=${cargo_home}=/cargo"

if [[ ${check} == true ]]; then
  # Only the reader's own crates: their dependencies are linted for the
  # host by tools/validate.sh.
  cargo clippy --locked --target wasm32-unknown-unknown --profile wasm \
    --package tilewright-pack --package tilewright-pack-wasm --no-deps -- -D warnings
fi
cargo build --locked --target wasm32-unknown-unknown --profile wasm --package tilewright-pack-wasm

rm -rf -- "${package}/wasm"
"${tools}/${wasm_bindgen}/wasm-bindgen" \
  --target web \
  --no-typescript \
  --remove-name-section \
  --remove-producers-section \
  --out-dir "${package}/wasm" \
  --out-name tilewright_pack \
  target/wasm32-unknown-unknown/wasm/tilewright_pack_wasm.wasm

wasm=${package}/wasm/tilewright_pack_bg.wasm
echo "tools/build-wasm.sh: ${wasm}: $(wc -c <"${wasm}" | tr -d ' ') bytes, $(gzip -9 -c "${wasm}" | wc -c | tr -d ' ') gzipped" >&2
