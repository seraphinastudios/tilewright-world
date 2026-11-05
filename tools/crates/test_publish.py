#!/usr/bin/env python3
"""Crate publication tests. GitHub, crates.io, gh and time are faked; git is real."""
import argparse
import base64
import copy
import gzip
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import subprocess
import tarfile
import tempfile
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("world_publish", Path(__file__).with_name("publish.py"))
P = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(P)
FIXTURE_SPEC = importlib.util.spec_from_file_location("world_publish_fixture", Path(__file__).with_name("test_prepare.py"))
F = importlib.util.module_from_spec(FIXTURE_SPEC)
FIXTURE_SPEC.loader.exec_module(F)
PUBLIC = P.PREPARE.PUBLIC
CURRENT = P.PREPARE.policy()[0]
LATER = F.next_minor(CURRENT)
READ, INTENT, CRATES = "read-token-for-tests", "intent-token-for-tests", "crates-token-for-tests"
RUN_ID = "4242"
ZOEY = {"kind": "user", "login": "zoeyrose", "id": 437663}


def bundle_bytes(files):
    """A USTAR bundle like crate_candidates.bundle, at any crate version."""
    output = io.BytesIO()
    with tarfile.open(fileobj=output, mode="w", format=tarfile.USTAR_FORMAT) as stream:
        for name, data in sorted(files.items()):
            item = tarfile.TarInfo(name)
            item.size = len(data)
            item.mode = 0o644
            stream.addfile(item, io.BytesIO(data))
    return gzip.compress(output.getvalue(), mtime=0)


def completed(returncode=0, stdout=b""):
    return subprocess.CompletedProcess([], returncode, stdout, b"")


class World:
    """GitHub and crates.io for one owning release at crate version LATER."""

    def __init__(self, test, *, owner=True):
        self.test = test
        self.repository = F.Repository(test)
        root = self.repository.root
        self.repository.commit()
        self.repository.tag("v0.1.0")
        self.repository.tag("go/v0.1.0")
        self.version = LATER if owner else CURRENT
        self.revision = self.repository.commit(self.version) if owner else self.repository.commit(files={"x": "y"})
        self.tag = "v0.7.0"
        self.repository.tag(self.tag)
        self.repository.tag("go/" + self.tag)
        self.root = root
        with tempfile.TemporaryDirectory() as scratch:
            output = Path(scratch)
            F.write_candidate(output, self.version, self.revision, root, graph=True)
            self.files = {path.name: path.read_bytes() for path in output.iterdir()}
        self.manifest = json.loads(self.files["candidate.json"])
        self.rows = {row["name"]: row for row in self.manifest["packages"]}
        self.bundle = bundle_bytes(self.files)
        self.digest = hashlib.sha256(self.bundle).hexdigest()
        self.name = f"tilewright-world-{self.tag[1:]}-crate-candidates.tar.gz"
        self.sums = f"{self.digest}  {self.name}\n{'c' * 64}  tilewright-{self.tag[1:]}-linux-amd64.tar.gz\n".encode()
        download = f"https://github.com/{P.REPOSITORY}/releases/download/{self.tag}/"
        self.downloads = {download + self.name: self.bundle, download + "SHA256SUMS": self.sums}
        self.release = {"id": 77, "node_id": "R_77", "tag_name": self.tag, "draft": False, "prerelease": False,
                        "immutable": True, "published_at": "2026-11-05T00:00:00Z", "assets": [
                            {"id": 1, "node_id": "A_1", "name": self.name, "size": len(self.bundle),
                             "digest": "sha256:" + self.digest, "state": "uploaded",
                             "browser_download_url": download + self.name, "download_count": 0},
                            {"id": 2, "node_id": "A_2", "name": "SHA256SUMS", "size": len(self.sums),
                             "digest": "sha256:" + hashlib.sha256(self.sums).hexdigest(), "state": "uploaded",
                             "browser_download_url": download + "SHA256SUMS", "download_count": 0}]}
        self.registry = {name: {} for name in PUBLIC}
        self.owners = {name: [ZOEY] for name in PUBLIC}
        self.refs, self.tags = {}, {}
        self.outcomes = {}
        self.puts, self.writes, self.events = [], [], []
        self.attested = True
        self.main_candidates = None  # main's candidates.json now, if not the checkout's

    def values(self, operation="publish"):
        return {"OPERATION": operation, "RELEASE_TAG": self.tag, "SOURCE_REVISION": self.revision,
                "BUNDLE_SHA256": self.digest}

    def publish(self, name, *, run_id=RUN_ID, version=None, data=None, by_hand=False, **flags):
        version = version or self.version
        data = data if data is not None else self.files[f"{name}-{version}.crate"]
        self.registry[name][version] = dict(
            {"checksum": hashlib.sha256(data).hexdigest(), "yanked": False, "api": True, "index": True,
             "cdn": True, "data": data, "published_by": {"login": "zoeyrose"} if by_hand else None,
             "trustpub_data": None if by_hand else {"provider": "github", "repository": P.REPOSITORY,
                                                    "run_id": run_id, "sha": "e" * 40}}, **flags)

    def record(self, space, name, number=None, *, release=None, attempt=None, kind=None, sha256=None,
               reason="http-403"):
        short = f"{space}/{name}/{self.version}" + (f"/{number}" if number else "")
        release = release or {"tag": self.tag, "revision": self.revision, "bundle_sha256": self.digest}
        message = {"schema": 1, "kind": kind or {P.LEGACY: P.LEGACY_KIND, P.ATTEMPT: P.ATTEMPT_KIND,
                                                 P.REFUSED: P.REFUSED_KIND}[space],
                   "repository_id": P.REPOSITORY_ID, "release": release,
                   "coordinate": {"name": name, "version": self.version,
                                  "sha256": sha256 or self.rows[name]["sha256"]}}
        if number:
            message["attempt"] = attempt or number
        if space == P.REFUSED:
            message["attempt_ref"] = f"refs/tags/{P.ATTEMPT}/{name}/{self.version}/{number}"
            message["reason"] = reason
        self.github_write("git/tags", {"tag": short, "message": json.dumps(message), "object": self.revision,
                                       "type": "commit"}, INTENT, quiet=True)
        sha = hashlib.sha1(json.dumps(message).encode()).hexdigest()
        self.github_write("git/refs", {"ref": "refs/tags/" + short, "sha": sha}, INTENT, quiet=True)

    # --- GitHub ---

    def github(self, suffix, token, *, absent=False, readback=False):
        self.test.assertIn(token, (READ, INTENT))
        if suffix == "":
            return {"id": P.REPOSITORY_ID, "full_name": P.REPOSITORY, "default_branch": "main", "archived": False}
        if suffix == "branches/main":
            return {"protected": True}
        if suffix == f"releases/tags/{self.tag}":
            return copy.deepcopy(self.release)
        if suffix in (f"commits/{self.tag}", f"commits/go%2F{self.tag}"):
            return {"sha": self.revision}
        if suffix == f"compare/{self.revision}...main":
            return {"status": "ahead", "base_commit": {"sha": self.revision}}
        if suffix == "contents/tools/crates/candidates.json?ref=main":
            data = self.main_candidates or (self.root / "tools/crates/candidates.json").read_bytes()
            return {"type": "file", "encoding": "base64", "size": len(data),
                    "content": base64.encodebytes(data).decode()}
        if suffix.startswith(f"attestations/sha256:{self.digest}?"):
            return {"attestations": [{"repository_id": P.REPOSITORY_ID, "bundle": {"mediaType": "test"}}]} \
                if self.attested else None
        if suffix.startswith("git/matching-refs/tags/"):
            prefix = "refs/tags/" + suffix[len("git/matching-refs/tags/"):]
            return [{"ref": ref, "object": dict(value)} for ref, value in sorted(self.refs.items())
                    if ref.startswith(prefix)]
        if suffix.startswith("git/ref/tags/"):
            ref = "refs/tags/" + suffix[len("git/ref/tags/"):]
            if ref in self.refs:
                return {"ref": ref, "object": dict(self.refs[ref])}
            if absent:
                return None
            raise P.Failure("GitHub", 404)
        if suffix.startswith("git/tags/"):
            return copy.deepcopy(self.tags[suffix[len("git/tags/"):]])
        raise AssertionError(suffix)

    def github_write(self, suffix, body, token, quiet=False):
        self.test.assertEqual(token, INTENT)
        if not quiet:
            self.writes.append((suffix, body))
            self.events.append(("write", suffix, body.get("ref") or body.get("tag")))
        if suffix == "git/tags":
            sha = hashlib.sha1(body["message"].encode()).hexdigest()
            self.tags[sha] = {"sha": sha, "tag": body["tag"], "message": body["message"],
                              "object": {"sha": body["object"], "type": body["type"]}}
            return {"sha": sha}
        if suffix == "git/refs":
            if body["ref"] in self.refs:
                raise P.Failure("GitHub", 422)
            self.refs[body["ref"]] = {"type": "tag", "sha": body["sha"]}
            return {"ref": body["ref"], "object": {"type": "tag", "sha": body["sha"]}}
        raise AssertionError(suffix)

    # --- downloads and crates.io ---

    def request(self, url, *, token=None, absent=False, maximum=P.MAX_JSON, **_):
        self.test.assertIsNone(token)
        if url in self.downloads:
            return self.downloads[url]
        match = re.fullmatch(r"https://crates\.io/api/v1/crates/([a-z-]+)/owners", url)
        if match:
            return json.dumps({"users": self.owners[match[1]]}).encode()
        match = re.fullmatch(r"https://crates\.io/api/v1/crates/([a-z-]+)/([0-9.]+)", url)
        if match:
            entry = self.registry[match[1]].get(match[2])
            if entry and entry["api"]:
                return json.dumps({"version": {"crate": match[1], "num": match[2], "checksum": entry["checksum"],
                                               "yanked": entry["yanked"], "published_by": entry["published_by"],
                                               "trustpub_data": entry["trustpub_data"]}}).encode()
            return self.absent(absent, 404)
        match = re.fullmatch(r"https://index\.crates\.io/ti/le/([a-z-]+)", url)
        if match:
            lines = [json.dumps({"name": match[1], "vers": version, "cksum": entry["checksum"],
                                 "yanked": entry["yanked"]}) for version, entry in self.registry[match[1]].items()
                     if entry["index"]]
            return "\n".join(lines).encode() if lines else self.absent(absent, 404)
        match = re.fullmatch(r"https://static\.crates\.io/crates/([a-z-]+)/\1-([0-9.]+)\.crate", url)
        if match:
            entry = self.registry[match[1]].get(match[2])
            # As static.crates.io does, a file not served answers 403.
            return entry["data"] if entry and entry["cdn"] else self.absent(absent, 403)
        raise AssertionError(url)

    @staticmethod
    def absent(absent, status):
        if absent and status in ((404,) if absent is True else absent):
            return None
        raise P.Failure("crates.io", status)

    def put(self, data, metadata, token):
        self.test.assertEqual(token, CRATES)
        name = metadata["name"]
        self.puts.append(name)
        self.events.append(("put", name))
        outcome = (self.outcomes.get(name) or ["ok"]).pop(0)
        if outcome in ("ok", "timeout-published", "other-run"):
            self.publish(name, data=data, run_id="999" if outcome == "other-run" else RUN_ID)
        if outcome == "ok" or outcome == "other-run":
            return P.PutOutcome(200, "acknowledged", "acknowledged")
        if isinstance(outcome, int):
            category = "refused" if outcome in (401, 403) else "rate_limited" if outcome == 429 else "uncertain"
            return P.PutOutcome(outcome, category, "http_error")
        return P.PutOutcome(None, "uncertain", "timeout")

    # --- the GitHub CLI ---

    def gh(self, arguments, directory, token=None):
        if arguments == ["version"]:
            return completed(stdout=b"gh version 2.83.1 (2026-10-01)\n")
        if arguments[:2] == ["attestation", "verify"]:
            self.test.assertIsNone(token)
            self.test.assertEqual(arguments[-2], "--")
            self.test.assertTrue((directory / "bundles.jsonl").is_file())
            self.test.assertEqual(Path(arguments[-1]).read_bytes(), self.bundle)
            return completed(stdout=json.dumps(attested(self.revision, self.digest)).encode())
        if arguments[:2] == ["release", "verify-asset"]:
            self.test.assertEqual(token, READ)
            self.test.assertEqual(arguments[-3:-1], ["--", self.tag])
            return completed()
        raise AssertionError(arguments)

    def patches(self, stack):
        for name, value in (("github", self.github), ("github_write", self.github_write),
                            ("request", self.request), ("put_once", self.put), ("gh", self.gh)):
            stack.enter_context(mock.patch.object(P, name, side_effect=value))
        stack.enter_context(mock.patch.object(P.time, "sleep"))


