"""Candidate bundles cross the release boundary without running build code."""
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import tarfile
import tempfile
import unittest
from unittest import mock

import crate_candidates as C
import package
from test_package import Fixture

SPEC = importlib.util.spec_from_file_location(
    "candidate_fixture", Path(__file__).resolve().parents[1] / "crates/test_prepare.py")
F = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(F)


def tar(entries):
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w:gz") as stream:
        for name, data, kind in entries:
            item = tarfile.TarInfo(name)
            item.type = kind
            item.size = len(data) if kind == tarfile.REGTYPE else 0
            item.linkname = "candidate.json" if kind == tarfile.SYMTYPE else ""
            stream.addfile(item, io.BytesIO(data) if item.isfile() else None)
    return output.getvalue()


class CandidateBundleTests(unittest.TestCase):
    def setUp(self):
        self.scratch = tempfile.TemporaryDirectory()
        self.addCleanup(self.scratch.cleanup)
        self.root = Path(self.scratch.name)
        F.write_candidate(self.root)

    def entries(self):
        return [(path.name, path.read_bytes(), tarfile.REGTYPE) for path in self.root.iterdir()]

    def test_exact_bytes_deterministic_metadata_and_no_build_or_credentials(self):
        with mock.patch.dict(os.environ, {"GH_TOKEN": "test", "CARGO_REGISTRY_TOKEN": "test"}), \
                mock.patch.object(C.CANDIDATES, "environment", side_effect=AssertionError), \
                mock.patch.object(C.CANDIDATES, "build_graph", side_effect=AssertionError), \
                mock.patch.object(C.CANDIDATES.subprocess, "run", side_effect=AssertionError):
            first = C.bundle(self.root, F.REVISION)
            self.assertEqual(first, C.bundle(self.root, F.REVISION))
            C.verify(first, F.REVISION)
        with tarfile.open(fileobj=io.BytesIO(first), mode="r:gz") as stream:
            members = list(stream)
            self.assertEqual([m.name for m in members], sorted(p.name for p in self.root.iterdir()))
            for member in members:
                self.assertEqual(stream.extractfile(member).read(), (self.root / member.name).read_bytes())
                self.assertEqual((member.uid, member.gid, member.mtime, member.uname, member.gname),
                                 (0, 0, 0, "", ""))
        self.assertEqual(first[4:8], b"\0" * 4)

    def test_missing_extra_duplicate_traversal_symlink_and_directory_fail(self):
        entries = self.entries()
        for bad in (entries[:-1], entries + [entries[0]],
                    entries + [("extra", b"", tarfile.REGTYPE)],
                    [("../candidate.json", b"", tarfile.REGTYPE)],
                    [("candidate.json", b"", tarfile.SYMTYPE)],
                    [("candidate.json", b"", tarfile.DIRTYPE)]):
            with self.subTest(entries=[row[0] for row in bad]), self.assertRaises((ValueError, KeyError)):
                C.verify(tar(bad), F.REVISION)

    def test_stale_revision_manifest_or_archive_fails(self):
        with self.assertRaisesRegex(ValueError, "different source revision"):
            C.verify(C.bundle(self.root, F.REVISION), "b" * 40)
        manifest_path = self.root / "candidate.json"
        manifest = json.loads(manifest_path.read_text())
        manifest["packages"][0]["sha256"] = "0" * 64
        manifest_path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, "digest, identity or inventory"):
            C.verify(tar(self.entries()), F.REVISION)
        F.write_candidate(self.root)
        archive = self.root / "tilewright-jsonc-0.1.0.crate"
        archive.write_bytes(F.crate("tilewright-jsonc", [("extra", b"changed")]))
        with self.assertRaisesRegex(ValueError, "digest, identity or inventory"):
            C.bundle(self.root, F.REVISION)

    def test_file_and_total_size_bounds_and_symlink_input(self):
        data = tar(self.entries())
        with mock.patch.object(C, "MAX_FILE_BYTES", 1), self.assertRaisesRegex(ValueError, "size limit"):
            C.verify(data, F.REVISION)
        with mock.patch.object(C, "MAX_BUNDLE_BYTES", 1), self.assertRaisesRegex(ValueError, "size limit"):
            C.verify(data, F.REVISION)
        path = self.root / "candidate.json"
        path.unlink()
        path.symlink_to("missing")
        with self.assertRaisesRegex(ValueError, "regular file"):
            C.bundle(self.root, F.REVISION)

    def test_hidden_tar_metadata_is_bounded_before_parsing_and_rejected(self):
        for format in (tarfile.PAX_FORMAT, tarfile.GNU_FORMAT):
            output = io.BytesIO()
            with tarfile.open(fileobj=output, mode="w:gz", format=format) as stream:
                for name, data, _ in self.entries():
                    item = tarfile.TarInfo(name)
                    item.size = len(data)
                    if format == tarfile.PAX_FORMAT:
                        item.pax_headers = {"comment": "x" * (1024 * 1024)}
                    else:
                        item.uname = "x" * 1024  # GNU truncates uname; force a longname below.
                        item.name = "x" * 1024
                    stream.addfile(item, io.BytesIO(data))
            data = output.getvalue()
            if format == tarfile.PAX_FORMAT:
                with mock.patch.object(C, "MAX_BUNDLE_BYTES", 128 * 1024), \
                        self.assertRaisesRegex(ValueError, "decompressed archive size limit"):
                    C.verify(data, F.REVISION)
            with self.assertRaisesRegex(ValueError, "unsafe"):
                C.verify(data, F.REVISION)

    def test_release_assembly_checksums_and_requires_exact_revision(self):
        fixture = Fixture()
        self.addCleanup(fixture.close)
        with mock.patch.object(F, "REVISION", fixture.revision):
            F.write_candidate(self.root)
        output = fixture.root / "release"
        package.assemble(fixture.repository, "0.2.0", fixture.revision, fixture.binaries,
                         output, fixture.npm_tarball(), metadata=fixture.metadata,
                         toolchain=lambda *_: ("1.97.1", b"notices"), candidates=self.root)
        names = package.verify(output, "0.2.0", npm=True, revision=fixture.revision)
        self.assertIn(C.name("0.2.0"), names)
        with self.assertRaisesRegex(ValueError, "different source revision"):
            package.verify(output, "0.2.0", npm=True, revision="b" * 40)
        bundle = output / "assets" / C.name("0.2.0")
        bundle.write_bytes(bundle.read_bytes() + b"changed")
        with self.assertRaisesRegex(package.ReleaseError, "SHA-256"):
            package.verify(output, "0.2.0", npm=True, revision=fixture.revision)
        bundle.unlink()
        package.write_checksums(output / "assets")
        with self.assertRaisesRegex(package.ReleaseError, "missing assets"):
            package.verify(output, "0.2.0", npm=True, revision=fixture.revision)

    def test_verify_takes_the_release_trees_policy(self):
        version = C.CANDIDATES.policy()[0]
        later = F.next_minor(version)
        repository = F.Repository(self)
        F.set_version(repository.root, later)
        F.write_candidate(self.root, later, root=repository.root)
        files = {path.name: path.read_bytes() for path in self.root.iterdir()
                 if path.name.endswith(f"-{later}.crate") or path.name == "candidate.json"}
        data = tar([(name, value, tarfile.REGTYPE) for name, value in files.items()])
        self.assertEqual(set(C.verify(data, F.REVISION, repository.root)), set(files))
        with self.assertRaises((ValueError, KeyError)):
            C.verify(data, F.REVISION)

    def test_workflow_preserves_candidate_and_publish_boundaries(self):
        workflow = (C.CANDIDATES.ROOT / ".github/workflows/release.yml").read_text()
        job = workflow.split("  crates:\n", 1)[1].split("  package:\n", 1)[0]
        self.assertIn("contents: read", job)
        self.assertNotIn("id-token:", job)
        self.assertNotIn("GH_TOKEN", job)
        self.assertIn("persist-credentials: false", job)
        self.assertIn('prepare.py verify "${RUNNER_TEMP}/crate-candidates" --build', job)
        self.assertIn("needs: [plan, build, npm, crates]", workflow)
        self.assertEqual(workflow.count('verify --version "${VERSION}" --revision "${GITHUB_SHA}" --npm'), 2)
        for job_name in ("attest", "release"):
            job = workflow.split(f"  {job_name}:\n", 1)[1].split("    runs-on:", 1)[0]
            self.assertIn("github.event_name != 'pull_request'", job)
        self.assertNotIn("cargo publish", workflow)
        self.assertNotIn("cargo publish", (C.CANDIDATES.ROOT / ".github/workflows/publish-crate.yml").read_text())

    def test_crate_publication_job_only_reads_and_dispatches(self):
        workflow = (C.CANDIDATES.ROOT / ".github/workflows/release.yml").read_text()
        job = workflow.split("  crate-publication:\n", 1)[1].split("\n  npm-publish:\n", 1)[0]
        permissions = job.split("    permissions:\n", 1)[1].split("\n    env:\n", 1)[0]
        self.assertEqual(permissions.split("\n"), ["      contents: read", "      actions: write"])
        for absent in ("uses:", "id-token", "environment:", "checkout", "CARGO", "cargo"):
            self.assertNotIn(absent, job)
        self.assertIn("    needs: [plan, release]\n", job)
        condition = job.split("    if: ", 1)[1].split("\n", 1)[0]
        for part in ("!cancelled()", "github.event_name != 'pull_request'", "needs.plan.outputs.publish == 'true'",
                     "github.ref == 'refs/heads/main'", "needs.release.result == 'success'"):
            self.assertIn(part, condition)
        self.assertIn("GH_TOKEN: ${{ github.token }}", job)
        self.assertIn("ENABLED: ${{ vars.TILEWRIGHT_CRATES_PUBLISH == 'true' }}", job)
        self.assertIn('gh workflow run publish-crate.yml --ref main -R "${GH_REPO}" -f operation=publish', job)
        self.assertIn("environments/crates-io-release", job)
        crates = job.split("crates=(", 1)[1].split(")", 1)[0].split()
        self.assertEqual(tuple(crates), C.CANDIDATES.PUBLIC)
        # It sits after the release, which is out before it starts.
        self.assertLess(workflow.index("  release:\n"), workflow.index("  crate-publication:\n"))
        self.assertLess(workflow.index("  crate-publication:\n"), workflow.index("  npm-publish:\n"))

    def test_plan_outputs_reach_the_jobs_that_use_them(self):
        workflow = (C.CANDIDATES.ROOT / ".github/workflows/release.yml").read_text()
        plan = workflow.split("  plan:\n", 1)[1].split("\n    steps:\n", 1)[0]
        declared = dict(re.findall(r"^      ([a-z_]+): \$\{\{ steps\.(?:plan|crates)\.outputs\.([a-z_]+) \}\}$",
                                   plan, re.MULTILINE))
        self.assertEqual({key: value for key, value in declared.items() if key.startswith("crate_")},
                         {name: name for name in ("crate_version", "crate_owner", "crate_owner_tag", "crate_changed")})
        for used in set(re.findall(r"needs\.plan\.outputs\.([a-z_]+)", workflow)):
            self.assertIn(used, declared)
        notes = workflow.split("      - name: Write the release notes\n", 1)[1].split("\n      - name:", 1)[0]
        mapped = dict(re.findall(r"^          ([A-Z_]+): \$\{\{ needs\.plan\.outputs\.([a-z_]+) \}\}$",
                                 notes, re.MULTILINE))
        self.assertEqual(mapped, {"CRATE_VERSION": "crate_version", "CRATE_OWNER": "crate_owner",
                                  "CRATE_OWNER_TAG": "crate_owner_tag", "CRATE_CHANGED": "crate_changed"})
        script = (C.CANDIDATES.ROOT / "tools/release/notes.sh").read_text()
        self.assertEqual(set(re.findall(r"\$\{(CRATE_[A-Z_]+):-\}", script)), set(mapped))
        step = workflow.split("      - name: Decide crate version ownership\n", 1)[1].split("\n      - name:", 1)[0]
        self.assertIn("if: steps.plan.outputs.build == 'true'", step)
        self.assertIn("python3 tools/crates/publish.py owner", step)
        self.assertIn("--pull-request", step)
        self.assertIn('--tag "v${VERSION}"', step)


if __name__ == "__main__":
    unittest.main()
