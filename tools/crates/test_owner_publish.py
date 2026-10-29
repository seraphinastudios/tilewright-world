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
    return {"": {"id": P.REPOSITORY_ID, "full_name": P.REPOSITORY, "default_branch": "main", "archived": False},
            "releases/123": {"id": 123, "tag_name": "v0.4.0", "immutable": True, "draft": False,
                             "prerelease": False, "assets": [
                                 {"id": 11, "name": P.bundle_name(PLAN), "digest": "sha256:" + PLAN["bundle_sha256"], "size": 32, "state": "uploaded"},
                                 {"id": 12, "name": "SHA256SUMS", "digest": "sha256:" + "d" * 64, "size": 128, "state": "uploaded"}]},
            "commits/v0.4.0": {"sha": REVISION}, "commits/go%2Fv0.4.0": {"sha": REVISION},
            f"compare/{REVISION}...main": {"status": "ahead", "base_commit": {"sha": REVISION}},
            "actions/runs/456/attempts/2": {"id": 456, "run_attempt": 2, "head_sha": REVISION,
                                           "head_branch": "main", "path": ".github/workflows/release.yml",
                                           "repository": {"id": P.REPOSITORY_ID}, "event": "push", "status": "completed",
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

    def test_github_repository_endpoint_has_no_trailing_slash(self):
        with patch.object(P, "request", return_value=(200, b'{"id":1}')) as request:
            self.assertEqual(P.api(""), {"id": 1})
            request.assert_called_once_with("api.github.com", f"/repos/{P.REPOSITORY}")

    def test_release_must_bind_tags_main_repository_and_run_attempt(self):
        evidence = github()
        with patch.object(P, "api", side_effect=evidence.__getitem__):
            self.assertEqual(P.release_evidence(PLAN)[0]["id"], P.REPOSITORY_ID)
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


class ExecuteTests(unittest.TestCase):
    TOKEN = "test-only-secret-never-real"

    def setUp(self):
        self.rows = [dict(name=name, version="0.1.0", asset=f"{name}-0.1.0.crate", sha256=P.sha256(name.encode()))
                     for name in ("tilewright-jsonc", "tilewright-source")]
        self.files = {row["asset"]: row["name"].encode() for row in self.rows}
        self.archives = {row["name"]: {"Cargo.toml": (
            f'[package]\nname="{row["name"]}"\nversion="0.1.0"\nreadme="README.md"\n'
            + ('[dependencies]\ntilewright-jsonc="=0.1.0"\n' if row["name"] == "tilewright-source" else '')).encode(),
            "README.md": b"README"} for row in self.rows}
        self.states = {row["name"]: "absent" for row in self.rows}

    def batch(self, **overrides):
        from contextlib import ExitStack
        stack = ExitStack()
        self.addCleanup(stack.close)
        names = {"terminal_boundary": None, "public_owner": None, "github_authority": None,
                 "require_no_intent": None, "clean_source": None, "release_evidence": ({"id": 1}, {}),
                 "reserve_intent": "f" * 40, "put_once": None, "owned": None,
                 "confirm_publication": None}
        names.update(overrides)
        mocks = {name: stack.enter_context(patch.object(P, name, **(
            {"side_effect": result} if callable(result) or isinstance(result, Exception) else {"return_value": result})))
            for name, result in names.items()}
        # A completed predecessor becomes publicly visible; pending successors stay absent.
        uploaded = set()
        mocks["put_once"].side_effect = lambda data, *_: uploaded.add(data.decode())
        mocks["registry_state"] = stack.enter_context(patch.object(P, "registry_state",
            side_effect=lambda row: "exact-existing" if row["name"] in uploaded else "absent"))
        mocks["input"] = stack.enter_context(patch("builtins.input", return_value=f"PUBLISH AS {P.OWNER}"))
        mocks["getpass"] = stack.enter_context(patch.object(P.getpass, "getpass", return_value=self.TOKEN))
        import resource
        stack.enter_context(patch.object(resource, "setrlimit"))
        mocks["stdout"] = stack.enter_context(patch.object(P.sys, "stdout", new_callable=io.StringIO))
        mocks["stderr"] = stack.enter_context(patch.object(P.sys, "stderr", new_callable=io.StringIO))
        return mocks

    def run_batch(self):
        P.execute(PLAN, {"packages": self.rows}, self.files, self.archives, self.states)

    def test_success_exact_held_bytes_order_single_ceremony_and_ownership(self):
        mocks = self.batch()
        self.run_batch()
        self.assertEqual(mocks["getpass"].call_count, 1)
        self.assertEqual(mocks["input"].call_count, 1)
        self.assertEqual([call.args[0] for call in mocks["put_once"].call_args_list], list(self.files.values()))
        self.assertEqual(mocks["reserve_intent"].call_count, 2)
        self.assertEqual([call.args[0] for call in mocks["confirm_publication"].call_args_list], self.rows)
        self.assertNotIn(self.TOKEN, mocks["stdout"].getvalue() + mocks["stderr"].getvalue())
        mocks["owned"].assert_called_once_with(self.rows[0])

    def test_five_crate_batch_routes_repeated_evidence_to_authenticated_reader(self):
        names = ("tilewright-jsonc", "tilewright-source", "tilewright-pack", "tilewright-diff", "tilewright-edit")
        self.rows = [dict(name=name, version="0.1.0", asset=f"{name}-0.1.0.crate", sha256=P.sha256(name.encode()))
                     for name in names]
        self.files = {row["asset"]: row["name"].encode() for row in self.rows}
        self.archives = {name: {"Cargo.toml": f'[package]\nname="{name}"\nversion="0.1.0"\n'.encode()} for name in names}
        self.states = dict.fromkeys(names, "absent")
        evidence = github()
        prefix = f"/repos/{P.REPOSITORY}"
        refs = {}
        public_reads = []
        authenticated_reads = []

        def public_request(host, path, **_):
            self.assertEqual(host, "api.github.com")
            public_reads.append(path)
            if "/git/ref/tags/" in path:
                return 404, b""
            return 200, json.dumps(evidence[path[len(prefix):].lstrip("/")]).encode()

        def github_call(path, body=None):
            suffix = path[len(prefix)-1:].lstrip("/")
            if suffix == "git/tags":
                return {"sha": "f" * 40}
            if suffix == "git/refs":
                ref = body["ref"]
                self.assertNotIn(ref, refs)
                refs[ref] = {"ref": ref, "object": {"type": "tag", "sha": body["sha"]}}
                return refs[ref]
            if suffix.startswith("git/ref/tags/"):
                return refs["refs/" + suffix[len("git/ref/"):]]
            authenticated_reads.append(suffix)
            return evidence[suffix]

        release_reader = P.release_evidence
        mocks = self.batch(release_evidence=release_reader, reserve_intent=P.reserve_intent,
                           require_no_intent=P.require_no_intent)
        with patch.object(P, "request", side_effect=public_request), patch.object(P, "github_call", side_effect=github_call) as gh:
            # The two credential-free preflight evidence snapshots remain public.
            release_reader(PLAN)
            release_reader(PLAN)
            self.run_batch()
            self.assertNotIn(self.TOKEN, repr(gh.call_args_list))
        self.assertEqual(len(public_reads), 22)  # 12 preflight + 10 intent absence checks
        self.assertEqual(len(authenticated_reads), 66)  # 11 execution snapshots, six reads each
        self.assertEqual(len(refs), 5)
        self.assertEqual(mocks["put_once"].call_count, 5)
        self.assertEqual(mocks["getpass"].call_count, 1)

    def test_cancel_or_nonmatching_assertion_precedes_token_intent_and_upload(self):
        for answer in ("no", "PUBLISH AS someone-else"):
            with self.subTest(answer=answer):
                mocks = self.batch()
                mocks["input"].return_value = answer
                with self.assertRaises(P.Refusal):
                    self.run_batch()
                mocks["getpass"].assert_not_called()
                mocks["reserve_intent"].assert_not_called()
                mocks["put_once"].assert_not_called()
        mocks = self.batch()
        mocks["getpass"].side_effect = KeyboardInterrupt()
        with self.assertRaises(KeyboardInterrupt):
            self.run_batch()
        mocks["reserve_intent"].assert_not_called()

    def test_existing_intent_blocks_even_public_absence_and_no_credential_prompt(self):
        mocks = self.batch(require_no_intent=P.Refusal("existing intent"))
        with self.assertRaises(P.Refusal):
            self.run_batch()
        mocks["getpass"].assert_not_called()
        mocks["reserve_intent"].assert_not_called()
        mocks["put_once"].assert_not_called()

    def test_ambiguous_first_put_stops_batch_no_retry_or_later_intent(self):
        mocks = self.batch()
        mocks["put_once"].side_effect = P.Refusal("ambiguous upload")
        with self.assertRaises(P.Refusal):
            self.run_batch()
        self.assertEqual(mocks["put_once"].call_count, 1)
        self.assertEqual(mocks["reserve_intent"].call_count, 1)
        mocks["confirm_publication"].assert_not_called()

    def test_partial_publication_owner_mismatch_stops_following_crate(self):
        mocks = self.batch(confirm_publication=P.Refusal("owner mismatch"))
        with self.assertRaises(P.Refusal):
            self.run_batch()
        self.assertEqual(mocks["put_once"].call_count, 1)
        self.assertEqual(mocks["reserve_intent"].call_count, 1)

    def test_dependency_not_visible_prevents_dependent_intent(self):
        mocks = self.batch()
        mocks["registry_state"].return_value = "absent"
        mocks["registry_state"].side_effect = None
        with self.assertRaisesRegex(P.Refusal, "dependency"):
            self.run_batch()
        self.assertEqual(mocks["put_once"].call_count, 1)
        self.assertEqual(mocks["reserve_intent"].call_count, 1)

    def test_mid_batch_authenticated_budget_error_stops_before_next_intent(self):
        mocks = self.batch()
        snapshot = ({"id": 1}, {})
        mocks["release_evidence"].side_effect = [snapshot, snapshot, snapshot, P.Refusal("GitHub budget unavailable")]
        with self.assertRaisesRegex(P.Refusal, "budget"):
            self.run_batch()
        self.assertEqual(mocks["put_once"].call_count, 1)
        self.assertEqual(mocks["reserve_intent"].call_count, 1)
        self.assertEqual(mocks["confirm_publication"].call_count, 1)

    def test_source_drift_after_intent_never_uploads(self):
        mocks = self.batch()
        mocks["release_evidence"].side_effect = [(1, {}), (1, {}), (2, {})]
        with self.assertRaisesRegex(P.Refusal, "changed after intent"):
            self.run_batch()
        self.assertEqual(mocks["reserve_intent"].call_count, 1)
        mocks["put_once"].assert_not_called()

    def test_existing_exact_archives_require_ownership_and_no_token(self):
        self.states = {row["name"]: "exact-existing" for row in self.rows}
        mocks = self.batch()
        self.run_batch()
        self.assertEqual(mocks["owned"].call_count, 2)
        mocks["getpass"].assert_not_called()
        mocks["put_once"].assert_not_called()

    def test_terminal_ci_environment_and_public_owner_refusals(self):
        with patch.object(P.sys.stdin, "isatty", return_value=False), self.assertRaises(P.Refusal):
            P.terminal_boundary()
        with patch.object(P.sys.stdin, "isatty", return_value=True), patch.object(P.sys.stderr, "isatty", return_value=True):
            for key in ("CI", "GITHUB_ACTIONS", "CRATES_IO_TOKEN", "CARGO_REGISTRY_TOKEN", "ACTIONS_ID_TOKEN_REQUEST_URL"):
                with patch.dict(os.environ, {key: "secret"}, clear=True), self.assertRaises(P.Refusal) as caught:
                    P.terminal_boundary()
                self.assertNotIn("secret", str(caught.exception))
        with patch.object(P, "request", return_value=(200, b'{"user":{"login":"someone-else","id":437663}}')):
            with self.assertRaises(P.Refusal):
                P.public_owner()
        with patch.object(P, "request", return_value=(200, b'{"users":[{"login":"zoeyrose","id":3865595,"kind":"user"}]}')):
            with self.assertRaises(P.Refusal):
                P.owned(self.rows[0])

    def test_exact_wire_framing_and_no_redirect_retry_or_error_echo(self):
        metadata = P.upload_metadata(self.archives[self.rows[0]["name"]])
        connection = Mock()
        connection.getresponse.return_value.status = 200
        connection.getresponse.return_value.read.return_value = b'{"warnings":{}}'
        with patch.object(P.http.client, "HTTPSConnection", return_value=connection):
            P.put_once(b"EXACT", metadata, self.TOKEN)
        call = connection.request.call_args
        self.assertEqual(call.args, ("PUT", "/api/v1/crates/new"))
        body = call.kwargs["body"]
        length = P.struct.unpack("<I", body[:4])[0]
        self.assertEqual(json.loads(body[4:4+length]), metadata)
        self.assertEqual(P.struct.unpack("<I", body[4+length:8+length])[0], 5)
        self.assertEqual(body[8+length:], b"EXACT")
        self.assertEqual(call.kwargs["headers"]["Authorization"], self.TOKEN)
        for status in (301, 302, 307, 308, 400, 403, 429, 500):
            connection.reset_mock()
            connection.getresponse.return_value.status = status
            with patch.object(P.http.client, "HTTPSConnection", return_value=connection), self.assertRaises(P.Refusal):
                P.put_once(b"EXACT", metadata, self.TOKEN)
            connection.request.assert_called_once()
        connection.getresponse.return_value.status = 200
        connection.getresponse.return_value.read.return_value = json.dumps({"errors": [{"detail": self.TOKEN}]}).encode()
        with patch.object(P.http.client, "HTTPSConnection", return_value=connection), self.assertRaises(P.Refusal) as caught:
            P.put_once(b"EXACT", metadata, self.TOKEN)
        self.assertNotIn(self.TOKEN, str(caught.exception))

    def test_metadata_renames_targets_features_and_kinds(self):
        files = {"README.md": b"hello", "Cargo.toml": b'''[package]
name="tilewright-source"
version="0.1.0"
readme="README.md"
license-file="LICENSE"
rust-version="1.97"
[features]
f=["alias/extra"]
[dependencies.alias]
package="tilewright-jsonc"
version="=0.1.0"
optional=true
default-features=false
features=["extra"]
[target.'cfg(unix)'.build-dependencies]
build="1"
[dev-dependencies]
test="2"
'''}
        metadata = P.upload_metadata(files)
        self.assertEqual(metadata["readme"], "hello")
        self.assertEqual(metadata["license_file"], "LICENSE")
        self.assertEqual(metadata["rust_version"], "1.97")
        dep = metadata["deps"][0]
        self.assertEqual((dep["name"], dep["explicit_name_in_toml"], dep["optional"], dep["default_features"]),
                         ("tilewright-jsonc", "alias", True, False))
        self.assertEqual([(d["kind"], d["target"]) for d in metadata["deps"]],
                         [("normal", None), ("dev", None), ("build", "cfg(unix)")])
        self.assertEqual(metadata["features"], {"f": ["alias/extra"]})
        files["Cargo.toml"] += b'\n[dependencies.bad]\nversion="1"\ngit="https://invalid"\n'
        with self.assertRaises(P.Refusal):
            P.upload_metadata(files)

    def test_intent_atomic_create_race_never_adopts_existing_reservation(self):
        row = self.rows[0]
        ref = "refs/tags/" + P.intent_name(row)
        object_value = {"type": "tag", "sha": "f" * 40}
        created = {"ref": ref, "object": object_value}
        calls = []
        reserved = False

        def github(path, body=None):
            nonlocal reserved
            calls.append((path, body))
            if path.endswith("git/tags"):
                record = json.loads(body["message"])
                self.assertEqual(record["release"], PLAN)
                self.assertEqual(record["coordinate"]["sha256"], row["sha256"])
                return {"sha": "f" * 40}
            if path.endswith("git/refs"):
                if reserved:
                    raise P.Refusal("422 reference already exists")
                reserved = True
            return created

        with patch.object(P, "require_no_intent"), patch.object(P, "github_authority"), patch.object(P, "github_call", side_effect=github):
            self.assertEqual(P.reserve_intent(PLAN, row, "e" * 64), "f" * 40)
            # Simulate another host seeing a stale absence. The atomic ref write fences it.
            with self.assertRaises(P.Refusal):
                P.reserve_intent(PLAN, row, "e" * 64)
        self.assertEqual(sum(path.endswith("git/refs") for path, _ in calls), 2)
        self.assertTrue(all("TOKEN" not in json.dumps(body) for _, body in calls))

    def test_github_subprocess_only_receives_public_intent_no_registry_secret(self):
        response = Mock(returncode=0, stdout=b'{"sha":"abc"}')
        with patch.object(P.subprocess, "run", return_value=response) as run:
            P.github_call("repos/seraphinastudios/tilewright-world/git/refs", {"ref": "public-intent"})
        arguments = run.call_args.args[0]
        self.assertEqual(arguments[:5], ["gh", "api", "--hostname", "github.com", "--method"])
        self.assertNotIn(self.TOKEN, repr(run.call_args))
        self.assertEqual(json.loads(run.call_args.kwargs["input"]), {"ref": "public-intent"})

    def test_confirm_reads_only_bounded_visibility_and_owner(self):
        with patch.object(P, "registry_state", side_effect=["absent", "exact-existing"]) as state, \
             patch.object(P, "owned") as owned, patch.object(P.time, "sleep"), patch.object(P, "put_once") as put:
            P.confirm_publication(self.rows[0])
            self.assertEqual(state.call_count, 2)
            owned.assert_called_once_with(self.rows[0])
            put.assert_not_called()
        with patch.object(P, "registry_state", return_value="absent") as state, \
             patch.object(P.time, "sleep"), self.assertRaises(P.Refusal):
            P.confirm_publication(self.rows[0])
        self.assertEqual(state.call_count, 12)


if __name__ == "__main__":
    unittest.main()