def attested(revision, digest, **changes):
    certificate = {"subjectAlternativeName": P.SIGNER, "issuer": "https://token.actions.githubusercontent.com",
                   "sourceRepositoryURI": P.REPO_URL, "sourceRepositoryDigest": revision,
                   "sourceRepositoryRef": "refs/heads/main", "sourceRepositoryIdentifier": str(P.REPOSITORY_ID),
                   "buildConfigURI": P.SIGNER, "buildConfigDigest": revision, "runnerEnvironment": "github-hosted",
                   "runInvocationURI": f"{P.REPO_URL}/actions/runs/123/attempts/1"}
    certificate.update(changes)
    return [{"verificationResult": {"signature": {"certificate": certificate},
                                    "statement": {"predicateType": P.SLSA,
                                                  "subject": [{"digest": {"sha256": digest}}]}}}]


class Context:
    """A World patched in, with a fresh evidence and receipts directory."""

    def __init__(self, test, **world):
        from contextlib import ExitStack
        self.world = World(test, **world)
        self.stack = ExitStack()
        test.addCleanup(self.stack.close)
        self.world.patches(self.stack)
        scratch = tempfile.TemporaryDirectory()
        test.addCleanup(scratch.cleanup)
        self.scratch = Path(scratch.name)
        self.stack.enter_context(mock.patch.dict(os.environ, {
            "GITHUB_RUN_ID": RUN_ID, "GITHUB_RUN_ATTEMPT": "1", "GITHUB_SHA": "e" * 40,
            "TILEWRIGHT_PUBLISH_RECEIPTS": str(self.scratch / "receipts")}))
        # Progress and receipts print; keep the test output readable.
        self.stdout = self.stack.enter_context(mock.patch("sys.stdout", new_callable=io.StringIO))
        self.stderr = self.stack.enter_context(mock.patch("sys.stderr", new_callable=io.StringIO))
        self.count = 0

    def verify(self, operation="publish", token=READ):
        self.count += 1
        return P.verify_release(self.world.values(operation), token, self.scratch / f"evidence-{self.count}",
                                fresh=True, root=self.world.root)

    def upload(self):
        values = self.world.values()
        plan = self.verify()
        evidence = self.scratch / f"evidence-{self.count}"
        plan = P.verify_release(values, INTENT, evidence, fresh=False, root=self.world.root)
        P.upload(plan, values, CRATES, INTENT, P.Receipts())
        return plan


class OwnershipTests(unittest.TestCase):
    def setUp(self):
        self.repository = F.Repository(self)

    def own(self, revision, tag=None):
        return P.ownership(revision, tag, self.repository.root)

    def test_first_release_owns(self):
        revision = self.repository.commit()
        result = self.own(revision)
        self.assertEqual((result["owner"], result["owner_tag"], result["previous_tag"]), (True, None, None))
        self.repository.tag("v0.1.0")
        self.assertEqual(self.own(revision, "v0.1.0")["owner_tag"], "v0.1.0")

    def test_higher_version_owns_equal_does_not_and_lower_is_backward(self):
        self.repository.commit()
        self.repository.tag("v0.1.0")
        self.repository.commit(files={"crates/tilewright-jsonc/src/lib.rs": "a"})
        self.repository.tag("v0.2.0")
        same = self.repository.commit(files={"crates/tilewright-jsonc/src/lib.rs": "b"})
        result = self.own(same, "v0.3.0")
        self.assertEqual((result["owner"], result["owner_tag"], result["previous_tag"]), (False, "v0.1.0", "v0.2.0"))
        self.assertEqual(result["changed"], ["tilewright-jsonc"])
        bumped = self.repository.commit(LATER)
        result = self.own(bumped, "v0.3.0")
        self.assertEqual((result["owner"], result["owner_tag"], result["crate_version"]), (True, "v0.3.0", LATER))
        self.assertEqual(result["previous_crate_version"], CURRENT)
        self.repository.tag("v0.3.0")
        back = self.repository.commit(CURRENT)
        with self.assertRaises(P.Backward):
            self.own(back, "v0.4.0")

    def test_other_tags_are_ignored_and_two_release_tags_at_a_revision_refused(self):
        first = self.repository.commit()
        for tag in ("v0.1.0", "go/v0.1.0", "crate-upload-intent/tilewright-jsonc/0.1.0", "v1.2", "vfoo", "v01.0.0"):
            self.repository.tag(tag)
        revision = self.repository.commit(LATER)
        for tag in ("go/v9.0.0", "crate-upload-attempt/tilewright-jsonc/9.0.0/1"):
            self.repository.tag(tag)
        result = self.own(revision, "v0.2.0")
        self.assertEqual((result["owner"], result["previous_tag"], result["tags_at_revision"]), (True, "v0.1.0", []))
        self.repository.tag("v0.2.0")
        self.repository.tag("v0.3.0")
        with self.assertRaisesRegex(P.Refusal, "more than one release tag"):
            self.own(revision, "v0.2.0")
        with self.assertRaisesRegex(P.Refusal, "another release tag"):
            self.own(first, "v0.9.0")

    def test_a_superseded_run_releases_the_bump_with_the_next_merge(self):
        self.repository.commit()
        self.repository.tag("v0.1.0")
        self.repository.commit(LATER)  # the bump, whose run was cancelled
        merged = self.repository.commit(files={"docs.md": "x"})
        self.repository.tag("v0.2.0")
        result = self.own(merged, "v0.2.0")
        self.assertEqual((result["owner"], result["owner_tag"]), (True, "v0.2.0"))
        later = self.repository.commit(files={"docs.md": "y"})
        self.assertEqual(self.own(later, "v0.3.0")["owner_tag"], "v0.2.0")

    def test_backward_history_is_an_error_not_a_stop(self):
        self.repository.commit(LATER)
        self.repository.tag("v0.1.0")
        self.repository.commit(CURRENT)
        self.repository.tag("v0.2.0")
        revision = self.repository.commit(CURRENT, files={"x": "y"})
        with self.assertRaises(P.Refusal) as caught:
            self.own(revision, "v0.3.0")
        self.assertNotIsInstance(caught.exception, P.Backward)

    def test_changed_counts_packaged_content_only(self):
        self.repository.commit()
        self.repository.tag("v0.1.0")
        base = self.repository.commit(files={"crates/tilewright-pack/tests/t.rs": "a"})
        cases = [({"Cargo.lock": "x", "rust-toolchain.toml.bak": "x", "crates/tilewright-pack/tests/t.rs": "b",
                   "crates/tilewright-edit/benches/b.rs": "c", "docs/x.md": "d"}, []),
                 ({"crates/tilewright-edit/src/lib.rs": "e"}, ["tilewright-edit"]),
                 ({"crates/tilewright-diff/Cargo.toml": (F.P.ROOT / "crates/tilewright-diff/Cargo.toml").read_text()
                   + "\n# changed\n"}, ["tilewright-diff"]),
                 ({"README.md": "new"}, list(PUBLIC)),
                 ({"LICENSE": "new"}, list(PUBLIC))]
        for files, expected in cases:
            with self.subTest(files=sorted(files)):
                revision = self.repository.commit(files=files)
                self.assertEqual(P.changed_crates(base, revision, self.repository.root), expected)
                base = revision
        manifest = self.repository.root / "Cargo.toml"
        manifest.write_text(re.sub(r'rusqlite = \{ version = "[^"]+"', 'rusqlite = { version = "0.99.0"',
                                   manifest.read_text()))
        revision = self.repository.commit()
        self.assertEqual(P.changed_crates(base, revision, self.repository.root), ["tilewright-pack"])

    def test_owner_command_outputs_and_failures(self):
        self.repository.commit()
        self.repository.tag("v0.1.0")
        revision = self.repository.commit(LATER)
        real = P.ownership
        with tempfile.TemporaryDirectory() as scratch, \
                mock.patch.object(P, "ownership", side_effect=lambda r, t: real(r, t, self.repository.root)):
            output, summary = Path(scratch) / "output", Path(scratch) / "summary"

            def run(*arguments, enabled="true"):
                output.write_text("")
                parser = argparse.Namespace(revision=revision, previous_tag="v0.1.0", tag=None, pull_request=False,
                                            github_output=True)
                for key, value in arguments:
                    setattr(parser, key, value)
                with mock.patch.dict(os.environ, {"GITHUB_OUTPUT": str(output), "GITHUB_STEP_SUMMARY": str(summary),
                                                  "TILEWRIGHT_CRATES_PUBLISH": enabled}), \
                        mock.patch("sys.stdout", new_callable=io.StringIO) as stdout:
                    code = P.owner_command(parser)
                return code, dict(line.split("=", 1) for line in output.read_text().splitlines()), stdout.getvalue()

            code, values, _ = run(("tag", "v0.2.0"))
            self.assertEqual((code, values), (0, {"crate_version": LATER, "crate_owner": "true",
                                                  "crate_owner_tag": "v0.2.0", "crate_changed": ""}))
            self.assertIn("starts the \"Publish crates\" workflow", summary.read_text())
            run(("tag", "v0.2.0"), enabled="")
            self.assertIn("publication is off", summary.read_text())
            code, values, _ = run(("pull_request", True))
            self.assertEqual((code, values["crate_owner"], values["crate_owner_tag"]), (0, "true", ""))
            self.assertIn("Merging this pull request", summary.read_text())
            for changes in ((("tag", "v0.2.0"), ("previous_tag", "v0.0.9")), (("tag", "0.2.0"),),
                            (("tag", "v0.2.0"), ("revision", "f" * 40))):
                code, values, stdout = run(*changes)
                with self.subTest(changes=changes):
                    self.assertEqual((code, values["crate_owner"]), (0, "error"))
                    self.assertIn("::warning", stdout)
            self.repository.tag("v0.2.0")
            back = self.repository.commit(CURRENT)
            code, values, stdout = run(("tag", "v0.3.0"), ("revision", back), ("previous_tag", "v0.2.0"))
            self.assertEqual((code, values), (1, {}))
            self.assertIn("::error", stdout)


