"""Tests for tools/release/package.py.

Run from the repository root: python3 -m unittest discover -s tools/release
"""

from __future__ import annotations

import io
import json
import os
import re
import shutil
import subprocess
import tarfile
import tempfile
import unittest
import zipfile
from pathlib import Path
from unittest import mock

import package

REPOSITORY = Path(__file__).resolve().parents[2]
PACK = "5830e26e449a1f0d0c786b32801b7ed68e351777db5f586532478cb4384844b2.twpack"
RUST_NOTICES = b"<html>notices of the Rust standard library</html>\n"
MUSL_NOTICE = "musl as a whole is licensed under the following standard MIT license:\n"


def git(root: Path, *arguments: str) -> str:
    return subprocess.run(
        ["git", *arguments], cwd=root, check=True, stdout=subprocess.PIPE, text=True
    ).stdout.strip()


def fake_metadata(root: Path):
    """cargo metadata for tilewright-cli -> alpha -> beta, with gamma only a
    build dependency of alpha and delta only a dev dependency of the CLI."""
    crates = root / "crates"

    def crate(name: str, files: dict[str, str]) -> dict:
        directory = crates / name
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "Cargo.toml").write_text("", encoding="utf-8")
        for file, text in files.items():
            (directory / file).write_text(text, encoding="utf-8")
        return {
            "id": f"registry+test#{name}@1.0.0",
            "name": name,
            "version": "1.0.0",
            "license": "MIT OR Apache-2.0",
            "license_file": None,
            "manifest_path": str(directory / "Cargo.toml"),
        }

    shared = "Apache License\nVersion 2.0\n"
    cli = {
        "id": "path+file:///world#tilewright-cli@0.1.0",
        "name": "tilewright-cli",
        "version": "0.1.0",
        "license": "MIT",
        "license_file": None,
        "manifest_path": str(root / "Cargo.toml"),
    }
    packages = [
        cli,
        crate("alpha", {"LICENSE-MIT": "Copyright alpha\n", "LICENSE-APACHE": shared, "README.md": "no"}),
        crate("beta", {"LICENSE-APACHE": shared, "COPYRIGHT": "beta authors\r\n"}),
        crate("gamma", {}),
        crate("delta", {}),
    ]

    def dependency(name: str, kind: str | None) -> dict:
        return {"pkg": f"registry+test#{name}@1.0.0", "dep_kinds": [{"kind": kind, "target": None}]}

    nodes = [
        {"id": cli["id"], "deps": [dependency("alpha", None), dependency("delta", "dev")]},
        {"id": packages[1]["id"], "deps": [dependency("beta", None), dependency("gamma", "build")]},
        {"id": packages[2]["id"], "deps": []},
        {"id": packages[3]["id"], "deps": []},
        {"id": packages[4]["id"], "deps": []},
    ]
    metadata = {"packages": packages, "workspace_members": [cli["id"]], "resolve": {"nodes": nodes}}
    return lambda _repository, _target: metadata


class Fixture:
    def __init__(self) -> None:
        self.directory = tempfile.TemporaryDirectory(prefix="tilewright-release-test-")
        self.root = Path(self.directory.name)
        self.repository = self.root / "repository"
        self.repository.mkdir()
        for name in package.DOCUMENTS:
            (self.repository / name).write_text(f"{name} text\n", encoding="utf-8")
        (self.repository / "rust-toolchain.toml").write_text('[toolchain]\nchannel = "1.97.1"\n', encoding="utf-8")
        musl = self.repository / package.MUSL_COPYRIGHT
        musl.parent.mkdir(parents=True)
        musl.write_text(MUSL_NOTICE, encoding="utf-8")
        git(self.repository, "init", "--quiet")
        git(self.repository, "config", "user.name", "Release fixture")
        git(self.repository, "config", "user.email", "release-fixture@example.invalid")
        git(self.repository, "config", "commit.gpgsign", "false")
        git(self.repository, "add", ".")
        git(self.repository, "commit", "--quiet", "-m", "feat: start")
        self.revision = git(self.repository, "rev-parse", "HEAD")
        self.binaries = self.root / "binaries"
        for platform in package.PLATFORMS:
            directory = self.binaries / platform.target
            directory.mkdir(parents=True)
            (directory / platform.executable).write_bytes(f"binary for {platform.target}".encode())
            (directory / "fixture-pack").write_text(PACK + "\n", encoding="utf-8")
        self.metadata = fake_metadata(self.root)

    def assemble(self, output: str = "out", version: str = "0.2.0", npm: Path | None = None) -> Path:
        destination = self.root / output
        package.assemble(
            self.repository,
            version,
            self.revision,
            self.binaries,
            destination,
            npm,
            metadata=self.metadata,
            toolchain=lambda _repository, _revision: ("1.97.1", RUST_NOTICES),
        )
        return destination

    def npm_tarball(self, version: str = "0.2.0", name: str = package.NPM_PACKAGE, wasm: bool = True) -> Path:
        path = self.root / f"seraphinastudios-tilewright-pack-{version}.tgz"
        files = {"package/package.json": json.dumps({"name": name, "version": version}).encode()}
        if wasm:
            files["package/wasm/tilewright_pack_bg.wasm"] = b"\0asm"
        with tarfile.open(path, "w:gz") as archive:
            for member, data in files.items():
                entry = tarfile.TarInfo(member)
                entry.size = len(data)
                archive.addfile(entry, io.BytesIO(data))
        return path

    def close(self) -> None:
        self.directory.cleanup()


