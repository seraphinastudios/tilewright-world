#!/usr/bin/env python3
"""Assemble and verify the assets of a tilewright-world release.

assemble: from the binaries tools/release/build.sh staged for every target,
write deterministic archives with the license files, a source archive, the
npm tarball when there is one, and SHA256SUMS. It runs rustc to find the
pinned toolchain's notices for the Rust standard library:

    OUTPUT/assets/     the release assets, SHA256SUMS included
    OUTPUT/binaries/   tilewright-PLATFORM[.exe], the binaries the archives
                       hold, for build provenance of installed binaries

verify: check that a directory assemble wrote is complete and consistent.
"""

from __future__ import annotations

import argparse
import crate_candidates
import gzip
import hashlib
import io
import json
import re
import shutil
import subprocess
import sys
import tarfile
import time
import zipfile
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

VERSION = re.compile(
    r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?"
)
REVISION = re.compile(r"[0-9a-f]{40}")
ASSET_NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9._+-]*")
CHECKSUM_LINE = re.compile(r"([0-9a-f]{64})  ([A-Za-z0-9][A-Za-z0-9._+-]*)")
PACK_NAME = re.compile(r"[0-9a-f]{64}\.twpack")
LICENSE_FILE = re.compile(r"(LICEN[CS]E|COPYING|COPYRIGHT|NOTICE|UNLICENSE)([-._].*)?", re.IGNORECASE)
NPM_PACKAGE = "@seraphinastudios/tilewright-pack"
# Files every archive carries next to the binary.
DOCUMENTS = ("LICENSE", "PROVENANCE.md", "THIRD_PARTY_NOTICES.md")
LICENSES = "THIRD_PARTY_LICENSES.txt"
# Besides the crates, every binary links in the Rust standard library, whose
# notices the pinned toolchain ships as this file, and the Linux binaries
# link musl statically, whose notice this repository keeps.
RUST_LIBRARY_NOTICES = "RUST_LIBRARY_COPYRIGHT.html"
TOOLCHAIN_NOTICES = Path("share", "doc", "rust", "COPYRIGHT-library.html")
MUSL_COPYRIGHT = "tools/release/licenses/musl-COPYRIGHT"
CHECKSUMS = "SHA256SUMS"
MAXIMUM_LICENSE_BYTES = 1 << 20


@dataclass(frozen=True)
class Platform:
    name: str
    target: str
    executable: str
    archive: str

    @property
    def musl(self) -> bool:
        return self.target.endswith("-linux-musl")


# Keep in step with the build matrix in .github/workflows/release.yml.
PLATFORMS = (
    Platform("linux-amd64", "x86_64-unknown-linux-musl", "tilewright", "tar.gz"),
    Platform("linux-arm64", "aarch64-unknown-linux-musl", "tilewright", "tar.gz"),
    Platform("macos-amd64", "x86_64-apple-darwin", "tilewright", "tar.gz"),
    Platform("macos-arm64", "aarch64-apple-darwin", "tilewright", "tar.gz"),
    Platform("windows-amd64", "x86_64-pc-windows-msvc", "tilewright.exe", "zip"),
)


class ReleaseError(Exception):
    pass


def archive_name(version: str, platform: Platform) -> str:
    return f"tilewright-{version}-{platform.name}.{platform.archive}"


def archive_root(version: str, platform: Platform) -> str:
    return f"tilewright-{version}-{platform.name}"


def source_name(version: str) -> str:
    return f"tilewright-world-{version}-source.tar.gz"


def binary_name(platform: Platform) -> str:
    suffix = ".exe" if platform.executable.endswith(".exe") else ""
    return f"tilewright-{platform.name}{suffix}"


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def git(repository: Path, *arguments: str) -> bytes:
    return subprocess.run(
        ["git", *arguments], cwd=repository, check=True, stdout=subprocess.PIPE
    ).stdout


def gzip_bytes(data: bytes) -> bytes:
    """gzip with no name and no timestamp, so equal input gives equal output."""
    output = io.BytesIO()
    with gzip.GzipFile(filename="", mode="wb", fileobj=output, compresslevel=9, mtime=0) as stream:
        stream.write(data)
    return output.getvalue()