class BoundaryTests(unittest.TestCase):
    REVISION = "e" * 40
    ENVIRONMENT = {"GITHUB_ACTIONS": "true", "GITHUB_REPOSITORY": P.REPOSITORY,
                   "GITHUB_REPOSITORY_ID": str(P.REPOSITORY_ID), "GITHUB_REF": "refs/heads/main",
                   "GITHUB_EVENT_NAME": "workflow_dispatch", "GITHUB_WORKFLOW_REF": P.WORKFLOW_REF,
                   "GITHUB_SHA": "e" * 40, "GITHUB_WORKFLOW_SHA": "e" * 40}

    def check(self, changes=None, *, upload=False, head=None, status=""):
        environment = dict(self.ENVIRONMENT, **(changes or {}))
        environment = {key: value for key, value in environment.items() if value is not None}
        answers = {"rev-parse": (head or self.REVISION) + "\n", "status": status}
        with mock.patch.dict(os.environ, environment, clear=True), \
                mock.patch.object(P, "git", side_effect=lambda arguments, root=P.ROOT: answers[arguments[0]]):
            P.boundary(upload=upload)

    def test_publisher_identity(self):
        self.check()
        for key, value in (("GITHUB_REPOSITORY", "someone/tilewright-world"), ("GITHUB_REPOSITORY_ID", "1"),
                           ("GITHUB_REF", "refs/heads/feature"), ("GITHUB_EVENT_NAME", "push"),
                           ("GITHUB_WORKFLOW_REF", f"{P.REPOSITORY}/.github/workflows/release.yml@refs/heads/main"),
                           ("GITHUB_WORKFLOW_REF", f"{P.REPOSITORY}/.github/workflows/publish-crate.yml@refs/heads/x"),
                           ("GITHUB_WORKFLOW_SHA", "f" * 40), ("GITHUB_SHA", None), ("GITHUB_ACTIONS", None)):
            with self.subTest(key=key, value=value), self.assertRaises(P.Refusal):
                self.check({key: value})
        with self.assertRaisesRegex(P.Refusal, "not the workflow's revision"):
            self.check(head="f" * 40)
        with self.assertRaisesRegex(P.Refusal, "not clean"):
            self.check(status="?? stray\n")

    def test_credentials_stay_in_the_upload_step(self):
        for key in ("CARGO_REGISTRY_TOKEN", "GH_INTENT_TOKEN", "CARGO_REGISTRIES_CRATES_IO_TOKEN"):
            with self.subTest(key=key), self.assertRaises(P.Refusal):
                self.check({key: "secret"})
        self.check({"CARGO_REGISTRY_TOKEN": "secret"}, upload=True)
        for key in ("GH_TOKEN", "GH_READ_TOKEN", "GITHUB_TOKEN", "CARGO_REGISTRIES_CRATES_IO_TOKEN"):
            with self.subTest(key=key), self.assertRaises(P.Refusal):
                self.check({key: "secret"}, upload=True)

    def test_dispatch_inputs(self):
        good = {"OPERATION": "publish", "RELEASE_TAG": "v0.7.0", "SOURCE_REVISION": "a" * 40,
                "BUNDLE_SHA256": "b" * 64}
        with mock.patch.dict(os.environ, good, clear=True):
            self.assertEqual(P.dispatch_inputs(), good)
        for key, value in (("OPERATION", "automatic"), ("RELEASE_TAG", "--repo=other/repo"), ("RELEASE_TAG", "v0.7"),
                           ("RELEASE_TAG", "v0.7.0/../x"), ("SOURCE_REVISION", "main"), ("BUNDLE_SHA256", "B" * 64)):
            with self.subTest(key=key, value=value), mock.patch.dict(os.environ, dict(good, **{key: value}), clear=True):
                with self.assertRaisesRegex(P.Refusal, "inputs"):
                    P.dispatch_inputs()

    def test_upload_command_pops_its_tokens_first(self):
        seen = {}

        def verify(values, token, evidence, *, fresh):
            seen.update(token=token, fresh=fresh, environment=dict(os.environ))
            raise P.Refusal("stop here")

        environment = {"CARGO_REGISTRY_TOKEN": CRATES, "GH_INTENT_TOKEN": INTENT, "TILEWRIGHT_CRATES_PUBLISH": "true",
                       "OPERATION": "publish", "RELEASE_TAG": "v0.7.0", "SOURCE_REVISION": "a" * 40,
                       "BUNDLE_SHA256": "b" * 64, "GITHUB_RUN_ID": "1", "GITHUB_RUN_ATTEMPT": "1"}
        with tempfile.TemporaryDirectory() as scratch, \
                mock.patch.dict(os.environ, dict(environment, TILEWRIGHT_PUBLISH_RECEIPTS=scratch), clear=True), \
                mock.patch.object(P, "boundary") as boundary, mock.patch.object(P, "verify_release", side_effect=verify):
            with self.assertRaisesRegex(P.Refusal, "stop here"):
                P.upload_command(argparse.Namespace(evidence=scratch, github_output=False))
            boundary.assert_called_once_with(upload=True)
        self.assertEqual((seen["token"], seen["fresh"]), (INTENT, False))
        self.assertNotIn("CARGO_REGISTRY_TOKEN", seen["environment"])
        self.assertNotIn("GH_INTENT_TOKEN", seen["environment"])
        for changes, message in (({"TILEWRIGHT_CRATES_PUBLISH": "false"}, "off"), ({"OPERATION": "verify"}, "operation"),
                                 ({"CARGO_REGISTRY_TOKEN": ""}, "crates.io token")):
            with tempfile.TemporaryDirectory() as scratch, \
                    mock.patch.dict(os.environ, dict(environment, TILEWRIGHT_PUBLISH_RECEIPTS=scratch, **changes),
                                    clear=True), mock.patch.object(P, "boundary"), \
                    mock.patch.object(P, "verify_release") as verify_release:
                with self.subTest(changes=changes), self.assertRaisesRegex(P.Refusal, message):
                    P.upload_command(argparse.Namespace(evidence=scratch, github_output=False))
                verify_release.assert_not_called()


