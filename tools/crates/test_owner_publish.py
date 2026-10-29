#!/usr/bin/env python3
"""Owner publication boundaries; all services, tools and credentials are mocked."""
import copy
import importlib.util
import io
import json
import os
from pathlib import Path
import tempfile
import unittest
from unittest.mock import Mock, patch

SPEC = importlib.util.spec_from_file_location("owner_publish", Path(__file__).with_name("owner_publish.py"))
P = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(P)
REVISION = "a" * 40
PLAN = {"repository": P.REPOSITORY, "release_id": 123, "tag": "v0.4.0", "revision": REVISION,
        "run_id": 456, "attempt": 2, "bundle_sha256": "b" * 64, "manifest_sha256": "c" * 64}


def attestation():
    return [{"verificationResult": {"signature": {"certificate": {
        "subjectAlternativeName": P.WORKFLOW, "issuer": "https://token.actions.githubusercontent.com",
        "sourceRepositoryURI": P.REPO_URL, "sourceRepositoryDigest": REVISION,
        "sourceRepositoryRef": "refs/heads/main", "sourceRepositoryIdentifier": "1",
        "buildConfigURI": P.WORKFLOW, "buildConfigDigest": REVISION, "runnerEnvironment": "github-hosted",
        "runInvocationURI": f"{P.REPO_URL}/actions/runs/456/attempts/2"}},
        "statement": {"predicateType": "https://slsa.dev/provenance/v1",
                      "subject": [{"digest": {"sha256": PLAN["bundle_sha256"]}}]}}}]


def github():
    return {"": {"id": 1, "full_name": P.REPOSITORY, "default_branch": "main", "archived": False},
            "releases/123": {"id": 123, "tag_name": "v0.4.0", "immutable": True, "draft": False,
                             "prerelease": False, "assets": [
                                 {"id": 11, "name": P.bundle_name(PLAN), "digest": "sha256:" + PLAN["bundle_sha256"], "size": 32, "state": "uploaded"},
                                 {"id": 12, "name": "SHA256SUMS", "digest": "sha256:" + "d" * 64, "size": 128, "state": "uploaded"}]},
            "commits/v0.4.0": {"sha": REVISION}, "commits/go%2Fv0.4.0": {"sha": REVISION},
            f"compare/{REVISION}...main": {"status": "ahead", "base_commit": {"sha": REVISION}},
            "actions/runs/456/attempts/2": {"id": 456, "run_attempt": 2, "head_sha": REVISION,
                                           "head_branch": "main", "path": ".github/workflows/release.yml",
                                           "repository": {"id": 1}, "event": "push", "status": "completed",
                                           "conclusion": "success"}}


