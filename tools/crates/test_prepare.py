#!/usr/bin/env python3
"""Credential-free candidate boundary tests; do not invoke Cargo."""
import copy
import importlib.util
import io
import json
import os
from pathlib import Path
import shutil
import tarfile
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("candidate_prepare", Path(__file__).with_name("prepare.py"))
P = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(P)
REVISION = "a" * 40


def crate(crate_name, extra=(), **package_changes):
    name = crate_name
    package = {"name": name, "version": "0.1.0", "publish": ["crates-io"],
               "license": "MIT", "license-file": "LICENSE", "repository": P.REPOSITORY}
    package.update(package_changes)
    manifest = "[package]\n" + "".join(f"{key} = {json.dumps(value)}\n" for key, value in package.items())
    files = [("Cargo.toml", manifest.encode()), ("Cargo.toml.orig", b"original"),
             (".cargo_vcs_info.json", json.dumps({"git": {"sha1": REVISION},
                                                "path_in_vcs": f"crates/{name}"}).encode()),
             ("LICENSE", (P.ROOT / "LICENSE").read_bytes()), ("README.md", b"candidate")]
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as stream:
        for path, data in [*files, *extra]:
            info = tarfile.TarInfo(f"{name}-0.1.0/{path}")
            info.size = len(data)
            stream.addfile(info, io.BytesIO(data))
    return buffer.getvalue()


def write_candidate(output):
    rows = []
    for name in P.PUBLIC:
        data = crate(name)
        row = P.package_row(name, "0.1.0", data, REVISION)
        rows.append(row)
        (output / row["asset"]).write_bytes(data)
    manifest = {"schema": 1, "status": "candidate-only", "repository": P.REPOSITORY,
                "revision": REVISION, "toolchain": "1.97.1", "registry": "crates-io",
                "reproducible_packages": True, "packages": rows}
    (output / "candidate.json").write_text(json.dumps(manifest))
    return manifest