class TransportTests(unittest.TestCase):
    def opener(self, *, status=200, body=b"{}", error=None, url=None):
        response = mock.MagicMock()
        response.__enter__.return_value = response
        response.status = status
        response.read.side_effect = lambda limit: body[:limit]
        response.geturl.return_value = url
        opener = mock.Mock()
        opener.open.side_effect = error or (lambda request, timeout: response)
        return opener

    def test_tokens_go_only_to_the_github_api_without_redirects(self):
        with mock.patch.object(P.urllib.request, "build_opener", return_value=self.opener()) as build:
            P.request("https://api.github.com/repos/x", token=READ)
            request = build.return_value.open.call_args.args[0]
            self.assertEqual(request.get_header("Authorization"), f"Bearer {READ}")
            self.assertEqual(build.call_args.args[1].hosts, set())
            P.request("https://github.com/x/releases/download/v1.0.0/a")
            self.assertEqual(build.call_args.args[1].hosts, P.DOWNLOAD_HOSTS)
            self.assertIsNone(build.return_value.open.call_args.args[0].get_header("Authorization"))
        for url, token in (("https://crates.io/api/v1/crates/x", READ), ("https://api.github.com/repos/x", None),
                           ("https://evil.example/x", None), ("http://crates.io/api/v1/crates/x", None)):
            with self.subTest(url=url), self.assertRaises(P.Refusal):
                P.request(url, token=token)
        handler = P.Redirects({"release-assets.githubusercontent.com"})
        with self.assertRaisesRegex(P.Refusal, "redirect"):
            handler.redirect_request(mock.Mock(), None, 302, "", {}, "https://evil.example/x")

    def test_bounds_absence_and_failures(self):
        with mock.patch.object(P.urllib.request, "build_opener", return_value=self.opener(body=b"x" * 9)):
            with self.assertRaisesRegex(P.Refusal, "size limit"):
                P.request("https://crates.io/x", maximum=8)
        missing = P.urllib.error.HTTPError("u", 404, "m", {}, None)
        with mock.patch.object(P.urllib.request, "build_opener", return_value=self.opener(error=missing)):
            self.assertIsNone(P.request("https://crates.io/x", absent=True))
            with self.assertRaises(P.Failure) as caught:
                P.request("https://crates.io/x")
            self.assertEqual(caught.exception.status, 404)

    def test_github_reads_retry_only_transient_failures(self):
        for status, readback, calls in ((503, False, 3), (404, False, 1), (404, True, 3), (403, False, 1)):
            with self.subTest(status=status, readback=readback), mock.patch.object(P.time, "sleep"), \
                    mock.patch.object(P, "request", side_effect=P.Failure("GitHub", status)) as request:
                with self.assertRaises(P.Failure):
                    P.github("git/ref/tags/x", INTENT, readback=readback)
                self.assertEqual(request.call_count, calls)

    def test_put_once_frames_exact_bytes_and_classifies_outcomes(self):
        metadata = {"name": "tilewright-jsonc", "vers": LATER, "deps": []}
        endpoint = "https://crates.io/api/v1/crates/new"
        with mock.patch.object(P.urllib.request, "build_opener",
                               return_value=self.opener(body=b'{"warnings":{}}', url=endpoint)) as build:
            self.assertEqual(P.put_once(b"EXACT", metadata, CRATES), P.PutOutcome(200, "acknowledged", "acknowledged"))
            request = build.return_value.open.call_args.args[0]
            self.assertEqual((request.get_method(), request.full_url), ("PUT", endpoint))
            self.assertEqual(request.get_header("Authorization"), CRATES)
            body = request.data
            length = int.from_bytes(body[:4], "little")
            self.assertEqual(json.loads(body[4:4 + length]), metadata)
            self.assertEqual(int.from_bytes(body[4 + length:8 + length], "little"), 5)
            self.assertEqual(body[8 + length:], b"EXACT")
            self.assertIsInstance(build.call_args.args[1], P.NoRedirect)
        cases = [(self.opener(body=b'{"errors":[{"detail":"x"}]}', url=endpoint), ("uncertain", 200)),
                 (self.opener(body=b"not json", url=endpoint), ("uncertain", None)),
                 (self.opener(url="https://elsewhere.example/"), ("uncertain", 200)),
                 (self.opener(error=TimeoutError()), ("uncertain", None))]
        for status, category in ((401, "refused"), (403, "refused"), (429, "rate_limited"), (302, "uncertain"),
                                 (400, "uncertain"), (500, "uncertain"), (503, "uncertain")):
            cases.append((self.opener(error=P.urllib.error.HTTPError(endpoint, status, "m", {}, None)),
                          (category, status)))
        for opener, (category, status) in cases:
            with self.subTest(category=category, status=status), \
                    mock.patch.object(P.urllib.request, "build_opener", return_value=opener):
                outcome = P.put_once(b"EXACT", metadata, CRATES)
                self.assertEqual(outcome.category, category)
                self.assertEqual(outcome.definite, category in ("refused", "rate_limited"))
                opener.open.assert_called_once()
                self.assertNotIn(CRATES, repr(outcome))


class EvidenceTests(unittest.TestCase):
    def test_a_good_release_verifies_and_keeps_its_evidence(self):
        context = Context(self)
        plan = context.verify()
        self.assertTrue(plan["owner"])
        self.assertEqual(plan["producer"], {"run_id": 123, "attempt": 1})
        self.assertEqual([row["name"] for row in plan["order"]], ["tilewright-jsonc", "tilewright-source",
                                                                 "tilewright-pack", "tilewright-edit", "tilewright-diff"])
        self.assertEqual(plan["pending"], [row["name"] for row in plan["order"]])
        self.assertEqual(set(plan["actions"].values()), {"attempt 1"})
        evidence = context.scratch / "evidence-1"
        self.assertEqual((evidence / "bundle").read_bytes(), context.world.bundle)
        self.assertEqual(sorted(path.name for path in evidence.iterdir()),
                         ["SHA256SUMS", "attestations.jsonl", "bundle", "release.json"])
        with self.assertRaises(FileExistsError):
            P.verify_release(context.world.values(), READ, evidence, fresh=True, root=context.world.root)

    def test_release_state_tags_and_main_are_bound(self):
        changes = [lambda w: w.release.update(immutable=False), lambda w: w.release.update(draft=True),
                   lambda w: w.release.update(prerelease=True),
                   lambda w: w.release["assets"].append(dict(w.release["assets"][0], id=9)),
                   lambda w: w.release["assets"][1].update(state="starter"),
                   lambda w: w.release["assets"][0].update(digest="sha256:" + "0" * 64)]
        for change in changes:
            context = Context(self)
            change(context.world)
            with self.subTest(change=changes.index(change)), self.assertRaises(P.Refusal):
                context.verify()
        for suffix, answer in (("commits/go%2Fv0.7.0", {"sha": "f" * 40}),
                               ("compare/{}...main", {"status": "diverged"}), ("branches/main", {"protected": False}),
                               ("", {"id": 1})):
            context = Context(self)
            world = context.world
            real = world.github

            def github(asked, token, answer=answer, suffix=suffix.format(world.revision), real=real, **flags):
                return answer if asked == suffix else real(asked, token, **flags)
            context.stack.enter_context(mock.patch.object(P, "github", side_effect=github))
            with self.subTest(suffix=suffix), self.assertRaises(P.Refusal):
                context.verify()

    def test_a_moved_local_tag_or_wrong_input_digest_is_refused(self):
        context = Context(self)
        context.world.repository.git("tag", "-f", "go/v0.7.0", "v0.1.0")
        with self.assertRaisesRegex(P.Refusal, "local go/v0.7.0"):
            context.verify()
        context = Context(self)
        values = dict(context.world.values(), BUNDLE_SHA256="0" * 64)
        with self.assertRaisesRegex(P.Refusal, "dispatched"):
            P.verify_release(values, READ, context.scratch / "x", fresh=True, root=context.world.root)

    def test_sha256sums_must_list_the_bundle_once(self):
        world = World(self)
        P.check_sums(world.sums, world.name, world.digest)
        for sums in (b"", world.sums + world.sums, world.sums.replace(world.digest.encode(), b"0" * 64),
                     b"bad line\n" + world.sums, "é".encode() + world.sums):
            with self.subTest(sums=sums[:20]), self.assertRaises(P.Refusal):
                P.check_sums(sums, world.name, world.digest)

    def test_the_downloaded_bundle_must_match_githubs_digest(self):
        context = Context(self)
        world = context.world
        for url in world.downloads:
            world.downloads[url] = world.downloads[url] + b"x"
        with self.assertRaisesRegex(P.Refusal, "differs from GitHub's digest"):
            context.verify()

    def test_certificate_fields_run_and_subject_are_bound(self):
        revision, digest = "a" * 40, "b" * 64
        self.assertEqual(P.producer(attested(revision, digest), revision, digest), {"run_id": 123, "attempt": 1})
        for key in attested(revision, digest)[0]["verificationResult"]["signature"]["certificate"]:
            for value in ("wrong", None):
                with self.subTest(key=key, value=value), self.assertRaises(P.Refusal):
                    P.producer(attested(revision, digest, **{key: value}), revision, digest)
        for uri in (f"{P.REPO_URL}/actions/runs/123", f"{P.REPO_URL}/actions/runs/0/attempts/1",
                    "https://github.com/other/repo/actions/runs/1/attempts/1"):
            with self.subTest(uri=uri), self.assertRaises(P.Refusal):
                P.producer(attested(revision, digest, runInvocationURI=uri), revision, digest)
        with self.assertRaises(P.Refusal):
            P.producer(attested(revision, "c" * 64), revision, digest)
        two = attested(revision, digest) + attested(revision, digest, runInvocationURI=f"{P.REPO_URL}/actions/runs/124/attempts/1")
        with self.assertRaisesRegex(P.Refusal, "more than one"):
            P.producer(two, revision, digest)
        retried = attested(revision, digest) + attested(revision, digest, runInvocationURI=f"{P.REPO_URL}/actions/runs/123/attempts/2")
        self.assertEqual(P.producer(retried, revision, digest), {"run_id": 123, "attempt": 1})

    def test_attestation_lookup_is_bounded_and_needs_bundles(self):
        sleep = mock.Mock()
        with mock.patch.object(P, "github", return_value=None) as github:
            with self.assertRaisesRegex(P.Refusal, "not readable"):
                P.attestations("b" * 64, READ, sleep)
        self.assertEqual(github.call_count, P.ATTESTATION_TRIES)
        self.assertEqual(sleep.call_count, P.ATTESTATION_TRIES - 1)
        for item in ({"repository_id": 1, "bundle": {}}, {"repository_id": P.REPOSITORY_ID, "bundle_url": "x"}):
            with self.subTest(item=item), mock.patch.object(P, "github", return_value={"attestations": [item]}):
                with self.assertRaises(P.Refusal):
                    P.attestations("b" * 64, READ, sleep)

    def test_the_cli_verifies_offline_with_no_token_and_options_first(self):
        world = World(self)
        calls = []

        def gh(arguments, directory, token=None):
            calls.append((arguments, token))
            return world.gh(arguments, directory, token)
        with mock.patch.object(P, "gh", side_effect=gh):
            P.provenance(world.bundle, world.name, b"{}\n", world.revision, world.digest)
        arguments, token = calls[-1]
        self.assertIsNone(token)
        self.assertEqual(arguments[arguments.index("--cert-identity") + 1], P.SIGNER)
        self.assertIn("--deny-self-hosted-runners", arguments)
        with mock.patch.object(P, "gh", return_value=completed(stdout=b"gh version 2.40.0\n")):
            with self.assertRaisesRegex(P.Refusal, "2.68"):
                P.provenance(world.bundle, world.name, b"{}\n", world.revision, world.digest)
        with mock.patch.object(P.subprocess, "run", return_value=completed()) as run, \
                mock.patch.dict(os.environ, {"GH_READ_TOKEN": READ, "GH_INTENT_TOKEN": INTENT}), \
                tempfile.TemporaryDirectory() as scratch:
            P.gh(["version"], Path(scratch))
            environment = run.call_args.kwargs["env"]
            self.assertNotIn(READ, environment.values())
            self.assertNotIn("GH_TOKEN", environment)
            P.gh(["version"], Path(scratch), READ)
            self.assertEqual(run.call_args.kwargs["env"]["GH_TOKEN"], READ)

    def test_release_attestation_retries_then_refuses(self):
        sleep = mock.Mock()
        with mock.patch.object(P, "gh", return_value=completed(1)) as gh:
            with self.assertRaisesRegex(P.Refusal, "release attestation"):
                P.release_attestation("v0.7.0", "bundle", b"x", READ, sleep)
        self.assertEqual(gh.call_count, P.ATTESTATION_TRIES)
        self.assertEqual(gh.call_args.args[2], READ)


