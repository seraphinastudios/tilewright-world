#!/usr/bin/env python3
"""Preflight one explicitly pinned source release for owner first publication.

No registry credentials are read by preflight. No Cargo commands are executed.
"""
from __future__ import annotations

import argparse
import hashlib
import http.client
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tempfile
from urllib.parse import quote

ROOT = Path(__file__).resolve().parents[2]
REPOSITORY = "seraphinastudios/tilewright-world"
REPO_URL = f"https://github.com/{REPOSITORY}"
WORKFLOW = f"{REPO_URL}/.github/workflows/release.yml@refs/heads/main"
MAX_JSON = 4 * 1024 * 1024
MAX_BUNDLE = 256 * 1024 * 1024


class Refusal(Exception):
    """A sanitized, actionable refusal safe to show in an owner terminal."""


def require(condition, message):
    if not condition:
        raise Refusal(message)


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def decode(data):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, "duplicate JSON field")
            result[key] = value
        return result
    return json.loads(data, object_pairs_hook=unique)


def request(host, path, *, limit=MAX_JSON):
    """Public GET only, no proxies, auth inheritance, cookies, redirects or retries."""
    require(host in {"api.github.com", "crates.io", "index.crates.io", "static.crates.io"},
            "unsupported public service")
    connection = http.client.HTTPSConnection(host, timeout=30)
    try:
        connection.request("GET", path, headers={
            "User-Agent": "tilewright-world-owner-preflight/1 (github.com/seraphinastudios/tilewright-world)",
            "Accept": "application/json", "Cache-Control": "no-cache"})
        response = connection.getresponse()
        require(response.status in {200, 404}, "public service unavailable or returned a redirect")
        data = response.read(limit + 1)
        require(len(data) <= limit, "public response exceeds size limit")
        return response.status, data
    except (OSError, http.client.HTTPException):
        raise Refusal("public service request failed; no publication attempted") from None
    finally:
        connection.close()


def api(path):
    status, data = request("api.github.com", f"/repos/{REPOSITORY}/{path}")
    require(status == 200, "required GitHub evidence is unavailable")
    return decode(data)


def command(arguments, *, cwd=ROOT):
    try:
        result = subprocess.run(arguments, cwd=cwd, stdin=subprocess.DEVNULL,
                                capture_output=True, timeout=180, check=False)
    except (OSError, subprocess.TimeoutExpired):
        raise Refusal("required local verifier could not finish") from None
    require(result.returncode == 0, "required local verifier refused; inspect it separately without registry credentials")
    require(len(result.stdout) <= 16 * MAX_JSON, "verifier output exceeds size limit")
    return result.stdout


def clean_source(revision):
    require(command(["git", "rev-parse", "HEAD"]).decode().strip() == revision,
            "run the merged helper from the exact reviewed release revision")
    require(not command(["git", "status", "--porcelain", "--untracked-files=all"]).strip(),
            "source checkout must be clean")