class CandidateTests(unittest.TestCase):
    @patch.object(P, "environment", return_value={})
    def test_prepare_packages_the_complete_graph_twice_and_detects_changes(self, _):
        archives = {name: crate(name) for name in P.PUBLIC}
        calls = []

        def package(command, **_kwargs):
            calls.append(command)
            target = Path(command[command.index("--target-dir") + 1]) / "package"
            target.mkdir(parents=True)
            for name, data in archives.items():
                (target / f"{name}-0.1.0.crate").write_bytes(data)

        def git(command, **_kwargs):
            return REVISION if command[1] == "rev-parse" else ""

        with tempfile.TemporaryDirectory() as scratch:
            with patch.object(P, "run", side_effect=git), patch.object(P.subprocess, "run", side_effect=package):
                output = Path(scratch) / "good"
                P.prepare(output)
                self.assertTrue((output / "candidate.json").is_file())
                self.assertEqual(len(calls), 2)
                self.assertNotEqual(calls[0], calls[1])
                for command in calls:
                    self.assertIn("--offline", command)
                    self.assertIn("--locked", command)
                    self.assertEqual([command[index + 1] for index, value in enumerate(command)
                                      if value == "--package"], list(P.PUBLIC))
            calls.clear()

            def changed_package(command, **kwargs):
                package(command, **kwargs)
                if len(calls) == 2:
                    target = Path(command[command.index("--target-dir") + 1]) / "package"
                    (target / f"{P.PUBLIC[0]}-0.1.0.crate").write_bytes(b"changed")

            with patch.object(P, "run", side_effect=git), patch.object(P.subprocess, "run", side_effect=changed_package):
                output = Path(scratch) / "different"
                with self.assertRaisesRegex(ValueError, "differ"):
                    P.prepare(output)
                self.assertFalse((output / "candidate.json").exists())

    def test_repository_policy(self):
        self.assertEqual(P.policy(), ("0.1.0", "1.97.1"))

    def test_policy_rejects_extra_public_package_and_private_publish(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            shutil.copy(P.ROOT / "Cargo.toml", root)
            shutil.copy(P.ROOT / "LICENSE", root)
            shutil.copy(P.ROOT / "rust-toolchain.toml", root)
            shutil.copytree(P.ROOT / "crates", root / "crates", ignore=shutil.ignore_patterns("src", "tests"))
            (root / "tools/crates").mkdir(parents=True)
            shutil.copy(P.ROOT / "tools/crates/candidates.json", root / "tools/crates")
            config = root / "tools/crates/candidates.json"
            value = json.loads(config.read_text())
            value["crates"].append("unexpected")
            config.write_text(json.dumps(value))
            with self.assertRaisesRegex(ValueError, "allowlist"):
                P.policy(root)
            shutil.copy(P.ROOT / "tools/crates/candidates.json", config)
            package = root / "crates/tilewright-cli/Cargo.toml"
            package.write_text(package.read_text().replace("publish = false", 'publish = ["crates-io"]'))
            with self.assertRaisesRegex(ValueError, "publication"):
                P.policy(root)

    def test_policy_requires_explicit_license_file_packaging(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            shutil.copy(P.ROOT / "Cargo.toml", root)
            shutil.copy(P.ROOT / "LICENSE", root)
            shutil.copytree(P.ROOT / "crates", root / "crates", ignore=shutil.ignore_patterns("src", "tests"))
            (root / "tools/crates").mkdir(parents=True)
            shutil.copy(P.ROOT / "tools/crates/candidates.json", root / "tools/crates")
            package = root / "crates/tilewright-source/Cargo.toml"
            package.write_text(package.read_text().replace("license-file.workspace = true\n", ""))
            with self.assertRaisesRegex(ValueError, "inherit the root MIT license file"):
                P.policy(root)
            shutil.copy(P.ROOT / "crates/tilewright-source/Cargo.toml", package)
            manifest = root / "Cargo.toml"
            manifest.write_text(manifest.read_text().replace('license-file = "LICENSE"\n', ""))
            with self.assertRaisesRegex(ValueError, "provide the root MIT license file"):
                P.policy(root)

    def test_output_is_fresh_outside_checkout_and_without_symlinks(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            source = root / "source"
            source.mkdir()
            with self.assertRaisesRegex(ValueError, "outside"):
                P.reserve_output(source / "output", source)
            output = P.reserve_output(root / "output", source)
            with self.assertRaises(FileExistsError):
                P.reserve_output(output, source)
            link = root / "link"
            link.symlink_to(root, target_is_directory=True)
            with self.assertRaisesRegex(ValueError, "symlink"):
                P.reserve_output(link / "new", source)
            dangling = root / "dangling"
            dangling.symlink_to(root / "absent")
            with self.assertRaisesRegex(ValueError, "symlink"):
                P.reserve_output(dangling, source)

    def test_archive_identity_provenance_and_safe_paths(self):
        name = P.PUBLIC[0]
        P.archive(crate(name), name, "0.1.0", REVISION)
        for extra, changes, revision in [
            ([("../escape", b"no")], {}, REVISION),
            ([("README.md", b"duplicate")], {}, REVISION),
            ([], {"name": "foreign"}, REVISION),
            ([], {"publish": ["other"]}, REVISION),
            ([], {}, "b" * 40),
        ]:
            with self.subTest(extra=extra, changes=changes, revision=revision):
                with self.assertRaises(ValueError):
                    P.archive(crate(name, extra, **changes), name, "0.1.0", revision)

    def test_archive_rejects_links_and_nonregistry_dependencies(self):
        name = P.PUBLIC[0]
        data = crate(name)
        buffer = io.BytesIO()
        with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as source:
            with tarfile.open(fileobj=buffer, mode="w:gz") as target:
                for info in source:
                    content = source.extractfile(info).read()
                    if info.name.endswith("/Cargo.toml"):
                        content += b'\n[dependencies]\nforeign = { path = "/tmp/foreign" }\n'
                        info.size = len(content)
                    target.addfile(info, io.BytesIO(content))
        with self.assertRaisesRegex(ValueError, "non-crates.io"):
            P.archive(buffer.getvalue(), name, "0.1.0", REVISION)
        buffer = io.BytesIO()
        with tarfile.open(fileobj=buffer, mode="w:gz") as target:
            info = tarfile.TarInfo(f"{name}-0.1.0/link")
            info.type = tarfile.SYMTYPE
            info.linkname = "/tmp/outside"
            target.addfile(info)
        with self.assertRaisesRegex(ValueError, "unsafe"):
            P.archive(buffer.getvalue(), name, "0.1.0", REVISION)

    def test_archive_requires_exact_root_license_notice(self):
        name = P.PUBLIC[0]
        for notice in (None, b"MIT"):
            buffer = io.BytesIO()
            with tarfile.open(fileobj=io.BytesIO(crate(name)), mode="r:gz") as source:
                with tarfile.open(fileobj=buffer, mode="w:gz") as target:
                    for info in source:
                        content = source.extractfile(info).read()
                        if info.name.endswith("/LICENSE"):
                            if notice is None:
                                continue
                            content = notice
                            info.size = len(content)
                        target.addfile(info, io.BytesIO(content))
            with self.subTest(notice=notice), self.assertRaises(ValueError):
                P.archive(buffer.getvalue(), name, "0.1.0", REVISION)

    @patch.object(P, "environment", return_value={})
    def test_candidate_manifest_rejects_mutation_and_extra_files(self, _):
        with tempfile.TemporaryDirectory() as scratch:
            output = Path(scratch)
            original = write_candidate(output)
            P.verify(output)
            for change in [lambda x: x.update(status="published"),
                           lambda x: x["packages"].pop(),
                           lambda x: x["packages"][0].update(asset="../../escape"),
                           lambda x: x["packages"][0].update(sha256="0" * 64),
                           lambda x: x["packages"][0]["inventory"].pop(),
                           lambda x: x.update(revision="branch-name")]:
                value = copy.deepcopy(original)
                change(value)
                (output / "candidate.json").write_text(json.dumps(value))
                with self.assertRaises(ValueError):
                    P.verify(output)
            (output / "candidate.json").write_text(json.dumps(original))
            (output / "foreign.crate").write_bytes(b"foreign")
            with self.assertRaisesRegex(ValueError, "inventory"):
                P.verify(output)
            (output / "foreign.crate").unlink()
            asset = output / original["packages"][0]["asset"]
            asset.unlink()
            asset.symlink_to(output / original["packages"][1]["asset"])
            with self.assertRaisesRegex(ValueError, "regular file"):
                P.verify(output)

    def test_credential_boundary(self):
        with tempfile.TemporaryDirectory() as scratch:
            with patch.dict(os.environ, {"CARGO_HOME": scratch}, clear=True):
                self.assertEqual(P.environment()["CARGO_NET_OFFLINE"], "true")
                for key in ("CARGO_REGISTRY_TOKEN", "CARGO_REGISTRIES_CRATES_IO_TOKEN", "GH_TOKEN",
                            "ACTIONS_ID_TOKEN_REQUEST_TOKEN"):
                    with patch.dict(os.environ, {key: "test-credential"}):
                        with self.assertRaisesRegex(ValueError, "credential-free"):
                            P.environment()
                (Path(scratch) / "credentials.toml").write_text("not-a-real-credential")
                with self.assertRaisesRegex(ValueError, "credential files"):
                    P.environment()


if __name__ == "__main__":
    unittest.main()