class TreePolicyTests(unittest.TestCase):
    def test_a_release_at_another_crate_version_verifies_from_its_tree(self):
        world = World(self)
        self.assertNotEqual(world.version, CURRENT)
        manifest, files, archives, version = P.candidate_bundle(world.bundle, world.revision, world.root)
        self.assertEqual((version, len(archives)), (LATER, 5))
        with self.assertRaises((ValueError, KeyError)):
            P.CANDIDATES.verify(world.bundle, world.revision)

    def test_a_license_differing_from_the_trees_is_refused(self):
        world = World(self)
        (world.root / "LICENSE").write_text("another license\n")
        revision = world.repository.commit()
        with self.assertRaisesRegex(ValueError, "license"):
            P.candidate_bundle(world.bundle, revision, world.root)

    def test_the_tree_holds_only_policy_files(self):
        world = World(self)
        with tempfile.TemporaryDirectory() as scratch:
            P.tree(world.revision, Path(scratch), world.root)
            found = sorted(str(path.relative_to(scratch)) for path in Path(scratch).rglob("*") if path.is_file())
        self.assertEqual(found, sorted(["Cargo.toml", "rust-toolchain.toml", "LICENSE", "tools/crates/candidates.json",
                                        *(f"crates/{name}/Cargo.toml" for name in PUBLIC + P.PREPARE.PRIVATE)]))

    def test_abandoned_versions_and_releases_are_refused(self):
        for entry in ({"version": LATER, "release": "v0.9.0"}, {"version": "9.9.9", "release": "v0.7.0"}):
            context = Context(self)
            config = context.world.root / "tools/crates/candidates.json"
            value = json.loads(config.read_text())
            value["abandoned"] = [entry]
            config.write_text(json.dumps(value))
            with self.subTest(entry=entry), self.assertRaisesRegex(P.Refusal, "abandoned"):
                context.verify()

    def test_mains_current_abandoned_list_counts_too(self):
        # A re-run of an older publish run checks out that run's main; the
        # list on main's tip now must refuse as well.
        context = Context(self)
        value = json.loads((context.world.root / "tools/crates/candidates.json").read_text())
        value["abandoned"] = [{"version": LATER, "release": "v0.7.0"}]
        context.world.main_candidates = json.dumps(value).encode()
        with self.assertRaisesRegex(P.Refusal, "abandoned"):
            context.verify()
        for answer in ({"type": "dir"}, {"type": "file", "encoding": "base64", "size": 3, "content": "e30="},
                       {"type": "file", "encoding": "base64", "size": 2, "content": "W10="}):
            with self.subTest(answer=answer), mock.patch.object(P, "github", return_value=answer), \
                    self.assertRaises(ValueError):
                P.main_abandoned(READ)

    def test_a_release_that_does_not_own_its_version_plans_nothing(self):
        context = Context(self, owner=False)
        plan = context.verify("verify")
        self.assertEqual((plan["owner"], plan["owner_tag"], plan["pending"]), (False, "v0.1.0", []))
        self.assertIn("owned by v0.1.0", "\n".join(P.plan_summary(plan, "x")))


class RegistryTests(unittest.TestCase):
    def setUp(self):
        self.world = World(self)
        self.row = self.world.rows["tilewright-jsonc"]
        patcher = mock.patch.object(P, "request", side_effect=self.world.request)
        patcher.start()
        self.addCleanup(patcher.stop)

    def test_tri_state(self):
        self.assertEqual(P.registry_state(self.row).state, "absent")
        for flag in ("api", "index", "cdn"):
            self.world.publish("tilewright-jsonc", **{flag: False})
            with self.subTest(flag=flag):
                self.assertEqual(P.registry_state(self.row).state, "indeterminate")
        self.world.publish("tilewright-jsonc")
        state = P.registry_state(self.row)
        self.assertEqual((state.state, state.version["trustpub_data"]["run_id"]), ("present", RUN_ID))

    def test_conflicts_yanks_cdn_bytes_and_owners_stop(self):
        for flags in ({"checksum": "0" * 64}, {"yanked": True}, {"data": b"other bytes"}):
            self.world.publish("tilewright-jsonc", **flags)
            with self.subTest(flags=flags), self.assertRaises(P.Refusal):
                P.registry_state(self.row)
        self.world.publish("tilewright-jsonc")
        self.world.owners["tilewright-jsonc"] = [dict(ZOEY, id=3865595)]
        with self.assertRaisesRegex(P.Refusal, "owner"):
            P.registry_state(self.row)

    def test_newer_versions_make_a_cohort_stale(self):
        self.world.publish("tilewright-jsonc", version=CURRENT, by_hand=True, data=b"older")
        registry = {name: P.registry_state(self.world.rows[name]) for name in PUBLIC}
        self.assertEqual(P.newer(registry, LATER), [])
        later = F.next_minor(LATER)
        self.world.publish("tilewright-edit", version=later, yanked=True, data=b"later")
        registry = {name: P.registry_state(self.world.rows[name]) for name in PUBLIC}
        self.assertEqual(P.newer(registry, LATER), [later])
        self.world.registry["tilewright-edit"]["9.0.0-alpha"] = dict(self.world.registry["tilewright-edit"][later])
        registry = {name: P.registry_state(self.world.rows[name]) for name in PUBLIC}
        with self.assertRaises(P.Refusal):
            P.newer(registry, LATER)


class RecordTests(unittest.TestCase):
    def setUp(self):
        self.world = World(self)
        self.row = self.world.rows["tilewright-pack"]
        patcher = mock.patch.object(P, "github", side_effect=self.world.github)
        patcher.start()
        self.addCleanup(patcher.stop)

    def found(self):
        return P.records(self.row, self.world.values(), INTENT)

    def test_attempts_follow_refusals_up_to_three(self):
        self.assertEqual(P.next_attempt(self.found(), self.row), 1)
        self.world.record(P.ATTEMPT, "tilewright-pack", 1)
        with self.assertRaisesRegex(P.Refusal, "attempt 1 .* unresolved"):
            P.next_attempt(self.found(), self.row)
        self.world.record(P.REFUSED, "tilewright-pack", 1)
        self.assertEqual(P.next_attempt(self.found(), self.row), 2)
        self.world.record(P.ATTEMPT, "tilewright-pack", 2)
        self.world.record(P.REFUSED, "tilewright-pack", 2)
        self.world.record(P.ATTEMPT, "tilewright-pack", 3)
        self.world.record(P.REFUSED, "tilewright-pack", 3)
        with self.assertRaisesRegex(P.Refusal, "all 3 attempts"):
            P.next_attempt(self.found(), self.row)

    def test_inconsistent_records_are_refused(self):
        for records in ([(P.ATTEMPT, 2)], [(P.REFUSED, 1)], [(P.ATTEMPT, 1), (P.ATTEMPT, 2), (P.REFUSED, 2)]):
            world = World(self)
            row = world.rows["tilewright-pack"]
            for space, number in records:
                world.record(space, "tilewright-pack", number)
            with self.subTest(records=records), mock.patch.object(P, "github", side_effect=world.github), \
                    self.assertRaisesRegex(P.Refusal, "inconsistent"):
                P.next_attempt(P.records(row, world.values(), INTENT), row)

    def test_records_must_name_this_release(self):
        other = {"tag": "v0.6.0", "revision": self.world.revision, "bundle_sha256": self.world.digest}
        for kwargs in ({"release": other}, {"attempt": 2}, {"kind": "world-first-crate-upload"},
                       {"sha256": "0" * 64}):
            world = World(self)
            row = world.rows["tilewright-pack"]
            world.record(P.ATTEMPT, "tilewright-pack", 1, **kwargs)
            with self.subTest(kwargs=kwargs), mock.patch.object(P, "github", side_effect=world.github), \
                    self.assertRaisesRegex(P.Refusal, "names another release"):
                P.records(row, world.values(), INTENT)

    def test_a_first_publication_intent_blocks_an_absent_crate(self):
        self.world.record(P.LEGACY, "tilewright-pack")
        found = self.found()
        self.assertTrue(found["legacy"])
        with self.assertRaisesRegex(P.Refusal, "first-publication intent"):
            P.next_attempt(found, self.row)

    def test_unexpected_refs_under_a_namespace_are_refused(self):
        base = f"refs/tags/{P.ATTEMPT}/tilewright-pack/{self.world.version}"
        for ref in (base, base + "/4", base + "/retry-1"):
            self.world.refs = {ref: {"type": "tag", "sha": "a" * 40}}
            with self.subTest(ref=ref), self.assertRaises(P.Refusal):
                self.found()
        self.world.refs = {base + "1/1": {"type": "tag", "sha": "a" * 40},
                           f"refs/tags/{P.ATTEMPT}/tilewright-pack-wasm/{self.world.version}/1": {"type": "tag", "sha": "a" * 40}}
        self.assertEqual(self.found(), {"legacy": False, "attempts": {}, "refused": {}})

    def test_create_record_is_create_only_and_reads_back(self):
        self.world.refs = {}
        with mock.patch.object(P, "github_write", side_effect=self.world.github_write):
            sha = P.create_record("crate-upload-attempt/tilewright-pack/9.9.9/1", {"x": 1}, self.world.values(), INTENT)
            self.assertEqual(self.world.refs["refs/tags/crate-upload-attempt/tilewright-pack/9.9.9/1"]["sha"], sha)
            with self.assertRaisesRegex(P.Refusal, "already exists"), mock.patch("sys.stderr", new_callable=io.StringIO):
                P.create_record("crate-upload-attempt/tilewright-pack/9.9.9/1", {"x": 1}, self.world.values(), INTENT)
        for answer in ({"ref": "refs/tags/other", "object": {"type": "tag", "sha": "a" * 40}}, {}):
            with self.subTest(answer=answer), mock.patch.object(P, "github_write", side_effect=[{"sha": "a" * 40}, answer]), \
                    mock.patch("sys.stderr", new_callable=io.StringIO) as stderr, self.assertRaises(P.Refusal):
                P.create_record("crate-upload-attempt/tilewright-pack/9.9.9/2", {"x": 1}, self.world.values(), INTENT)
            self.assertIn("reference", stderr.getvalue())

        def lost(suffix, body, token):
            self.world.github_write(suffix, body, token)
            if suffix == "git/refs":
                self.world.refs[body["ref"]] = {"type": "tag", "sha": "f" * 40}
            return {"sha": "a" * 40} if suffix == "git/tags" else {"ref": body["ref"], "object": {"type": "tag", "sha": "a" * 40}}
        with mock.patch.object(P, "github_write", side_effect=lost), \
                mock.patch("sys.stderr", new_callable=io.StringIO) as stderr, self.assertRaisesRegex(P.Refusal, "read back"):
            P.create_record("crate-upload-attempt/tilewright-pack/9.9.9/3", {"x": 1}, self.world.values(), INTENT)
        self.assertIn("read-back", stderr.getvalue())


