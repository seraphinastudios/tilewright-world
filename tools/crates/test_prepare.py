#!/usr/bin/env python3
"""Credential-free candidate boundary tests; do not invoke Cargo."""
import copy
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location("candidate_prepare", Path(__file__).with_name("prepare.py"))
P = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(P)
REVISION = "a" * 40
# The workspace crate version: archives are checked against it, so the
# candidates written here carry it.
VERSION = P.policy()[0]


def crate(crate_name, extra=(), *, version=VERSION, revision=REVISION, dependencies=(), **package_changes):
    name = crate_name
    package = {"name": name, "version": version, "publish": ["crates-io"],
               "license": "MIT", "license-file": "LICENSE", "repository": P.REPOSITORY}
    package.update(package_changes)
    manifest = "[package]\n" + "".join(f"{key} = {json.dumps(value)}\n" for key, value in package.items())
    manifest += "".join(f'[dependencies.{dependency}]\nversion = "={version}"\n' for dependency in dependencies)
    files = [("Cargo.toml", manifest.encode()), ("Cargo.toml.orig", b"original"),
             (".cargo_vcs_info.json", json.dumps({"git": {"sha1": revision},
                                                "path_in_vcs": f"crates/{name}"}).encode()),
             ("LICENSE", (P.ROOT / "LICENSE").read_bytes()), ("README.md", b"candidate")]
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as stream:
        for path, data in [*files, *extra]:
            info = tarfile.TarInfo(f"{name}-{version}/{path}")
            info.size = len(data)
            stream.addfile(info, io.BytesIO(data))
    return buffer.getvalue()


def write_candidate(output, version=VERSION, revision=None, root=P.ROOT, graph=False):
    """Five candidate archives and their manifest; `graph` gives them the
    internal dependencies of the real crates/*/Cargo.toml."""
    revision = revision or REVISION
    rows = []
    for name in P.PUBLIC:
        data = crate(name, version=version, revision=revision,
                     dependencies=internal_dependencies(name) if graph else ())
        row = P.package_row(name, version, data, revision, root)
        rows.append(row)
        (output / row["asset"]).write_bytes(data)
    manifest = {"schema": 1, "status": "candidate-only", "repository": P.REPOSITORY,
                "revision": revision, "toolchain": P.policy(root)[1], "registry": "crates-io",
                "reproducible_packages": True, "packages": rows}
    (output / "candidate.json").write_text(json.dumps(manifest))
    return manifest


def internal_dependencies(name):
    manifest = P.load_toml(P.ROOT / f"crates/{name}/Cargo.toml")
    return sorted(key for key in manifest.get("dependencies", {}) if key in P.PUBLIC)


POLICY_FILES = ("Cargo.toml", "rust-toolchain.toml", "LICENSE", "README.md", "tools/crates/candidates.json",
                "scripts/release/release-policy.cjs", *(f"crates/{name}/Cargo.toml" for name in P.PUBLIC + P.PRIVATE))


def next_minor(version):
    major, minor, _ = map(int, version.split("."))
    return f"{major}.{minor + 1}.0"


def set_version(root, version):
    """Moves a policy tree's workspace crate version and exact internal pins."""
    manifest = root / "Cargo.toml"
    text = manifest.read_text()
    text, count = re.subn(r'(\[workspace\.package\]\nversion = ")[^"]+(")', rf"\g<1>{version}\g<2>", text)
    assert count == 1
    text, count = re.subn(r'(path = "crates/tilewright-[a-z]+", version = "=)[^"]+(")', rf"\g<1>{version}\g<2>", text)
    assert count == len(P.PUBLIC)
    manifest.write_text(text)