def tar_gz(members: list[tuple[str, bytes, int]], directory: str, mtime: int) -> bytes:
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w", format=tarfile.PAX_FORMAT) as archive:
        entry = tarfile.TarInfo(directory)
        entry.type = tarfile.DIRTYPE
        entry.mode = 0o755
        entry.mtime = mtime
        archive.addfile(entry)
        for name, data, mode in members:
            entry = tarfile.TarInfo(f"{directory}/{name}")
            entry.size = len(data)
            entry.mode = mode
            entry.mtime = mtime
            archive.addfile(entry, io.BytesIO(data))
    return gzip_bytes(raw.getvalue())


def zip_bytes(members: list[tuple[str, bytes, int]], directory: str, mtime: int) -> bytes:
    stamp = time.gmtime(max(mtime, 315532800))[:6]  # zip dates start in 1980
    output = io.BytesIO()
    with zipfile.ZipFile(output, "w") as archive:
        for name, data, mode in members:
            entry = zipfile.ZipInfo(f"{directory}/{name}", date_time=stamp)
            entry.create_system = 3
            entry.external_attr = (0o100000 | mode) << 16
            entry.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(entry, data, compresslevel=9)
    return output.getvalue()


def cargo_metadata(repository: Path, target: str) -> dict:
    output = subprocess.run(
        ["cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", target],
        cwd=repository,
        check=True,
        stdout=subprocess.PIPE,
    ).stdout
    return json.loads(output)


def license_texts(metadata: dict, root_package: str = "tilewright-cli") -> list[tuple[str, str, dict[str, str]]]:
    """The crates outside this workspace that `root_package` compiles in for
    the metadata's platform: (name, version, license expression) with the
    texts of the license files each crate ships, by file name. Only normal
    dependencies count; build and dev dependencies are not linked in."""
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    workspace = set(metadata["workspace_members"])
    roots = [package["id"] for package in metadata["packages"] if package["name"] == root_package and package["id"] in workspace]
    if len(roots) != 1:
        raise ReleaseError(f"the workspace has no single {root_package} package")
    seen: set[str] = set()
    pending = roots
    while pending:
        identifier = pending.pop()
        if identifier in seen:
            continue
        seen.add(identifier)
        for dependency in nodes[identifier]["deps"]:
            if any(kind["kind"] is None for kind in dependency["dep_kinds"]):
                pending.append(dependency["pkg"])
    crates = []
    missing = []
    for identifier in seen - workspace:
        package = packages[identifier]
        directory = Path(package["manifest_path"]).parent
        names = sorted(
            entry.name for entry in directory.iterdir() if entry.is_file() and LICENSE_FILE.fullmatch(entry.name)
        )
        # A crate may name its license file with `license-file`, anywhere in
        # the crate.
        if package.get("license_file"):
            custom = (directory / package["license_file"]).resolve()
            if not custom.is_relative_to(directory.resolve()) or not custom.is_file():
                raise ReleaseError(f"{package['name']} {package['version']}: its license file is not in the crate")
            name = custom.relative_to(directory.resolve()).as_posix()
            if name not in names:
                names.append(name)
        texts = {}
        for name in names:
            data = (directory / name).read_bytes()
            if len(data) > MAXIMUM_LICENSE_BYTES:
                raise ReleaseError(f"{package['name']} {package['version']}: {name} is larger than 1 MiB")
            texts[name] = data.decode("utf-8").replace("\r\n", "\n")
        if not texts:
            missing.append(f"{package['name']} {package['version']}")
        crates.append((package["name"], package["version"], package.get("license") or "", texts))
    if missing:
        raise ReleaseError("these crates ship no license file to include: " + ", ".join(sorted(missing)))
    crates.sort(key=lambda crate: (crate[0], crate[1]))
    return [(f"{name} {version}", license, texts) for name, version, license, texts in crates]