def pins(value):
    require(set(value) == {"repository", "release_id", "tag", "revision", "run_id", "attempt",
                           "bundle_sha256", "manifest_sha256"}, "publication decision fields differ")
    require(value["repository"] == REPOSITORY, "wrong repository")
    require(re.fullmatch(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", value["tag"]),
            "pin a stable source-release tag")
    for key, count in (("revision", 40), ("bundle_sha256", 64), ("manifest_sha256", 64)):
        require(re.fullmatch(f"[0-9a-f]{{{count}}}", value[key]), "invalid pinned digest")
    for key in ("release_id", "run_id", "attempt"):
        require(type(value[key]) is int and value[key] > 0, "invalid pinned release or run identity")
    return value


def bundle_name(plan):
    return f"tilewright-world-{plan['tag'][1:]}-crate-candidates.tar.gz"


def bounded_file(path, limit):
    require(path.is_file() and not path.is_symlink(), "input must be a regular non-symlink file")
    with path.open("rb") as stream:
        data = stream.read(limit + 1)
    require(len(data) <= limit, "input exceeds size limit")
    return data


def release_evidence(plan):
    repository = api("")
    require(repository["full_name"] == REPOSITORY and repository["default_branch"] == "main" and
            repository["archived"] is False, "repository identity or default branch changed")
    release = api(f"releases/{plan['release_id']}")
    require(release["id"] == plan["release_id"] and release["tag_name"] == plan["tag"] and
            release["immutable"] is True and release["draft"] is False and
            release["prerelease"] is False, "release must be the pinned immutable stable publication")
    for tag in (plan["tag"], "go/" + plan["tag"]):
        require(api("commits/" + quote(tag, safe=""))["sha"] == plan["revision"], "source or Go tag moved")
    comparison = api(f"compare/{plan['revision']}...main")
    require(comparison["status"] in {"ahead", "identical"} and
            comparison["base_commit"]["sha"] == plan["revision"], "source revision is not on main")
    run = api(f"actions/runs/{plan['run_id']}/attempts/{plan['attempt']}")
    require(run["id"] == plan["run_id"] and run["run_attempt"] == plan["attempt"] and
            run["head_sha"] == plan["revision"] and run["head_branch"] == "main" and
            run["path"] == ".github/workflows/release.yml" and
            run["repository"]["id"] == repository["id"] and
            run["event"] in {"push", "workflow_dispatch"} and
            run["status"] == "completed" and run["conclusion"] == "success",
            "producing workflow run/attempt is not the pinned successful main release")
    assets = {}
    for asset in release["assets"]:
        require(asset["name"] not in assets, "duplicate release asset name")
        assets[asset["name"]] = asset
    require(bundle_name(plan) in assets and "SHA256SUMS" in assets, "release lacks crate bundle/checksums")
    require(assets[bundle_name(plan)]["digest"] == "sha256:" + plan["bundle_sha256"],
            "GitHub asset digest differs from reviewed decision")
    # Compare only trust-relevant fields. Public counters and other mutable
    # repository/asset metadata can change while verification runs.
    identity = {key: repository[key] for key in ("id", "full_name", "default_branch", "archived")}
    selected = {name: {key: assets[name][key] for key in ("id", "name", "digest", "size", "state")}
                for name in (bundle_name(plan), "SHA256SUMS")}
    require(all(asset["state"] == "uploaded" for asset in selected.values()), "release asset is incomplete")
    return identity, selected


def verify_attestation(result, plan, repository_id):
    expected = {"subjectAlternativeName": WORKFLOW,
                "issuer": "https://token.actions.githubusercontent.com",
                "sourceRepositoryURI": REPO_URL,
                "sourceRepositoryDigest": plan["revision"],
                "sourceRepositoryRef": "refs/heads/main",
                "sourceRepositoryIdentifier": str(repository_id),
                "buildConfigURI": WORKFLOW,
                "buildConfigDigest": plan["revision"],
                "runnerEnvironment": "github-hosted",
                "runInvocationURI": f"{REPO_URL}/actions/runs/{plan['run_id']}/attempts/{plan['attempt']}"}
    for item in result:
        verified = item["verificationResult"]
        certificate = verified["signature"]["certificate"]
        statement = verified["statement"]
        if (all(certificate.get(key) == value for key, value in expected.items()) and
                statement["predicateType"] == "https://slsa.dev/provenance/v1" and
                any(subject.get("digest", {}).get("sha256") == plan["bundle_sha256"]
                    for subject in statement["subject"])):
            return
    raise Refusal("no verified certificate binds this bundle to the exact producing run/attempt")


def verify_bundle(data, plan):
    spec = importlib.util.spec_from_file_location("world_release_candidates", ROOT / "tools/release/crate_candidates.py")
    verifier = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(verifier)
    verifier.verify(data, plan["revision"])
    # Only parse bytes after the canonical byte-only validator has accepted them.
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        files = {item.name: archive.extractfile(item).read() for item in archive}
    require(sha256(files["candidate.json"]) == plan["manifest_sha256"], "candidate manifest differs from decision")
    manifest, archives = verifier.CANDIDATES.verify_files(files)
    return manifest, files, archives


def registry_state(row):
    name, version, expected = row["name"], row["version"], row["sha256"]
    status, body = request("crates.io", f"/api/v1/crates/{name}")
    index_status, index_body = request("index.crates.io", f"/{name[:2]}/{name[2:4]}/{name}")
    require((status == 404) == (index_status == 404), "registry API and index disagree; wait and investigate")
    if status == 404:
        return "absent"
    metadata = decode(body)
    require(metadata["crate"]["name"] == name, "registry normalized to another crate name")
    versions = metadata["versions"]
    index = [decode(line) for line in index_body.splitlines() if line]
    require(len(versions) == 1 and len(index) == 1, "existing name is outside first-publication scope")
    require(versions[0]["num"] == version and versions[0]["checksum"] == expected and
            versions[0]["yanked"] is False and index[0]["name"] == name and
            index[0]["vers"] == version and index[0]["cksum"] == expected and
            index[0]["yanked"] is False, "existing coordinate conflicts with verified candidate")
    download_status, download = request("static.crates.io", f"/crates/{name}/{name}-{version}.crate",
                                        limit=128 * 1024 * 1024)
    require(download_status == 200 and sha256(download) == expected, "registry download differs from candidate")
    return "exact-existing"


def preflight(plan, bundle, checksums):
    pins(plan)
    require(not any(value and key.startswith("CARGO_REGISTR") for key, value in os.environ.items()),
            "remove registry credential/configuration environment variables")
    clean_source(plan["revision"])
    data = bounded_file(bundle, MAX_BUNDLE)
    require(sha256(data) == plan["bundle_sha256"], "bundle differs from reviewed decision")
    sums = bounded_file(checksums, MAX_JSON)
    repository, assets = release_evidence(plan)
    require(assets["SHA256SUMS"]["digest"] == "sha256:" + sha256(sums), "checksum asset digest differs")
    entries = [line.split() for line in sums.decode("ascii").splitlines()]
    require(all(len(entry) == 2 and re.fullmatch(r"[0-9a-f]{64}", entry[0]) for entry in entries),
            "invalid checksum inventory")
    require(sum(entry[1] == bundle_name(plan) for entry in entries) == 1 and
            [plan["bundle_sha256"], bundle_name(plan)] in entries, "bundle checksum missing or duplicated")
    # Snapshot the bytes: subsequent changes to owner input paths cannot affect verification.
    with tempfile.TemporaryDirectory(prefix="world-owner-preflight-") as directory:
        snapshot = Path(directory) / bundle_name(plan)
        snapshot.write_bytes(data)
        command(["gh", "release", "verify-asset", plan["tag"], str(snapshot), "--repo", REPOSITORY])
        result = decode(command(["gh", "attestation", "verify", str(snapshot), "--repo", REPOSITORY,
                                 "--cert-identity", WORKFLOW, "--source-ref", "refs/heads/main",
                                 "--source-digest", plan["revision"], "--deny-self-hosted-runners", "--format", "json"]))
        verify_attestation(result, plan, repository["id"])
    manifest, files, archives = verify_bundle(data, plan)
    states = {row["name"]: registry_state(row) for row in manifest["packages"]}
    # Detect live source/release drift after the potentially slow verification work.
    require(release_evidence(plan) == (repository, assets), "release evidence changed during preflight")
    clean_source(plan["revision"])
    return manifest, files, archives, states


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--decision", required=True, type=Path)
    parser.add_argument("--bundle", required=True, type=Path)
    parser.add_argument("--checksums", required=True, type=Path)
    args = parser.parse_args(argv)
    try:
        plan = decode(bounded_file(args.decision, MAX_JSON))
        manifest, _, _, states = preflight(plan, args.bundle, args.checksums)
        print(json.dumps({"release": plan, "coordinates": [
            {"name": row["name"], "version": row["version"], "sha256": row["sha256"], "state": states[row["name"]]}
            for row in manifest["packages"]], "publication_authorized": False}, indent=2))
    except Refusal as error:
        parser.exit(1, f"owner publication: {error}\n")
    except (ValueError, KeyError, TypeError, OSError, tarfile.TarError):
        parser.exit(1, "owner publication: malformed or unavailable evidence; no publication attempted\n")


if __name__ == "__main__":
    main()
