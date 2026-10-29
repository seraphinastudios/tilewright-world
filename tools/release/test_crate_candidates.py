"""Candidate bundles cross the release boundary without running build code."""
import importlib.util
import io
import json
import os
from pathlib import Path
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


if __name__ == "__main__":
    unittest.main()