class PreflightTests(unittest.TestCase):
    def test_exact_pins_and_no_latest(self):
        self.assertEqual(P.pins(copy.deepcopy(PLAN)), PLAN)
        for key, invalid in (("repository", "attacker/world"), ("tag", "latest"), ("revision", "main"),
                             ("bundle_sha256", "abc"), ("attempt", True), ("run_id", 0)):
            value = dict(PLAN, **{key: invalid})
            with self.subTest(key=key), self.assertRaises(P.Refusal):
                P.pins(value)
        with self.assertRaises(P.Refusal):
            P.decode('{"attempt":1,"attempt":2}')

    def test_release_must_bind_tags_main_repository_and_run_attempt(self):
        evidence = github()
        with patch.object(P, "api", side_effect=evidence.__getitem__):
            self.assertEqual(P.release_evidence(PLAN)[0]["id"], 1)
        cases = [("releases/123", "immutable", False), ("releases/123", "prerelease", True),
                 ("commits/go%2Fv0.4.0", "sha", "f" * 40),
                 (f"compare/{REVISION}...main", "status", "diverged"),
                 ("actions/runs/456/attempts/2", "run_attempt", 3),
                 ("actions/runs/456/attempts/2", "head_branch", "feature/forged"),
                 ("actions/runs/456/attempts/2", "conclusion", "failure"),
                 ("actions/runs/456/attempts/2", "path", ".github/workflows/other.yml"),
                 ("actions/runs/456/attempts/2", "repository", {"id": 2})]
        for endpoint, field, value in cases:
            changed = copy.deepcopy(evidence)
            changed[endpoint][field] = value
            with self.subTest(endpoint=endpoint, field=field), patch.object(P, "api", side_effect=changed.__getitem__):
                with self.assertRaises(P.Refusal):
                    P.release_evidence(PLAN)

    def test_drift_checks_ignore_counters_but_bind_asset_identity(self):
        evidence = github()
        with patch.object(P, "api", side_effect=evidence.__getitem__):
            first = P.release_evidence(PLAN)
            evidence[""]["stargazers_count"] = 12
            evidence["releases/123"]["assets"][0]["download_count"] = 99
            self.assertEqual(P.release_evidence(PLAN), first)
            evidence["releases/123"]["assets"][0]["id"] = 999
            self.assertNotEqual(P.release_evidence(PLAN), first)

    def test_real_candidate_bundle_uses_canonical_verifier_without_cargo(self):
        fixture_spec = importlib.util.spec_from_file_location("owner_candidate_fixture", Path(__file__).with_name("test_prepare.py"))
        fixture = importlib.util.module_from_spec(fixture_spec)
        fixture_spec.loader.exec_module(fixture)
        bundle_spec = importlib.util.spec_from_file_location("owner_bundle_fixture", P.ROOT / "tools/release/crate_candidates.py")
        bundler = importlib.util.module_from_spec(bundle_spec)
        bundle_spec.loader.exec_module(bundler)
        with tempfile.TemporaryDirectory() as directory, patch.object(P.subprocess, "run") as command:
            root = Path(directory)
            fixture.write_candidate(root)
            data = bundler.bundle(root, REVISION)
            plan = dict(PLAN, bundle_sha256=P.sha256(data),
                        manifest_sha256=P.sha256((root / "candidate.json").read_bytes()))
            manifest, files, archives = P.verify_bundle(data, plan)
            self.assertEqual(len(manifest["packages"]), 5)
            self.assertEqual(len(archives), 5)
            self.assertEqual(files["tilewright-jsonc-0.1.0.crate"], (root / "tilewright-jsonc-0.1.0.crate").read_bytes())
            command.assert_not_called()
            with self.assertRaisesRegex(P.Refusal, "manifest differs"):
                P.verify_bundle(data, dict(plan, manifest_sha256="f" * 64))

    def test_full_preflight_retains_snapshot_and_detects_late_drift(self):
        data = b"verified bundle bytes"
        plan = dict(PLAN, bundle_sha256=P.sha256(data))
        sums = f"{plan['bundle_sha256']}  {P.bundle_name(plan)}\n".encode()
        assets = {"SHA256SUMS": {"digest": "sha256:" + P.sha256(sums)}}
        evidence = ({"id": 1}, assets)
        result = attestation()
        result[0]["verificationResult"]["statement"]["subject"][0]["digest"]["sha256"] = plan["bundle_sha256"]
        row = {"name": "tilewright-jsonc", "version": "0.1.0", "sha256": "c" * 64}
        checked = ({"packages": [row]}, {"archive.crate": b"exact archive"}, {})
        with tempfile.TemporaryDirectory() as directory:
            bundle, checksum_file = Path(directory) / "bundle", Path(directory) / "SHA256SUMS"
            bundle.write_bytes(data)
            checksum_file.write_bytes(sums)
            commands = []

            def verifier(arguments):
                commands.append(arguments)
                self.assertEqual(Path(arguments[4] if arguments[1] == "release" else arguments[3]).read_bytes(), data)
                bundle.write_bytes(b"owner input changed after snapshot")
                return json.dumps(result).encode() if arguments[1] == "attestation" else b""

            with patch.dict(os.environ, {}, clear=True), patch.object(P, "clean_source"), \
                 patch.object(P, "release_evidence", return_value=evidence), \
                 patch.object(P, "command", side_effect=verifier), \
                 patch.object(P, "verify_bundle", return_value=checked) as verify, \
                 patch.object(P, "registry_state", return_value="absent"):
                self.assertEqual(P.preflight(plan, bundle, checksum_file)[1], checked[1])
                verify.assert_called_once_with(data, plan)
                self.assertEqual([command[:3] for command in commands],
                                 [["gh", "release", "verify-asset"], ["gh", "attestation", "verify"]])
            bundle.write_bytes(data)
            with patch.dict(os.environ, {}, clear=True), patch.object(P, "clean_source"), \
                 patch.object(P, "release_evidence", side_effect=[evidence, ({"id": 2}, assets)]), \
                 patch.object(P, "command", side_effect=verifier), \
                 patch.object(P, "verify_bundle", return_value=checked), \
                 patch.object(P, "registry_state", return_value="absent"):
                with self.assertRaisesRegex(P.Refusal, "changed during"):
                    P.preflight(plan, bundle, checksum_file)

    def test_certificate_binds_exact_run_not_user_controlled_predicate(self):
        P.verify_attestation(attestation(), PLAN, 1)
        for field in attestation()[0]["verificationResult"]["signature"]["certificate"]:
            result = attestation()
            result[0]["verificationResult"]["signature"]["certificate"][field] = "wrong"
            result[0]["verificationResult"]["statement"]["predicate"] = {"runDetails": {
                "metadata": {"invocationId": f"{P.REPO_URL}/actions/runs/456/attempts/2"}}}
            with self.subTest(field=field), self.assertRaises(P.Refusal):
                P.verify_attestation(result, PLAN, 1)
        result = attestation()
        result[0]["verificationResult"]["statement"]["subject"][0]["digest"]["sha256"] = "d" * 64
        with self.assertRaises(P.Refusal):
            P.verify_attestation(result, PLAN, 1)

    def test_public_transport_never_redirects_or_authenticates_or_retries(self):
        for status in (301, 302, 307, 308, 401, 403, 429, 500):
            connection = Mock()
            connection.getresponse.return_value.status = status
            with self.subTest(status=status), patch.object(P.http.client, "HTTPSConnection", return_value=connection):
                with self.assertRaises(P.Refusal):
                    P.request("crates.io", "/api/v1/crates/tilewright-jsonc")
            connection.request.assert_called_once()
            self.assertEqual(connection.request.call_args.args[0], "GET")
            self.assertNotIn("Authorization", connection.request.call_args.kwargs["headers"])
            connection.close.assert_called_once()
        with self.assertRaises(P.Refusal):
            P.request("evil.example", "/")

    def test_public_transport_has_response_bound(self):
        connection = Mock()
        connection.getresponse.return_value.status = 200
        connection.getresponse.return_value.read.return_value = b"x" * 9
        with patch.object(P.http.client, "HTTPSConnection", return_value=connection), self.assertRaises(P.Refusal):
            P.request("crates.io", "/", limit=8)
        connection.getresponse.return_value.read.assert_called_once_with(9)

    def test_registry_absence_is_only_public_observation_and_disagreement_fails(self):
        row = {"name": "tilewright-jsonc", "version": "0.1.0", "sha256": "a" * 64}
        with patch.object(P, "request", return_value=(404, b"")) as request:
            self.assertEqual(P.registry_state(row), "absent")
            self.assertEqual(request.call_count, 2)
        with patch.object(P, "request", side_effect=[(404, b""), (200, b"{}")]), self.assertRaises(P.Refusal):
            P.registry_state(row)

    def test_exact_existing_requires_api_index_and_download_bytes(self):
        data = b"preverified archive bytes"
        row = {"name": "tilewright-jsonc", "version": "0.1.0", "sha256": P.sha256(data)}
        metadata = {"crate": {"name": row["name"]}, "versions": [
            {"num": row["version"], "checksum": row["sha256"], "yanked": False}]}
        index = {"name": row["name"], "vers": row["version"], "cksum": row["sha256"], "yanked": False}
        replies = [(200, json.dumps(metadata).encode()), (200, json.dumps(index).encode()), (200, data)]
        with patch.object(P, "request", side_effect=replies):
            self.assertEqual(P.registry_state(row), "exact-existing")
        with patch.object(P, "request", side_effect=[*replies[:2], (200, b"changed")]), self.assertRaises(P.Refusal):
            P.registry_state(row)
        metadata["versions"][0]["yanked"] = True
        with patch.object(P, "request", side_effect=[(200, json.dumps(metadata).encode()), replies[1]]):
            with self.assertRaises(P.Refusal):
                P.registry_state(row)

    def test_unattended_preflight_never_reads_credentials_or_executes_cargo(self):
        with patch.dict(os.environ, {"CARGO_REGISTRY_TOKEN": "DO-NOT-LOG"}), patch.object(P, "command") as command:
            with self.assertRaisesRegex(P.Refusal, "remove registry") as caught:
                P.preflight(PLAN, Path("missing"), Path("missing"))
            self.assertNotIn("DO-NOT-LOG", str(caught.exception))
            command.assert_not_called()

    def test_no_verifier_error_output_or_inherited_stdin_disclosed(self):
        result = Mock(returncode=1, stdout=b"PRIVATE", stderr=b"SECRET")
        with patch.object(P.subprocess, "run", return_value=result) as run:
            with self.assertRaises(P.Refusal) as caught:
                P.command(["gh", "attestation", "verify"])
            self.assertNotIn("SECRET", str(caught.exception))
            self.assertEqual(run.call_args.kwargs["stdin"], P.subprocess.DEVNULL)

    def test_input_bounds_symlinks_and_moved_or_dirty_source(self):
        with tempfile.TemporaryDirectory() as directory:
            real = Path(directory) / "file"
            real.write_bytes(b"1234")
            link = Path(directory) / "link"
            link.symlink_to(real)
            for path, limit in ((real, 3), (link, 4)):
                with self.assertRaises(P.Refusal):
                    P.bounded_file(path, limit)
        with patch.object(P, "command", return_value=b"b" * 40), self.assertRaises(P.Refusal):
            P.clean_source(REVISION)
        with patch.object(P, "command", side_effect=[REVISION.encode(), b" M file"]), self.assertRaises(P.Refusal):
            P.clean_source(REVISION)


if __name__ == "__main__":
    unittest.main()