def rust_library_notices(repository: Path, revision: str) -> tuple[str, bytes]:
    """The pinned Rust version and the notices its toolchain ships for the
    Rust standard library. The build jobs use the same pinned toolchain, and
    rustc here must be it too."""
    toolchain = git(repository, "show", f"{revision}:rust-toolchain.toml").decode("utf-8")
    pinned = re.search(r'^channel = "([^"]+)"$', toolchain, re.MULTILINE)
    if not pinned:
        raise ReleaseError("rust-toolchain.toml names no channel")

    def rustc(*arguments: str) -> str:
        return subprocess.run(
            ["rustc", *arguments], cwd=repository, check=True, stdout=subprocess.PIPE, text=True
        ).stdout.strip()

    reported = rustc("--version").split()
    if len(reported) < 2 or reported[1] != pinned.group(1):
        raise ReleaseError(f"rustc is not the pinned {pinned.group(1)}: {' '.join(reported)}")
    notices = Path(rustc("--print", "sysroot")) / TOOLCHAIN_NOTICES
    if not notices.is_file():
        raise ReleaseError(f"the Rust toolchain ships no {TOOLCHAIN_NOTICES.as_posix()}")
    return pinned.group(1), notices.read_bytes()


def render_licenses(
    version: str,
    platform: Platform,
    crates: list[tuple[str, str, dict[str, str]]],
    rust: str,
    musl: str | None,
) -> bytes:
    """THIRD_PARTY_LICENSES.txt for one platform: the crates, the Rust
    standard library of Rust `rust`, and musl's notice `musl` if the
    platform links it."""
    rule = "=" * 78
    lines = [
        f"Third-party licenses for tilewright {version} ({platform.name}, {platform.target})",
        "",
        "The tilewright binary compiles in the Rust crates listed below. Each keeps",
        "its own license. The license files the crates ship follow the list; a text",
        "that several crates ship is printed once. THIRD_PARTY_NOTICES.md describes",
        "the C libraries some of them build in.",
        "",
        f"The binary also links in the standard library of Rust {rust}, which is",
        "licensed under Apache-2.0 OR MIT, with some parts under other permissive",
        f"licenses. Its copyright notices are in {RUST_LIBRARY_NOTICES}.",
    ]
    if musl is not None:
        lines.extend(
            [
                "It is statically linked with musl, the C library, which is licensed under",
                "MIT. musl's copyright notice follows the crates' license files.",
            ]
        )
    lines.append("")
    for crate, license, _ in crates:
        lines.append(f"  {crate}: {license or 'see its license files'}")
    groups: dict[str, list[str]] = {}
    for crate, _, texts in crates:
        for name, text in texts.items():
            groups.setdefault(text, []).append(f"{crate}: {name}")
    sections = list(groups.items())
    if musl is not None:
        sections.append((musl, ["musl: COPYRIGHT"]))
    for text, users in sections:
        lines.extend(["", rule])
        lines.extend(users)
        lines.append("-" * 78)
        lines.append(text.rstrip("\n"))
    return ("\n".join(lines) + "\n").encode("utf-8")


def read_fixture_pack(directory: Path) -> str:
    path = directory / "fixture-pack"
    if not path.is_file():
        raise ReleaseError(f"{path} is missing")
    name = path.read_text(encoding="utf-8").strip()
    if not PACK_NAME.fullmatch(name):
        raise ReleaseError(f"{path} does not name a pack")
    return name


def check_npm_tarball(path: Path, version: str) -> None:
    """The npm tarball holds the package at this release's version, with its
    WebAssembly module."""
    if not ASSET_NAME.fullmatch(path.name) or not path.name.endswith(".tgz"):
        raise ReleaseError(f"unexpected npm tarball name: {path.name}")
    with tarfile.open(path, "r:gz") as archive:
        names = archive.getnames()
        try:
            manifest = archive.extractfile("package/package.json")
        except KeyError:
            manifest = None
        if manifest is None:
            raise ReleaseError("the npm tarball has no package/package.json")
        package = json.loads(manifest.read())
    if package.get("name") != NPM_PACKAGE or package.get("version") != version:
        raise ReleaseError(
            f"the npm tarball holds {package.get('name')}@{package.get('version')}, not {NPM_PACKAGE}@{version}"
        )
    if "package/wasm/tilewright_pack_bg.wasm" not in names:
        raise ReleaseError("the npm tarball has no wasm/tilewright_pack_bg.wasm")