class UploadTests(unittest.TestCase):
    def receipts(self, context):
        directory = context.scratch / "receipts"
        return sorted(path.name for path in directory.iterdir()) if directory.exists() else []

    def test_the_cohort_uploads_once_each_in_order_behind_its_records(self):
        context = Context(self)
        world = context.world
        context.upload()
        order = ["tilewright-jsonc", "tilewright-source", "tilewright-pack", "tilewright-edit", "tilewright-diff"]
        self.assertEqual(world.puts, order)
        version = world.version
        self.assertEqual([event for event in world.events if event[0] == "put" or event[1] == "git/refs"],
                         [event for name in order for event in
                          (("write", "git/refs", f"refs/tags/{P.ATTEMPT}/{name}/{version}/1"), ("put", name))])
        self.assertEqual(self.receipts(context), sorted(f"{name}-{version}-1-{stage}.json"
                                                        for name in order for stage in ("put", "reserved")))
        tag = world.tags[world.refs[f"refs/tags/{P.ATTEMPT}/tilewright-jsonc/{version}/1"]["sha"]]
        message = json.loads(tag["message"])
        self.assertEqual((message["kind"], message["attempt"], message["release"]["tag"], message["release"]["run_id"],
                          message["coordinate"]["sha256"], message["publisher"]["run_id"]),
                         (P.ATTEMPT_KIND, 1, world.tag, 123, world.rows["tilewright-jsonc"]["sha256"], int(RUN_ID)))
        self.assertEqual(tag["object"], {"sha": world.revision, "type": "commit"})
        # A second dispatch finds all five present and writes nothing.
        writes = len(world.writes)
        plan = context.upload()
        self.assertEqual((world.puts, len(world.writes), plan["pending"]), (order, writes, []))

    def test_the_reserved_receipt_exists_before_the_put(self):
        context = Context(self)
        world = context.world
        real = world.put

        def put(data, metadata, token):
            self.assertIn(f"{metadata['name']}-{world.version}-1-reserved.json", self.receipts(context))
            self.assertNotIn(f"{metadata['name']}-{world.version}-1-put.json", self.receipts(context))
            return real(data, metadata, token)
        context.stack.enter_context(mock.patch.object(P, "put_once", side_effect=put))
        context.upload()

    def test_a_refusal_is_recorded_and_the_next_dispatch_makes_attempt_two(self):
        for status in (401, 403, 429):
            context = Context(self)
            world = context.world
            world.outcomes["tilewright-pack"] = [status]
            with self.subTest(status=status), self.assertRaisesRegex(P.Refusal, f"refused attempt 1 .* HTTP {status}"):
                context.upload()
            self.assertEqual(world.puts, ["tilewright-jsonc", "tilewright-source", "tilewright-pack"])
            refused = f"refs/tags/{P.REFUSED}/tilewright-pack/{world.version}/1"
            message = json.loads(world.tags[world.refs[refused]["sha"]]["message"])
            self.assertEqual((message["reason"], message["attempt"], message["attempt_ref"]),
                             (f"http-{status}", 1, f"refs/tags/{P.ATTEMPT}/tilewright-pack/{world.version}/1"))
            # Recorded right after the PUT, before any confirm poll.
            put = world.events.index(("put", "tilewright-pack"))
            self.assertEqual(world.events[put + 1:put + 3], [("write", "git/tags", refused[len("refs/tags/"):]),
                                                             ("write", "git/refs", refused)])
            context.upload()
            self.assertEqual(world.puts[3:], ["tilewright-pack", "tilewright-edit", "tilewright-diff"])
            self.assertIn(f"refs/tags/{P.ATTEMPT}/tilewright-pack/{world.version}/2", world.refs)

    def test_an_uncertain_put_that_lands_from_this_run_continues(self):
        context = Context(self)
        context.world.outcomes["tilewright-source"] = ["timeout-published"]
        context.upload()
        self.assertEqual(len(context.world.puts), 5)

    def test_an_uncertain_put_that_stays_absent_stops_without_a_refusal_record(self):
        for outcome in ("timeout", 500):
            context = Context(self)
            world = context.world
            world.outcomes["tilewright-source"] = [outcome]
            with self.subTest(outcome=outcome), self.assertRaisesRegex(P.Refusal, "unresolved"):
                context.upload()
            self.assertEqual(world.puts, ["tilewright-jsonc", "tilewright-source"])
            self.assertFalse(any(ref.startswith(f"refs/tags/{P.REFUSED}/") for ref in world.refs))
            with self.assertRaisesRegex(P.Refusal, "attempt 1 for tilewright-source .* unresolved"):
                context.verify()

    def test_another_publisher_of_the_version_stops_the_run(self):
        context = Context(self)
        context.world.outcomes["tilewright-jsonc"] = ["other-run"]
        with self.assertRaisesRegex(P.Refusal, "not from this run"):
            context.upload()
        self.assertEqual(context.world.puts, ["tilewright-jsonc"])

    def test_present_crates_are_skipped_and_the_canary_writes_nothing(self):
        context = Context(self)
        world = context.world
        for name in PUBLIC:
            world.publish(name, by_hand=True)
            world.record(P.LEGACY, name)
        context.upload()
        self.assertEqual((world.puts, world.writes), ([], []))
        context = Context(self)
        context.world.publish("tilewright-jsonc", by_hand=True)
        with mock.patch("sys.stdout", new_callable=io.StringIO):
            context.upload()
        self.assertEqual(context.world.puts, ["tilewright-source", "tilewright-pack", "tilewright-edit",
                                              "tilewright-diff"])

    def test_indeterminate_and_stale_cohorts_write_nothing(self):
        context = Context(self)
        context.world.publish("tilewright-pack", index=False)
        with self.assertRaisesRegex(P.Refusal, "only one of its API"):
            context.upload()
        context = Context(self)
        context.world.publish("tilewright-diff", version=F.next_minor(LATER), data=b"newer")
        with self.assertRaisesRegex(P.Refusal, "stale"):
            context.upload()
        self.assertEqual((context.world.puts, context.world.writes), ([], []))

    def test_a_missing_dependency_or_changed_release_stops_before_any_record(self):
        context = Context(self)
        world = context.world
        values = world.values()
        plan = context.verify()
        plan["order"] = [row for row in plan["order"] if row["name"] == "tilewright-source"]
        with self.assertRaisesRegex(P.Refusal, "waits for it"):
            P.upload(plan, values, CRATES, INTENT, P.Receipts())
        plan = context.verify()
        world.release["assets"][0]["id"] = 99
        with self.assertRaisesRegex(P.Refusal, "release changed"):
            P.upload(plan, values, CRATES, INTENT, P.Receipts())
        with self.assertRaisesRegex(P.Refusal, "changed since it was verified"):
            P.verify_release(values, INTENT, context.scratch / f"evidence-{context.count}", fresh=False,
                             root=world.root)
        self.assertEqual((world.puts, world.writes), ([], []))

    def not_sent(self, world, name, number=1):
        ref = f"refs/tags/{P.REFUSED}/{name}/{world.version}/{number}"
        self.assertIn(ref, world.refs)
        return json.loads(world.tags[world.refs[ref]["sha"]]["message"])["reason"]

    def test_a_read_failure_after_a_refusal_still_leaves_the_refused_record(self):
        # The review's repro: a 403, then crates.io reads fail during the polls.
        context = Context(self)
        world = context.world
        world.outcomes["tilewright-pack"] = [403]
        real_request, real_put = world.request, world.put
        armed = []

        def flaky(url, **kwargs):
            if armed == [True] and "tilewright-pack" in url:
                world.events.append(("read", url))
                raise P.Failure("crates.io", 503)
            return real_request(url, **kwargs)

        def put(data, metadata, token):
            outcome = real_put(data, metadata, token)
            if metadata["name"] == "tilewright-pack" and not armed:
                armed.append(True)  # every pack read fails after the first PUT
            return outcome
        context.stack.enter_context(mock.patch.object(P, "request", side_effect=flaky))
        context.stack.enter_context(mock.patch.object(P, "put_once", side_effect=put))
        with self.assertRaisesRegex(P.Refusal, "refused attempt 1 for tilewright-pack .* HTTP 403"):
            context.upload()
        self.assertEqual(self.not_sent(world, "tilewright-pack"), "http-403")
        first_read = next(index for index, event in enumerate(world.events) if event[0] == "read")
        self.assertLess(world.events.index(("write", "git/refs",
                                            f"refs/tags/{P.REFUSED}/tilewright-pack/{world.version}/1")), first_read)
        armed.append(False)  # disarmed for good
        context.upload()
        self.assertEqual(world.puts[3:], ["tilewright-pack", "tilewright-edit", "tilewright-diff"])

    def test_the_refused_record_is_retried_and_accepts_only_its_own_copy(self):
        context = Context(self)
        world = context.world
        world.outcomes["tilewright-pack"] = [429]
        real = world.github_write
        lost = []

        def write(suffix, body, token):
            answer = real(suffix, body, token)
            if suffix == "git/refs" and body["ref"].startswith(f"refs/tags/{P.REFUSED}/") and not lost:
                lost.append(body["ref"])
                raise P.Failure("GitHub", 502)  # created, but the answer was lost
            return answer
        context.stack.enter_context(mock.patch.object(P, "github_write", side_effect=write))
        with self.assertRaisesRegex(P.Refusal, "refused attempt 1"):
            context.upload()
        self.assertEqual(self.not_sent(world, "tilewright-pack"), "http-429")
        # Three attempt records and the refused record, created once.
        self.assertEqual(sum(1 for event in world.events if event[:2] == ("write", "git/refs")), 4)
        plan = context.verify()
        row = world.rows["tilewright-pack"]
        attempt = world.refs[f"refs/tags/{P.ATTEMPT}/tilewright-pack/{world.version}/1"]["sha"]
        with self.assertRaisesRegex(P.Refusal, "not as this run's record"):
            P.record_refusal(plan, row, 1, attempt, "not-sent", world.values(), INTENT)
        sleep = mock.Mock()
        context.stack.enter_context(mock.patch.object(P, "github_write", side_effect=P.Failure("GitHub", 503)))
        with self.assertRaises(P.Failure):
            P.record_refusal(plan, row, 2, attempt, "http-403", world.values(), INTENT, sleep)
        self.assertEqual(sleep.call_count, P.RECORD_TRIES - 1)

    def test_a_crate_that_appears_before_the_put_gets_a_not_sent_record(self):
        context = Context(self)
        world = context.world
        real = world.github_write

        def write(suffix, body, token):
            answer = real(suffix, body, token)
            if suffix == "git/refs" and body["ref"] == f"refs/tags/{P.ATTEMPT}/tilewright-jsonc/{world.version}/1":
                world.publish("tilewright-jsonc", by_hand=True)  # someone else published it meanwhile
            return answer
        context.stack.enter_context(mock.patch.object(P, "github_write", side_effect=write))
        with self.assertRaisesRegex(P.Refusal, "changed on crates.io after attempt 1"):
            context.upload()
        self.assertEqual((world.puts, self.not_sent(world, "tilewright-jsonc")), ([], "not-sent"))
        context.upload()
        self.assertEqual(world.puts, ["tilewright-source", "tilewright-pack", "tilewright-edit", "tilewright-diff"])

    def test_a_failed_read_before_the_put_gets_a_not_sent_record(self):
        context = Context(self)
        world = context.world
        real = world.request
        armed = []

        def flaky(url, **kwargs):
            if armed and "tilewright-jsonc" in url:
                armed.clear()
                raise P.Failure("crates.io", 503)
            return real(url, **kwargs)
        real_write = world.github_write

        def write(suffix, body, token):
            answer = real_write(suffix, body, token)
            if suffix == "git/refs" and body["ref"] == f"refs/tags/{P.ATTEMPT}/tilewright-jsonc/{world.version}/1":
                armed.append(True)
            return answer
        context.stack.enter_context(mock.patch.object(P, "request", side_effect=flaky))
        context.stack.enter_context(mock.patch.object(P, "github_write", side_effect=write))
        with self.assertRaisesRegex(P.Failure, "503"):
            context.upload()
        self.assertEqual((world.puts, self.not_sent(world, "tilewright-jsonc")), ([], "not-sent"))
        context.upload()
        self.assertIn(f"refs/tags/{P.ATTEMPT}/tilewright-jsonc/{world.version}/2", world.refs)
        self.assertEqual(len(world.puts), 5)

    def test_a_receipt_that_cannot_be_written_gets_a_not_sent_record(self):
        context = Context(self)
        real = Path.open

        def open_(path, *args, **kwargs):
            if path.name.endswith("-1-reserved.json"):
                raise OSError("disk full")
            return real(path, *args, **kwargs)
        with mock.patch.object(Path, "open", open_), \
                self.assertRaisesRegex(P.Refusal, "receipt .* could not be written"):
            context.upload()
        world = context.world
        self.assertEqual(world.puts, [])
        self.assertIn(f"refs/tags/{P.ATTEMPT}/tilewright-jsonc/{world.version}/1", world.refs)
        self.assertEqual(self.not_sent(world, "tilewright-jsonc"), "not-sent")
        context.upload()
        self.assertEqual(len(world.puts), 5)

    def test_an_unrecordable_not_sent_attempt_stays_unresolved(self):
        context = Context(self)
        world = context.world
        real_open, real_write = Path.open, world.github_write

        def open_(path, *args, **kwargs):
            if path.name.endswith("-reserved.json"):
                raise OSError("disk full")
            return real_open(path, *args, **kwargs)

        def write(suffix, body, token):
            if body.get("ref", body.get("tag", "")).replace("refs/tags/", "").startswith(P.REFUSED):
                raise P.Failure("GitHub", 503)
            return real_write(suffix, body, token)
        context.stack.enter_context(mock.patch.object(P, "github_write", side_effect=write))
        with mock.patch.object(Path, "open", open_), self.assertRaisesRegex(P.Refusal, "receipt"):
            context.upload()
        self.assertIn("could not be recorded either", context.stderr.getvalue())
        with self.assertRaisesRegex(P.Refusal, "attempt 1 for tilewright-jsonc .* unresolved"):
            context.verify()

    def test_confirm_needs_this_runs_trusted_publishing_and_treats_lag_as_not_yet(self):
        world = World(self)
        row = world.rows["tilewright-jsonc"]
        sleep = mock.Mock()
        with mock.patch.object(P, "request", side_effect=world.request):
            self.assertFalse(P.confirm_upload(row, RUN_ID, sleep))
            self.assertEqual(sleep.call_count, P.POLLS - 1)
            world.publish("tilewright-jsonc", cdn=False)  # the CDN answers 403 for now
            self.assertFalse(P.confirm_upload(row, RUN_ID, sleep))
            world.publish("tilewright-jsonc")
            self.assertTrue(P.confirm_upload(row, RUN_ID, sleep))
            for flags in ({"by_hand": True}, {"run_id": "1"}):
                world.publish("tilewright-jsonc", **flags)
                with self.subTest(flags=flags), self.assertRaises(P.Refusal):
                    P.confirm_upload(row, RUN_ID, sleep)
        # A read that fails is "not yet"; the next one may see it.
        world.publish("tilewright-jsonc")
        failures = iter([P.Failure("crates.io", 503), P.Failure("crates.io", None)])

        def flaky(url, **kwargs):
            failure = next(failures, None)
            if failure:
                raise failure
            return world.request(url, **kwargs)
        with mock.patch.object(P, "request", side_effect=flaky):
            self.assertTrue(P.confirm_upload(row, RUN_ID, mock.Mock()))
        # Outside the polls, a CDN that does not serve the file yet is
        # indeterminate, and a 403 there is no error.
        world.publish("tilewright-jsonc", cdn=False)
        with mock.patch.object(P, "request", side_effect=world.request):
            self.assertEqual(P.registry_state(row).state, "indeterminate")