class Repository:
    """A git repository of this checkout's policy files, at chosen crate versions."""

    def __init__(self, test):
        scratch = tempfile.TemporaryDirectory()
        test.addCleanup(scratch.cleanup)
        self.root = Path(scratch.name)
        self.git("init", "-q", "-b", "main")
        for path in POLICY_FILES:
            (self.root / path).parent.mkdir(parents=True, exist_ok=True)
            shutil.copy(P.ROOT / path, self.root / path)

    def git(self, *arguments):
        return subprocess.run(["git", "-c", "user.name=Test", "-c", "user.email=test@example.invalid",
                               "-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false", *arguments],
                              cwd=self.root, check=True, capture_output=True, text=True).stdout.strip()

    def commit(self, version=None, files=None):
        if version:
            set_version(self.root, version)
        for path, data in (files or {}).items():
            (self.root / path).parent.mkdir(parents=True, exist_ok=True)
            (self.root / path).write_text(data)
        self.git("add", "-A")
        self.git("commit", "-q", "--allow-empty", "-m", "change")
        return self.git("rev-parse", "HEAD")

    def tag(self, name, revision="HEAD"):
        self.git("tag", name, revision)


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
                (target / f"{name}-{VERSION}.crate").write_bytes(data)

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
                    (target / f"{P.PUBLIC[0]}-{VERSION}.crate").write_bytes(b"changed")

            with patch.object(P, "run", side_effect=git), patch.object(P.subprocess, "run", side_effect=changed_package):
                output = Path(scratch) / "different"
                with self.assertRaisesRegex(ValueError, "differ"):
                    P.prepare(output)
                self.assertFalse((output / "candidate.json").exists())

    def test_repository_policy(self):
        workspace = P.load_toml(P.ROOT / "Cargo.toml")["workspace"]["package"]["version"]
        self.assertEqual(P.policy(), (workspace, "1.97.1"))

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
        P.archive(crate(name), name, VERSION, REVISION)
        for extra, changes, revision in [
            ([("../escape", b"no")], {}, REVISION),
            ([("README.md", b"duplicate")], {}, REVISION),
            ([], {"name": "foreign"}, REVISION),
            ([], {"publish": ["other"]}, REVISION),
            ([], {}, "b" * 40),
        ]:
            with self.subTest(extra=extra, changes=changes, revision=revision):
                with self.assertRaises(ValueError):
                    P.archive(crate(name, extra, **changes), name, VERSION, revision)

    def test_nested_archive_metadata_is_bounded_before_parsing(self):
        name = P.PUBLIC[0]
        output = io.BytesIO()
        with tarfile.open(fileobj=io.BytesIO(crate(name)), mode="r:gz") as source:
            with tarfile.open(fileobj=output, mode="w:gz", format=tarfile.PAX_FORMAT) as target:
                for item in source:
                    item.pax_headers = {"comment": "x" * (1024 * 1024)}
                    target.addfile(item, source.extractfile(item))
        data = output.getvalue()
        self.assertLess(len(data), 128 * 1024)
        with patch.object(P, "MAX_ARCHIVE_BYTES", 128 * 1024), \
                self.assertRaisesRegex(ValueError, "decompressed archive size limit"):
            P.archive(data, name, VERSION, REVISION)
        with self.assertRaisesRegex(ValueError, "unsafe"):
            P.archive(data, name, VERSION, REVISION)

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
            P.archive(buffer.getvalue(), name, VERSION, REVISION)
        buffer = io.BytesIO()
        with tarfile.open(fileobj=buffer, mode="w:gz") as target:
            info = tarfile.TarInfo(f"{name}-{VERSION}/link")
            info.type = tarfile.SYMTYPE
            info.linkname = "/tmp/outside"
            target.addfile(info)
        with self.assertRaisesRegex(ValueError, "unsafe"):
            P.archive(buffer.getvalue(), name, VERSION, REVISION)

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
                P.archive(buffer.getvalue(), name, VERSION, REVISION)

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