def write_checksums(assets: Path) -> None:
    lines = [
        f"{sha256(path)}  {path.name}\n"
        for path in sorted(assets.iterdir(), key=lambda path: path.name)
        if path.name != CHECKSUMS
    ]
    (assets / CHECKSUMS).write_text("".join(lines), encoding="utf-8", newline="\n")


def assemble(
    repository: Path,
    version: str,
    revision: str,
    binaries: Path,
    output: Path,
    npm_tarball: Path | None,
    metadata: Callable[[Path, str], dict] = cargo_metadata,
    toolchain: Callable[[Path, str], tuple[str, bytes]] = rust_library_notices,
    candidates: Path | None = None,
) -> None:
    if not VERSION.fullmatch(version):
        raise ReleaseError(f"invalid release version: {version}")
    if not REVISION.fullmatch(revision):
        raise ReleaseError(f"invalid revision: {revision}")
    if output.exists():
        raise ReleaseError(f"the output already exists: {output}")

    # Every target compiled the fixture to the same pack.
    packs = {platform.target: read_fixture_pack(binaries / platform.target) for platform in PLATFORMS}
    if len(set(packs.values())) != 1:
        raise ReleaseError("the targets compiled the fixture to different packs: " + json.dumps(packs, sort_keys=True))

    mtime = int(git(repository, "show", "--no-patch", "--format=%ct", revision).decode().strip())
    documents = [(name, git(repository, "show", f"{revision}:{name}"), 0o644) for name in DOCUMENTS]
    rust, rust_notices = toolchain(repository, revision)
    documents.append((RUST_LIBRARY_NOTICES, rust_notices, 0o644))
    musl = git(repository, "show", f"{revision}:{MUSL_COPYRIGHT}").decode("utf-8")
    assets = output / "assets"
    staged = output / "binaries"
    assets.mkdir(parents=True)
    staged.mkdir()
    for platform in PLATFORMS:
        binary = binaries / platform.target / platform.executable
        if not binary.is_file():
            raise ReleaseError(f"{binary} is missing")
        data = binary.read_bytes()
        crates = license_texts(metadata(repository, platform.target))
        licenses = render_licenses(version, platform, crates, rust, musl if platform.musl else None)
        members = [(platform.executable, data, 0o755), *documents, (LICENSES, licenses, 0o644)]
        members.sort(key=lambda member: member[0])
        root = archive_root(version, platform)
        packed = tar_gz(members, root, mtime) if platform.archive == "tar.gz" else zip_bytes(members, root, mtime)
        (assets / archive_name(version, platform)).write_bytes(packed)
        (staged / binary_name(platform)).write_bytes(data)

    source = git(repository, "archive", "--format=tar", f"--prefix=tilewright-world-{version}/", revision)
    (assets / source_name(version)).write_bytes(gzip_bytes(source))

    if npm_tarball is not None:
        check_npm_tarball(npm_tarball, version)
        shutil.copyfile(npm_tarball, assets / npm_tarball.name)

    if candidates is not None:
        (assets / crate_candidates.name(version)).write_bytes(crate_candidates.bundle(candidates, revision))

    write_checksums(assets)


def archive_members(path: Path) -> dict[str, bytes]:
    members: dict[str, bytes] = {}
    if path.name.endswith(".zip"):
        with zipfile.ZipFile(path) as archive:
            for entry in archive.infolist():
                members[entry.filename] = archive.read(entry)
    else:
        with tarfile.open(path, "r:gz") as archive:
            for entry in archive.getmembers():
                if entry.isfile():
                    stream = archive.extractfile(entry)
                    members[entry.name] = stream.read() if stream else b""
                elif not entry.isdir():
                    raise ReleaseError(f"{path.name}: {entry.name} is neither a file nor a directory")
    return members