class ConsumerTests(unittest.TestCase):
    def evidence(self, context):
        context.verify()
        return context.scratch / f"evidence-{context.count}"

    def run_command(self, command, **arguments):
        with mock.patch.object(P, "boundary"), mock.patch("sys.stdout", new_callable=io.StringIO):
            command(argparse.Namespace(**arguments))

    def test_consumer_with_and_without_the_release_archives(self):
        context = Context(self)
        evidence = self.evidence(context)
        output = context.scratch / "outputs"
        output.write_text("")
        values = context.world.values()
        real = P.offline_plan
        with mock.patch.dict(os.environ, dict(values, GITHUB_OUTPUT=str(output))), \
                mock.patch.object(P, "offline_plan", side_effect=lambda v, e: real(v, e, context.world.root)):
            self.run_command(P.consumer_command, evidence=evidence, output=context.scratch / "resolve",
                             patch=True, github_output=True)
            self.run_command(P.consumer_command, evidence=evidence, output=context.scratch / "accept",
                             patch=False, github_output=False)
        manifest = (context.scratch / "resolve/consumer/Cargo.toml").read_text()
        for name in PUBLIC:
            self.assertIn(f'{name} = "={LATER}"', manifest)
            self.assertTrue((context.scratch / f"resolve/archives/{name}-{LATER}/Cargo.toml").is_file())
        self.assertIn("[patch.crates-io]", manifest)
        self.assertIn("[workspace]", manifest)
        self.assertNotIn("[patch", (context.scratch / "accept/consumer/Cargo.toml").read_text())
        self.assertIn("toolchain=" + P.PREPARE.policy()[1], output.read_text())

    def test_accept_lock_requires_the_release_checksums_from_crates_io(self):
        context = Context(self)
        evidence = self.evidence(context)
        world = context.world
        lock = context.scratch / "Cargo.lock"

        def write(**changes):
            entries = []
            for name, row in world.rows.items():
                entry = {"name": name, "version": world.version, "source": P.CRATES_IO, "checksum": row["sha256"]}
                entry.update(changes.get(name, {}))
                if entry.get("drop"):
                    continue
                entries.append(entry)
            lock.write_text("version = 4\n" + "".join(
                "\n[[package]]\n" + "".join(f"{key} = {json.dumps(value)}\n" for key, value in entry.items())
                for entry in entries))

        real = P.offline_plan
        with mock.patch.dict(os.environ, world.values()), \
                mock.patch.object(P, "offline_plan", side_effect=lambda v, e: real(v, e, world.root)):
            write()
            self.run_command(P.accept_lock_command, evidence=evidence, lock=lock)
            for changes, message in (({"tilewright-pack": {"checksum": "0" * 64}}, "differs"),
                                     ({"tilewright-edit": {"drop": True}}, "once"),
                                     ({"tilewright-diff": {"source": "registry+https://example.invalid/index"}},
                                      "crates.io")):
                write(**changes)
                with self.subTest(changes=changes), self.assertRaisesRegex(P.Refusal, message):
                    self.run_command(P.accept_lock_command, evidence=evidence, lock=lock)