class ReleaseTreeTests(unittest.TestCase):
    """main verifies a release's bytes against the policy files of its tree."""

    def test_policy_and_archives_follow_the_given_tree(self):
        version, toolchain = P.policy()
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            for path in POLICY_FILES:
                (root / path).parent.mkdir(parents=True, exist_ok=True)
                shutil.copy(P.ROOT / path, root / path)
            later = next_minor(version)
            set_version(root, later)
            self.assertEqual(P.policy(root), (later, toolchain))
            name = P.PUBLIC[0]
            data = crate(name, version=later)
            P.archive(data, name, later, REVISION, root)
            (root / "LICENSE").write_bytes(b"another notice")
            with self.assertRaisesRegex(ValueError, "root MIT license notice"):
                P.archive(data, name, later, REVISION, root)
            with self.assertRaisesRegex(ValueError, "root MIT license notice"):
                P.package_row(name, later, data, REVISION, root)

    def test_verify_files_takes_the_tree_policy(self):
        version, _ = P.policy()
        later = next_minor(version)
        with tempfile.TemporaryDirectory() as scratch, tempfile.TemporaryDirectory() as tree:
            root = Path(tree)
            for path in POLICY_FILES:
                (root / path).parent.mkdir(parents=True, exist_ok=True)
                shutil.copy(P.ROOT / path, root / path)
            set_version(root, later)
            output = Path(scratch)
            write_candidate(output, later, root=root)
            files = {path.name: path.read_bytes() for path in output.iterdir()}
            manifest, archives = P.verify_files(files, root)
            self.assertEqual({row["version"] for row in manifest["packages"]}, {later})
            self.assertEqual(len(archives), 5)
            with self.assertRaises((ValueError, KeyError)):
                P.verify_files(files)

    def test_older_trees_without_an_abandoned_list_still_pass(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            for path in POLICY_FILES:
                (root / path).parent.mkdir(parents=True, exist_ok=True)
                shutil.copy(P.ROOT / path, root / path)
            config = root / "tools/crates/candidates.json"
            value = json.loads(config.read_text())
            value.pop("abandoned")
            config.write_text(json.dumps(value))
            self.assertEqual(P.policy(root), P.policy())
            self.assertEqual(P.candidates(root), [])

    def test_abandoned_list_is_checked_for_its_form(self):
        self.assertEqual(P.candidates(), [])
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            (root / "tools/crates").mkdir(parents=True)
            config = root / "tools/crates/candidates.json"
            value = json.loads((P.ROOT / "tools/crates/candidates.json").read_text())
            value["abandoned"] = [{"version": "0.2.0", "release": "v0.7.0"}]
            config.write_text(json.dumps(value))
            self.assertEqual(P.candidates(root), [("0.2.0", "v0.7.0")])
            for abandoned in ([{"version": "0.2", "release": "v0.7.0"}], [{"version": "0.2.0", "release": "0.7.0"}],
                              [{"version": "0.2.0"}], [{"version": "0.2.0", "release": "v0.7.0", "why": "x"}],
                              [{"version": "0.2.0", "release": "v0.7.0"}, {"version": "0.2.0", "release": "v0.8.0"}],
                              {"version": "0.2.0"}):
                value["abandoned"] = abandoned
                config.write_text(json.dumps(value))
                with self.subTest(abandoned=abandoned), self.assertRaisesRegex(ValueError, "abandoned"):
                    P.candidates(root)
            config.write_text("[]")
            with self.assertRaisesRegex(ValueError, "allowlist"):
                P.candidates(root)


class CheckVersionTests(unittest.TestCase):
    def setUp(self):
        self.version = P.policy()[0]
        self.repository = Repository(self)
        self.base = self.repository.commit()

    def check(self, version):
        if version != P.policy(self.repository.root)[0]:
            set_version(self.repository.root, version)
        return P.check_version(self.base, self.repository.root)

    def test_unchanged_next_patch_and_next_minor_pass(self):
        major, minor, patch = map(int, self.version.split("."))
        for version, step in ((self.version, "unchanged"), (f"{major}.{minor}.{patch + 1}", "the next patch version"),
                              (f"{major}.{minor + 1}.0", "the next minor version")):
            with self.subTest(version=version):
                self.assertEqual(self.check(version), (self.version, version, step))

    def test_backward_skipping_and_major_moves_fail(self):
        major, minor, patch = map(int, self.version.split("."))
        self.repository.commit(f"{major}.{minor + 1}.1")
        self.base = self.repository.git("rev-parse", "HEAD")
        for version, message in ((self.version, "backward"), (f"{major}.{minor + 1}.0", "backward"),
                                 (f"{major}.{minor + 1}.3", "may only stay"), (f"{major}.{minor + 3}.0", "may only stay"),
                                 (f"{major}.{minor + 2}.1", "may only stay"), (f"{major + 1}.0.0", "may only stay")):
            with self.subTest(version=version), self.assertRaisesRegex(ValueError, message):
                self.check(version)

    def test_major_stays_zero_while_pre_1_0(self):
        policy = self.repository.root / "scripts/release/release-policy.cjs"
        self.assertTrue(P.pre_1_0(self.repository.root))
        self.repository.commit("1.0.0")
        self.base = self.repository.git("rev-parse", "HEAD")
        with self.assertRaisesRegex(ValueError, "PRE_1_0"):
            self.check("1.0.1")
        policy.write_text(policy.read_text().replace("const PRE_1_0 = true;", "const PRE_1_0 = false;"))
        self.assertEqual(self.check("1.0.1")[2], "the next patch version")
        policy.write_text("const PRE_1_0 = true;\nconst PRE_1_0 = false;\n")
        with self.assertRaisesRegex(ValueError, "exactly once"):
            P.pre_1_0(self.repository.root)

    def test_lockstep_and_pins_come_from_policy(self):
        package = self.repository.root / "crates/tilewright-edit/Cargo.toml"
        package.write_text(package.read_text().replace("version.workspace = true", 'version = "9.9.9"'))
        with self.assertRaisesRegex(ValueError, "identity"):
            P.check_version(self.base, self.repository.root)
        shutil.copy(P.ROOT / "crates/tilewright-edit/Cargo.toml", package)
        manifest = self.repository.root / "Cargo.toml"
        manifest.write_text(manifest.read_text().replace(f'version = "={self.version}"', 'version = "=9.9.9"', 1))
        with self.assertRaisesRegex(ValueError, "exact candidate version"):
            P.check_version(self.base, self.repository.root)

    def test_missing_or_option_like_base_fails(self):
        for base in ("", "--help", "f" * 40, "no-such-ref"):
            with self.subTest(base=base), self.assertRaises(ValueError):
                P.check_version(base, self.repository.root)


if __name__ == "__main__":
    unittest.main()