def verify(directory: Path, version: str, npm: bool, revision: str | None = None) -> list[str]:
    """Checks a directory that assemble wrote; returns the asset names."""
    if not VERSION.fullmatch(version):
        raise ReleaseError(f"invalid release version: {version}")
    assets = directory / "assets"
    staged = directory / "binaries"
    checksums = (assets / CHECKSUMS).read_text(encoding="utf-8")
    if not checksums.endswith("\n"):
        raise ReleaseError(f"{CHECKSUMS} does not end with a newline")
    listed: dict[str, str] = {}
    for line in checksums[:-1].split("\n"):
        match = CHECKSUM_LINE.fullmatch(line)
        if not match or match.group(2) == CHECKSUMS or match.group(2) in listed:
            raise ReleaseError(f"{CHECKSUMS} has an invalid line: {line!r}")
        listed[match.group(2)] = match.group(1)
    if list(listed) != sorted(listed):
        raise ReleaseError(f"{CHECKSUMS} is not sorted by name")
    present = {path.name for path in assets.iterdir()} - {CHECKSUMS}
    if present != set(listed):
        raise ReleaseError(f"{CHECKSUMS} lists {sorted(listed)} but the assets are {sorted(present)}")
    for name, digest in listed.items():
        if sha256(assets / name) != digest:
            raise ReleaseError(f"{name} does not match its SHA-256 in {CHECKSUMS}")

    expected = {archive_name(version, platform) for platform in PLATFORMS} | {source_name(version)}
    if revision is not None:
        if not REVISION.fullmatch(revision):
            raise ReleaseError(f"invalid candidate source revision: {revision}")
        expected.add(crate_candidates.name(version))
    if not expected <= present:
        raise ReleaseError(f"missing assets: {sorted(expected - present)}")
    if revision is not None:
        crate_candidates.verify_path(assets / crate_candidates.name(version), revision)
    tarballs = sorted(present - expected)
    if npm:
        if len(tarballs) != 1:
            raise ReleaseError(f"expected one npm tarball besides the archives, found {tarballs}")
        check_npm_tarball(assets / tarballs[0], version)
    elif tarballs:
        raise ReleaseError(f"unexpected assets: {tarballs}")

    if {path.name for path in staged.iterdir()} != {binary_name(platform) for platform in PLATFORMS}:
        raise ReleaseError("the staged binaries do not match the platforms")
    for platform in PLATFORMS:
        root = archive_root(version, platform)
        members = archive_members(assets / archive_name(version, platform))
        wanted = {f"{root}/{name}" for name in (platform.executable, *DOCUMENTS, RUST_LIBRARY_NOTICES, LICENSES)}
        if set(members) != wanted:
            raise ReleaseError(f"{archive_name(version, platform)} holds {sorted(members)}, not {sorted(wanted)}")
        if members[f"{root}/{platform.executable}"] != (staged / binary_name(platform)).read_bytes():
            raise ReleaseError(f"{archive_name(version, platform)} holds a different binary from {binary_name(platform)}")
    return sorted(present | {CHECKSUMS})


def main(arguments: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    build = commands.add_parser("assemble")
    build.add_argument("--version", required=True)
    build.add_argument("--revision", required=True)
    build.add_argument("--binaries", required=True, type=Path)
    build.add_argument("--output", required=True, type=Path)
    build.add_argument("--npm-tarball", type=Path)
    build.add_argument("--crate-candidates", type=Path)
    check = commands.add_parser("verify")
    check.add_argument("--version", required=True)
    check.add_argument("--npm", action="store_true", help="require the npm tarball")
    check.add_argument("--revision", help="require crate candidates from this exact source revision")
    check.add_argument("directory", type=Path)
    options = parser.parse_args(arguments)
    repository = Path(git(Path.cwd(), "rev-parse", "--show-toplevel").decode().strip())
    try:
        if options.command == "assemble":
            assemble(
                repository,
                options.version,
                options.revision,
                options.binaries.resolve(),
                options.output.resolve(),
                options.npm_tarball.resolve() if options.npm_tarball else None,
                candidates=options.crate_candidates.resolve() if options.crate_candidates else None,
            )
            names = verify(options.output.resolve(), options.version, options.npm_tarball is not None,
                           options.revision if options.crate_candidates else None)
        else:
            names = verify(options.directory.resolve(), options.version, options.npm, options.revision)
    except (ReleaseError, ValueError, OSError, KeyError, TypeError, tarfile.TarError) as error:
        print(f"release: {error}", file=sys.stderr)
        return 1
    for name in names:
        print(name)
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