class AssembleTest(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = Fixture()
        self.addCleanup(self.fixture.close)

    def test_assets_are_complete_and_deterministic(self) -> None:
        first = self.fixture.assemble("first")
        second = self.fixture.assemble("second")
        names = package.verify(first, "0.2.0", npm=False)
        self.assertEqual(
            names,
            sorted(
                [
                    "SHA256SUMS",
                    "tilewright-0.2.0-linux-amd64.tar.gz",
                    "tilewright-0.2.0-linux-arm64.tar.gz",
                    "tilewright-0.2.0-macos-amd64.tar.gz",
                    "tilewright-0.2.0-macos-arm64.tar.gz",
                    "tilewright-0.2.0-windows-amd64.zip",
                    "tilewright-world-0.2.0-source.tar.gz",
                ]
            ),
        )
        for name in names:
            self.assertEqual(
                (first / "assets" / name).read_bytes(), (second / "assets" / name).read_bytes(), name
            )
            # Two runs within one second would not catch a clock time in a
            # gzip header, so check the headers: no file name, no time.
            if name.endswith(".tar.gz"):
                header = (first / "assets" / name).read_bytes()[:10]
                self.assertEqual(header[:3], b"\x1f\x8b\x08", name)
                self.assertEqual(header[3] & 0x08, 0, f"{name} records a file name")
                self.assertEqual(header[4:8], b"\0\0\0\0", f"{name} records a time")
        self.assertEqual(
            sorted(path.name for path in (first / "binaries").iterdir()),
            [
                "tilewright-linux-amd64",
                "tilewright-linux-arm64",
                "tilewright-macos-amd64",
                "tilewright-macos-arm64",
                "tilewright-windows-amd64.exe",
            ],
        )
        checksums = (first / "assets" / "SHA256SUMS").read_text(encoding="utf-8")
        self.assertRegex(checksums, re.compile(r"\A([0-9a-f]{64}  [^\n]+\n){6}\Z"))

    def test_archives_hold_the_binary_and_licenses(self) -> None:
        output = self.fixture.assemble()
        with tarfile.open(output / "assets" / "tilewright-0.2.0-linux-arm64.tar.gz") as archive:
            members = {member.name: member for member in archive.getmembers()}
            self.assertEqual(
                sorted(members),
                [
                    "tilewright-0.2.0-linux-arm64",
                    "tilewright-0.2.0-linux-arm64/LICENSE",
                    "tilewright-0.2.0-linux-arm64/PROVENANCE.md",
                    "tilewright-0.2.0-linux-arm64/RUST_LIBRARY_COPYRIGHT.html",
                    "tilewright-0.2.0-linux-arm64/THIRD_PARTY_LICENSES.txt",
                    "tilewright-0.2.0-linux-arm64/THIRD_PARTY_NOTICES.md",
                    "tilewright-0.2.0-linux-arm64/tilewright",
                ],
            )
            binary = members["tilewright-0.2.0-linux-arm64/tilewright"]
            self.assertEqual(binary.mode, 0o755)
            self.assertEqual((binary.uid, binary.gid, binary.uname, binary.gname), (0, 0, "", ""))
            self.assertEqual(archive.extractfile(binary).read(), b"binary for aarch64-unknown-linux-musl")
            licenses = archive.extractfile(members["tilewright-0.2.0-linux-arm64/THIRD_PARTY_LICENSES.txt"])
            text = licenses.read().decode()
            notices = archive.extractfile(members["tilewright-0.2.0-linux-arm64/RUST_LIBRARY_COPYRIGHT.html"])
            self.assertEqual(notices.read(), RUST_NOTICES)
        self.assertIn("tilewright 0.2.0 (linux-arm64, aarch64-unknown-linux-musl)", text)
        self.assertIn("standard library of Rust 1.97.1", text)
        # The static Linux binaries carry musl's notice, after the crates'.
        self.assertIn("musl: COPYRIGHT\n" + "-" * 78 + "\n" + MUSL_NOTICE, text)
        self.assertGreater(text.index("musl: COPYRIGHT"), text.index("beta authors"))
        self.assertIn("  alpha 1.0.0: MIT OR Apache-2.0\n  beta 1.0.0: MIT OR Apache-2.0\n", text)
        self.assertNotIn("gamma", text)
        self.assertNotIn("delta", text)
        self.assertIn("Copyright alpha", text)
        self.assertIn("beta authors\n", text)
        self.assertNotIn("\r", text)
        # The shared Apache text is printed once, under both crates.
        self.assertEqual(text.count("Apache License\nVersion 2.0"), 1)
        self.assertIn("alpha 1.0.0: LICENSE-APACHE\nbeta 1.0.0: LICENSE-APACHE\n", text)
        with zipfile.ZipFile(output / "assets" / "tilewright-0.2.0-windows-amd64.zip") as archive:
            entry = archive.getinfo("tilewright-0.2.0-windows-amd64/tilewright.exe")
            self.assertEqual(entry.external_attr >> 16, 0o100755)
            self.assertEqual(archive.read(entry), b"binary for x86_64-pc-windows-msvc")
            self.assertEqual(archive.read("tilewright-0.2.0-windows-amd64/RUST_LIBRARY_COPYRIGHT.html"), RUST_NOTICES)
            text = archive.read("tilewright-0.2.0-windows-amd64/THIRD_PARTY_LICENSES.txt").decode()
        self.assertIn("standard library of Rust 1.97.1", text)
        self.assertNotIn("musl", text)
        with tarfile.open(output / "assets" / "tilewright-0.2.0-macos-arm64.tar.gz") as archive:
            text = archive.extractfile("tilewright-0.2.0-macos-arm64/THIRD_PARTY_LICENSES.txt").read().decode()
        self.assertNotIn("musl", text)
        with tarfile.open(output / "assets" / "tilewright-world-0.2.0-source.tar.gz") as archive:
            self.assertIn("tilewright-world-0.2.0/LICENSE", archive.getnames())

    def test_the_targets_must_compile_the_same_pack(self) -> None:
        (self.fixture.binaries / "x86_64-pc-windows-msvc" / "fixture-pack").write_text(
            "0" * 64 + ".twpack\n", encoding="utf-8"
        )
        with self.assertRaisesRegex(package.ReleaseError, "different packs"):
            self.fixture.assemble()

    def test_every_target_is_required(self) -> None:
        (self.fixture.binaries / "aarch64-apple-darwin" / "tilewright").unlink()
        with self.assertRaisesRegex(package.ReleaseError, "aarch64-apple-darwin/tilewright is missing"):
            self.fixture.assemble()

    def test_a_crate_without_a_license_file_stops_the_release(self) -> None:
        for path in (self.fixture.root / "crates" / "beta").iterdir():
            if path.name != "Cargo.toml":
                path.unlink()
        with self.assertRaisesRegex(package.ReleaseError, "beta 1.0.0"):
            self.fixture.assemble()

    def test_invalid_inputs(self) -> None:
        with self.assertRaisesRegex(package.ReleaseError, "invalid release version"):
            self.fixture.assemble(version="v0.2.0")
        self.fixture.assemble("taken")
        with self.assertRaisesRegex(package.ReleaseError, "already exists"):
            self.fixture.assemble("taken")

    def test_npm_tarball(self) -> None:
        output = self.fixture.assemble(npm=self.fixture.npm_tarball())
        names = package.verify(output, "0.2.0", npm=True)
        self.assertIn("seraphinastudios-tilewright-pack-0.2.0.tgz", names)
        with self.assertRaisesRegex(package.ReleaseError, "unexpected assets"):
            package.verify(output, "0.2.0", npm=False)
        plain = self.fixture.assemble("plain")
        with self.assertRaisesRegex(package.ReleaseError, "expected one npm tarball"):
            package.verify(plain, "0.2.0", npm=True)
        with self.assertRaisesRegex(package.ReleaseError, "not @seraphinastudios/tilewright-pack@0.2.0"):
            self.fixture.assemble("old", npm=self.fixture.npm_tarball(version="0.1.0"))
        with self.assertRaisesRegex(package.ReleaseError, "no wasm"):
            self.fixture.assemble("bare", npm=self.fixture.npm_tarball(wasm=False))


class ToolchainTest(unittest.TestCase):
    """rust_library_notices with a stand-in rustc on PATH."""

    def setUp(self) -> None:
        self.fixture = Fixture()
        self.addCleanup(self.fixture.close)
        self.sysroot = self.fixture.root / "sysroot"
        notices = self.sysroot / package.TOOLCHAIN_NOTICES
        notices.parent.mkdir(parents=True)
        notices.write_bytes(RUST_NOTICES)
        self.bin = self.fixture.root / "bin"
        self.bin.mkdir()

    def rustc(self, version: str) -> None:
        script = self.bin / "rustc"
        script.write_text(
            "#!/bin/sh\n"
            f'if [ "$1" = --version ]; then echo "rustc {version} (0123456789 2026-10-01)"; '
            f"else echo '{self.sysroot}'; fi\n",
            encoding="utf-8",
        )
        script.chmod(0o755)

    def notices(self) -> tuple[str, bytes]:
        with mock.patch.dict(os.environ, {"PATH": f"{self.bin}{os.pathsep}{os.environ['PATH']}"}):
            return package.rust_library_notices(self.fixture.repository, self.fixture.revision)

    @unittest.skipIf(os.name == "nt", "the stand-in rustc is a shell script")
    def test_the_pinned_toolchain_supplies_the_notices(self) -> None:
        self.rustc("1.97.1")
        self.assertEqual(self.notices(), ("1.97.1", RUST_NOTICES))

    @unittest.skipIf(os.name == "nt", "the stand-in rustc is a shell script")
    def test_another_rustc_stops_the_release(self) -> None:
        self.rustc("1.98.0")
        with self.assertRaisesRegex(package.ReleaseError, "not the pinned 1.97.1"):
            self.notices()

    @unittest.skipIf(os.name == "nt", "the stand-in rustc is a shell script")
    def test_a_toolchain_without_the_notices_stops_the_release(self) -> None:
        self.rustc("1.97.1")
        (self.sysroot / package.TOOLCHAIN_NOTICES).unlink()
        with self.assertRaisesRegex(package.ReleaseError, "ships no share/doc/rust/COPYRIGHT-library.html"):
            self.notices()


class VerifyTest(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = Fixture()
        self.addCleanup(self.fixture.close)
        self.output = self.fixture.assemble()
        self.assets = self.output / "assets"

    def test_a_changed_asset_fails(self) -> None:
        source = self.assets / "tilewright-world-0.2.0-source.tar.gz"
        source.write_bytes(source.read_bytes() + b"\0")
        with self.assertRaisesRegex(package.ReleaseError, "does not match its SHA-256"):
            package.verify(self.output, "0.2.0", npm=False)

    def test_an_unlisted_or_missing_asset_fails(self) -> None:
        (self.assets / "extra.txt").write_text("extra", encoding="utf-8")
        with self.assertRaisesRegex(package.ReleaseError, "SHA256SUMS lists"):
            package.verify(self.output, "0.2.0", npm=False)
        (self.assets / "extra.txt").unlink()
        (self.assets / "tilewright-0.2.0-macos-amd64.tar.gz").unlink()
        with self.assertRaisesRegex(package.ReleaseError, "SHA256SUMS lists"):
            package.verify(self.output, "0.2.0", npm=False)

    def test_a_listed_but_unexpected_asset_fails(self) -> None:
        (self.assets / "extra.txt").write_text("extra", encoding="utf-8")
        package.write_checksums(self.assets)
        with self.assertRaisesRegex(package.ReleaseError, "unexpected assets"):
            package.verify(self.output, "0.2.0", npm=False)

    def test_a_swapped_binary_fails(self) -> None:
        (self.output / "binaries" / "tilewright-linux-amd64").write_bytes(b"other")
        with self.assertRaisesRegex(package.ReleaseError, "holds a different binary"):
            package.verify(self.output, "0.2.0", npm=False)

    def test_the_version_must_match(self) -> None:
        with self.assertRaisesRegex(package.ReleaseError, "missing assets"):
            package.verify(self.output, "0.3.0", npm=False)


class WorkflowTest(unittest.TestCase):
    def test_the_build_matrix_matches_the_platforms(self) -> None:
        workflow = (REPOSITORY / ".github" / "workflows" / "release.yml").read_text(encoding="utf-8")
        targets = re.findall(r"^\s+(?:- )?target: (\S+)$", workflow, re.MULTILINE)
        self.assertEqual(sorted(targets), sorted(platform.target for platform in package.PLATFORMS))

    def test_the_build_script_accepts_exactly_the_platforms(self) -> None:
        script = (REPOSITORY / "tools" / "release" / "build.sh").read_text(encoding="utf-8")
        for platform in package.PLATFORMS:
            self.assertIn(platform.target, script)


if __name__ == "__main__":
    unittest.main()