class CommandTests(unittest.TestCase):
    def test_verify_command_outputs_the_plan_and_drops_the_read_token(self):
        context = Context(self)
        world = context.world
        output, summary = context.scratch / "output", context.scratch / "summary"
        output.write_text("")
        world.publish("tilewright-jsonc", by_hand=True)
        real = P.verify_release
        seen = {}

        def verify(values, token, evidence, *, fresh):
            seen["environment"] = dict(os.environ)
            return real(values, token, evidence, fresh=fresh, root=world.root)
        with mock.patch.dict(os.environ, dict(world.values(), GH_READ_TOKEN=READ, GITHUB_OUTPUT=str(output),
                                              GITHUB_STEP_SUMMARY=str(summary))), \
                mock.patch.object(P, "boundary"), mock.patch.object(P, "verify_release", side_effect=verify), \
                mock.patch("sys.stdout", new_callable=io.StringIO) as stdout:
            self.assertEqual(P.main(["verify", "--evidence", str(context.scratch / "evidence"), "--github-output"]), 0)
        self.assertNotIn("GH_READ_TOKEN", seen["environment"])
        self.assertEqual(output.read_text(), f"owner=true\nowner_tag={world.tag}\ncrate_version={LATER}\npending=4\n")
        self.assertIn("| tilewright-jsonc " + LATER + " | present | none | present |", summary.read_text())
        self.assertIn("published outside this workflow", stdout.getvalue())

    def test_confirm_command_waits_then_reports_what_is_missing(self):
        context = Context(self)
        world = context.world
        context.verify()
        evidence = context.scratch / f"evidence-{context.count}"
        real = P.offline_plan
        for name in PUBLIC[:-1]:
            world.publish(name)
        with mock.patch.dict(os.environ, world.values()), mock.patch.object(P, "boundary"), \
                mock.patch.object(P, "offline_plan", side_effect=lambda v, e: real(v, e, world.root)), \
                mock.patch("sys.stdout", new_callable=io.StringIO) as stdout:
            self.assertEqual(P.main(["confirm", "--evidence", str(evidence)]), 1)
            self.assertIn(f"does not have {PUBLIC[-1]} at {LATER}", stdout.getvalue())
            world.publish(PUBLIC[-1])
            self.assertEqual(P.main(["confirm", "--evidence", str(evidence)]), 0)

    def test_errors_print_static_text_only(self):
        cases = [(P.Refusal("a static refusal"), "a static refusal"),
                 (ValueError("candidate archive lacks source metadata or notices"), "candidate archive lacks"),
                 (json.JSONDecodeError("Expecting value", "SERVER TEXT " + CRATES, 0), "malformed evidence"),
                 (KeyError(CRATES), "malformed or unavailable evidence (KeyError)"),
                 (P.FIRST.Refusal("unsupported upload dependency metadata"), "unsupported upload")]
        for error, expected in cases:
            with self.subTest(error=type(error).__name__), mock.patch.object(P, "confirm_command", side_effect=error), \
                    mock.patch("sys.stdout", new_callable=io.StringIO) as stdout:
                self.assertEqual(P.main(["confirm", "--evidence", "x"]), 1)
                self.assertIn(expected, stdout.getvalue())
                if not isinstance(error, (P.Refusal, P.FIRST.Refusal)) and type(error) is not ValueError:
                    self.assertNotIn(CRATES, stdout.getvalue())
                self.assertTrue(stdout.getvalue().startswith("::error title=Publish crates::confirm: "))

    def test_outputs_refuse_line_breaks(self):
        with tempfile.TemporaryDirectory() as scratch, \
                mock.patch.dict(os.environ, {"GITHUB_OUTPUT": str(Path(scratch) / "output")}):
            with self.assertRaisesRegex(P.Refusal, "line break"):
                P.outputs({"owner": "true\ninjected=1"})


ROOT = Path(__file__).resolve().parents[2]


def workflow_jobs(text):
    """The jobs of a workflow, each as its text, by a line-based split."""
    body = text.split("\njobs:\n", 1)[1]
    jobs = {}
    for block in re.split(r"\n(?=  [a-z][a-z-]*:\n)", "\n" + body):
        if block.strip():
            name = block.strip().split(":", 1)[0]
            jobs[name] = block
    return jobs


def steps(job):
    return re.split(r"\n(?=      - )", job)[1:]


def block(job, key):
    """The lines of a job-level mapping such as permissions, stripped."""
    lines = []
    for line in job.split(f"\n    {key}:\n", 1)[1].split("\n"):
        if not line.startswith("      "):
            break
        lines.append(line.strip())
    return lines


class PublishWorkflowTests(unittest.TestCase):
    TEXT = (ROOT / ".github/workflows/publish-crate.yml").read_text()
    JOBS = workflow_jobs(TEXT)
    GUARD = ("github.repository == 'seraphinastudios/tilewright-world' && github.repository_id == '1423814231' && "
             "github.ref == 'refs/heads/main' && github.event_name == 'workflow_dispatch'")

    def test_dispatch_only_with_four_inputs_read_through_env(self):
        top = self.TEXT.split("\njobs:\n", 1)[0]
        on = top.split("\non:\n", 1)[1].split("\npermissions:", 1)[0]
        self.assertEqual(re.findall(r"^  ([a-z_]+):", on, re.MULTILINE), ["workflow_dispatch"])
        self.assertEqual(re.findall(r"^      ([a-z0-9_]+):$", on, re.MULTILINE),
                         ["operation", "release_tag", "source_revision", "bundle_sha256"])
        self.assertIn("\npermissions: {}\n", top)
        for line in self.TEXT.splitlines():
            if "inputs." in line and not line.lstrip().startswith("#"):
                with self.subTest(line=line):
                    self.assertTrue(re.fullmatch(r"\s+(group|OPERATION|RELEASE_TAG|SOURCE_REVISION|BUNDLE_SHA256|if): .*",
                                                 line), line)
        for job in self.JOBS.values():
            for step in steps(job):
                if "run: " in step:
                    script = step.split("run: ", 1)[1]
                    self.assertNotIn("${{", script, step)

    def test_concurrency_per_release_and_operation_never_cancelled(self):
        self.assertIn("concurrency:\n  group: world-crate-publication-${{ inputs.release_tag }}-${{ inputs.operation }}\n"
                      "  cancel-in-progress: false\n", self.TEXT)

    def test_jobs_guards_checkouts_and_input_checks(self):
        self.assertEqual(list(self.JOBS), ["verify", "resolve", "publish", "accept"])
        for name, job in self.JOBS.items():
            with self.subTest(job=name):
                self.assertIn(self.GUARD, job.split("    if: ", 1)[1].split("\n", 1)[0])
                first = steps(job)[0]
                self.assertIn("name: Check the inputs", first)
                self.assertIn("^[0-9a-f]{40}$", first)
                self.assertIn("^[0-9a-f]{64}$", first)
                self.assertIn("ref: ${{ github.sha }}\n          fetch-depth: 0\n          persist-credentials: false", job)
                self.assertNotIn("cargo publish", job)

    def test_permissions_and_environment(self):
        expected = {"verify": {"contents": "read", "attestations": "read"},
                    "resolve": {"contents": "read", "attestations": "read"},
                    "publish": {"contents": "write", "attestations": "read", "id-token": "write"},
                    "accept": {"contents": "read", "attestations": "read"}}
        for name, job in self.JOBS.items():
            found = dict(line.split(": ") for line in block(job, "permissions"))
            with self.subTest(job=name):
                self.assertEqual(found, expected[name])
                self.assertEqual("environment:" in job, name == "publish")
        self.assertIn("    environment: crates-io-release\n", self.JOBS["publish"])
        self.assertIn("inputs.operation == 'publish' && vars.TILEWRIGHT_CRATES_PUBLISH == 'true' && "
                      "needs.verify.outputs.owner == 'true' && needs.resolve.result == 'success'", self.JOBS["publish"])

    def test_actions_are_pinned_and_the_publish_job_is_minimal(self):
        uses = re.findall(r"uses: (\S+)( # v\S+)?", self.TEXT)
        for action, comment in uses:
            with self.subTest(action=action):
                self.assertRegex(action, r"^[a-z0-9-]+/[a-z0-9-]+@[0-9a-f]{40}$")
                self.assertTrue(comment)
        publish = self.JOBS["publish"]
        self.assertEqual(sorted({action.split("@")[0] for action in re.findall(r"uses: (\S+)", publish)}),
                         ["actions/checkout", "actions/upload-artifact", "rust-lang/crates-io-auth-action"])
        self.assertRegex(publish, r"uses: rust-lang/crates-io-auth-action@[0-9a-f]{40} # v")
        for word in ("cargo", "npm", "pip", "rustup"):
            for name in ("verify", "publish"):
                with self.subTest(word=word, job=name):
                    self.assertNotRegex(self.JOBS[name], rf"\b{word}\b")

    def test_the_registry_token_reaches_only_the_upload_step(self):
        lines = [line for line in self.TEXT.splitlines() if "CARGO_REGISTRY_TOKEN" in line]
        self.assertEqual(lines, ["          CARGO_REGISTRY_TOKEN: ${{ steps.auth.outputs.token }}"])
        upload = next(step for step in steps(self.JOBS["publish"]) if "id: upload" in step)
        environment = re.findall(r"^          ([A-Z_]+):", upload, re.MULTILINE)
        self.assertEqual(environment, ["CARGO_REGISTRY_TOKEN", "GH_INTENT_TOKEN", "TILEWRIGHT_PUBLISH_RECEIPTS",
                                       "TILEWRIGHT_CRATES_PUBLISH"])
        self.assertIn("publish.py upload", upload)
        for name, job in self.JOBS.items():
            for step in steps(job):
                if "GH_READ_TOKEN" in step:
                    self.assertIn("publish.py verify", step)
                if "GH_INTENT_TOKEN" in step:
                    self.assertIn("publish.py upload", step)

    def test_receipts_confirm_and_accept_coupling(self):
        publish = self.JOBS["publish"]
        self.assertIn("if: ${{ always() }}\n        uses: actions/upload-artifact@", publish)
        self.assertIn("name: crate-put-outcomes-${{ github.run_id }}-${{ github.run_attempt }}", publish)
        self.assertIn("retention-days: 90", publish)
        self.assertIn("complete: ${{ steps.upload.outputs.complete }}", publish)
        accept = self.JOBS["accept"]
        self.assertIn("!cancelled()", accept)
        self.assertIn("needs.publish.outputs.complete == 'true'", accept)
        self.assertNotIn("needs.publish.result", accept)
        self.assertIn("publish.py accept-lock", accept)
        self.assertIn('cargo "+${TOOLCHAIN}" check --locked', accept)
        resolve = self.JOBS["resolve"]
        self.assertIn("--patch", resolve)
        self.assertRegex(resolve, r'cargo "\+\$\{TOOLCHAIN\}" check\n')

    def test_the_names_crates_io_trusts_are_the_documented_ones(self):
        self.assertTrue(P.WORKFLOW_REF.endswith("/.github/workflows/publish-crate.yml@refs/heads/main"))
        releasing = (ROOT / "docs/RELEASING.md").read_text()
        for value in ("publish-crate.yml", "crates-io-release", "314492348"):
            self.assertIn(value, releasing)


if __name__ == "__main__":
    unittest.main()
