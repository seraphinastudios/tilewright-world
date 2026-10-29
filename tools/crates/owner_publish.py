#!/usr/bin/env python3
"""Preflight one explicitly pinned source release for owner first publication.

No registry credentials are read by preflight. No Cargo commands are executed.
"""
from __future__ import annotations

import argparse
import hashlib
import getpass
import http.client
import importlib.util
import io
import json
import os
from pathlib import Path
import re
import subprocess
import struct
import sys
import tarfile
import tempfile
import time
import tomllib
import warnings
from urllib.parse import quote

ROOT = Path(__file__).resolve().parents[2]
REPOSITORY = "seraphinastudios/tilewright-world"
REPOSITORY_ID = 1423814231
OWNER = "zoeyrose"
OWNER_ID = 437663
GITHUB_OWNER_ID = 3865595
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
        raise Refusal("public service request failed; preserve any upload intents") from None
    finally:
        connection.close()


def api(path):
    status, data = request("api.github.com", f"/repos/{REPOSITORY}" + (f"/{path}" if path else ""))
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


def release_evidence(plan, *, read=None):
    read = read or api
    repository = read("")
    require(repository["full_name"] == REPOSITORY and repository["default_branch"] == "main" and
            repository["archived"] is False and repository["id"] == REPOSITORY_ID, "repository identity or default branch changed")
    release = read(f"releases/{plan['release_id']}")
    require(release["id"] == plan["release_id"] and release["tag_name"] == plan["tag"] and
            release["immutable"] is True and release["draft"] is False and
            release["prerelease"] is False, "release must be the pinned immutable stable publication")
    for tag in (plan["tag"], "go/" + plan["tag"]):
        require(read("commits/" + quote(tag, safe=""))["sha"] == plan["revision"], "source or Go tag moved")
    comparison = read(f"compare/{plan['revision']}...main")
    require(comparison["status"] in {"ahead", "identical"} and
            comparison["base_commit"]["sha"] == plan["revision"], "source revision is not on main")
    run = read(f"actions/runs/{plan['run_id']}/attempts/{plan['attempt']}")
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


def public_owner():
    status, data = request("crates.io", f"/api/v1/users/{OWNER}")
    require(status == 200, "intended public owner is unavailable")
    user = decode(data)["user"]
    require(user["login"] == OWNER and user["id"] == OWNER_ID, "intended public owner identity differs")


def owned(row):
    status, data = request("crates.io", f"/api/v1/crates/{row['name']}/owners")
    require(status == 200, "published ownership is unavailable; stop the batch")
    users = decode(data)["users"]
    require(any(user.get("kind") == "user" and user.get("login") == OWNER and
                user.get("id") == OWNER_ID for user in users), "published owner mismatch; stop the batch")


def terminal_boundary():
    require(os.name == "posix" and sys.stdin.isatty() and sys.stderr.isatty(),
            "execution requires the owner's interactive POSIX terminal")
    forbidden = ("CI", "GITHUB_ACTIONS", "ACTIONS_ID_TOKEN_REQUEST_URL", "ACTIONS_ID_TOKEN_REQUEST_TOKEN")
    require(not any(value and (key in forbidden or key.startswith("CARGO_REGISTR") or
                    ("TOKEN" in key.upper() and any(word in key.upper() for word in ("CRATE", "CARGO"))))
                    for key, value in os.environ.items()),
            "execution refuses CI and registry credential environment variables")


def github_call(path, body=None):
    """Only owner execution calls this; registry tokens never enter this subprocess."""
    arguments = ["gh", "api", "--hostname", "github.com", "--method", "POST" if body else "GET", path]
    options = {"stdin": subprocess.DEVNULL}
    if body is not None:
        arguments += ["--input", "-"]
        options = {"input": json.dumps(body, sort_keys=True).encode()}
    # No registry credential is in the environment; the hidden token is a local
    # Python value. GitHub CLI uses its existing owner-managed authentication.
    try:
        result = subprocess.run(arguments, cwd=ROOT, capture_output=True, timeout=60,
                                check=False, **options)
    except (OSError, subprocess.TimeoutExpired):
        raise Refusal("GitHub operation unresolved; preserve any intent and do not retry uploads") from None
    require(result.returncode == 0 and len(result.stdout) <= MAX_JSON,
            "GitHub operation refused or unresolved; preserve any intent and do not retry uploads")
    return decode(result.stdout)


def owner_api(path):
    """Execution-only authenticated evidence reads avoid the public hourly quota."""
    return github_call(f"repos/{REPOSITORY}" + (f"/{path}" if path else ""))


def github_authority():
    actor = github_call("user")
    require(actor["login"] == OWNER and actor["id"] == GITHUB_OWNER_ID, "wrong GitHub owner identity")
    repository = github_call(f"repos/{REPOSITORY}")
    require(repository["id"] == REPOSITORY_ID and repository["full_name"] == REPOSITORY and
            repository["default_branch"] == "main" and repository["archived"] is False and
            repository["permissions"]["push"] is True, "GitHub owner lacks intended repository authority")


def intent_name(row):
    return f"crate-upload-intent/{row['name']}/{row['version']}"


def require_no_intent(row):
    status, _ = request("api.github.com", f"/repos/{REPOSITORY}/git/ref/tags/{intent_name(row)}")
    require(status == 404, "upload intent already exists; public absence never permits a retry")


def reserve_intent(plan, row, metadata_digest):
    """Create-only global fence; no update, reset, delete or retry operation exists."""
    require_no_intent(row)
    github_authority()
    record = {"schema": 1, "kind": "world-first-crate-upload", "repository_id": REPOSITORY_ID,
              "release": plan, "coordinate": {"name": row["name"], "version": row["version"],
                                               "sha256": row["sha256"], "metadata_sha256": metadata_digest},
              "owner": {"login": OWNER, "registry_id": OWNER_ID, "github_id": GITHUB_OWNER_ID}}
    tag = github_call(f"repos/{REPOSITORY}/git/tags", {
        "tag": intent_name(row), "message": json.dumps(record, sort_keys=True),
        "object": plan["revision"], "type": "commit"})
    require(re.fullmatch(r"[0-9a-f]{40}", tag["sha"]), "invalid intent object response")
    ref = "refs/tags/" + intent_name(row)
    # GitHub's create-reference endpoint atomically rejects an existing ref.
    # Even two processes creating the same tag object cannot both win this step.
    created = github_call(f"repos/{REPOSITORY}/git/refs", {"ref": ref, "sha": tag["sha"]})
    require(created["ref"] == ref and created["object"]["type"] == "tag" and
            created["object"]["sha"] == tag["sha"], "intent creation response unresolved")
    observed = github_call(f"repos/{REPOSITORY}/git/ref/tags/{intent_name(row)}")
    require(observed["ref"] == ref and observed["object"] == created["object"], "intent changed before upload")
    return tag["sha"]


def upload_metadata(files):
    """Map Cargo's normalized manifest to the documented registry API, without Cargo."""
    manifest = tomllib.loads(files["Cargo.toml"].decode())
    package = manifest["package"]
    dependencies = []
    tables = [(None, manifest), *sorted(manifest.get("target", {}).items())]
    for target, table in tables:
        for section, kind in (("dependencies", "normal"), ("dev-dependencies", "dev"), ("build-dependencies", "build")):
            for alias, specification in sorted(table.get(section, {}).items()):
                specification = {"version": specification} if isinstance(specification, str) else specification
                require(set(specification) <= {"version", "features", "optional", "default-features", "package"}
                        and isinstance(specification.get("version"), str), "unsupported upload dependency metadata")
                dependencies.append({"name": specification.get("package", alias), "version_req": specification["version"],
                                     "features": specification.get("features", []), "optional": specification.get("optional", False),
                                     "default_features": specification.get("default-features", True), "target": target,
                                     "kind": kind, "registry": None,
                                     "explicit_name_in_toml": alias if "package" in specification else None})
    readme = package.get("readme")
    require(readme is None or readme is False or isinstance(readme, str), "unsupported readme metadata")
    require(not readme or readme in files, "packaged readme is missing")
    result = {"name": package["name"], "vers": package["version"], "deps": dependencies,
              "features": manifest.get("features", {}), "authors": package.get("authors", []),
              "keywords": package.get("keywords", []), "categories": package.get("categories", []),
              "readme": files[readme].decode() if readme else None, "readme_file": readme or None,
              "license_file": package.get("license-file"), "rust_version": package.get("rust-version"),
              "badges": manifest.get("badges", {})}
    for key in ("description", "documentation", "homepage", "license", "repository", "links"):
        result[key] = package.get(key)
    return result


def publication_order(manifest, archives):
    rows = manifest["packages"]
    metadata = {row["name"]: upload_metadata(archives[row["name"]]) for row in rows}
    by_name = {row["name"]: row for row in rows}
    dependencies = {}
    for name, meta in metadata.items():
        dependencies[name] = set()
        require(meta["name"] == name and meta["vers"] == by_name[name]["version"], "upload metadata identity differs")
        for dependency in meta["deps"]:
            if dependency["name"] in by_name:
                require(dependency["version_req"] == "=" + by_name[dependency["name"]]["version"],
                        "internal upload dependency is not exact")
                dependencies[name].add(dependency["name"])
    ordered = []
    remaining = list(by_name)
    while remaining:
        ready = [name for name in remaining if dependencies[name] <= {row["name"] for row in ordered}]
        require(ready, "internal publication dependencies are cyclic")
        for name in ready:
            remaining.remove(name)
            ordered.append(by_name[name])
    return ordered, metadata, dependencies


def put_once(data, metadata, token):
    encoded = json.dumps(metadata, sort_keys=True, separators=(",", ":")).encode()
    require(len(encoded) <= MAX_JSON and len(data) <= 128 * 1024 * 1024, "upload payload exceeds limit")
    body = struct.pack("<I", len(encoded)) + encoded + struct.pack("<I", len(data)) + data
    connection = http.client.HTTPSConnection("crates.io", timeout=60)
    try:
        connection.request("PUT", "/api/v1/crates/new", body=body, headers={
            "Authorization": token, "Content-Type": "application/octet-stream", "Accept": "application/json",
            "User-Agent": "tilewright-world-owner-publication/1 (github.com/seraphinastudios/tilewright-world)"})
        response = connection.getresponse()
        require(response.status == 200, "upload outcome unresolved; preserve intent and do not retry")
        reply = response.read(MAX_JSON + 1)
        require(len(reply) <= MAX_JSON, "upload response exceeds limit; preserve intent")
        result = decode(reply)
        require(isinstance(result, dict) and not result.get("errors"), "registry refused upload; preserve intent")
        # Never print server-controlled messages: a server could echo the token.
    except (OSError, http.client.HTTPException, ValueError):
        raise Refusal("upload outcome unresolved; preserve intent and do not retry") from None
    finally:
        connection.close()


def confirm_publication(row):
    # Read-only polling after a confirmed HTTP success, never a second PUT.
    for attempt in range(12):
        if registry_state(row) == "exact-existing":
            owned(row)
            return
        if attempt < 11:
            time.sleep(5)
    raise Refusal("publication is not yet visible; preserve intent and use read-only preflight later")


def execute(plan, manifest, files, archives, states):
    terminal_boundary()
    public_owner()
    github_authority()
    ordered, metadata, dependencies = publication_order(manifest, archives)
    by_name = {row["name"]: row for row in ordered}
    pending = []
    for row in ordered:
        if states[row["name"]] == "exact-existing":
            owned(row)
        else:
            require_no_intent(row)
            pending.append(row)
    if not pending:
        print("All five exact archives and intended public ownership are already confirmed; no upload needed.")
        return
    snapshot = release_evidence(plan, read=owner_api)
    print("First publication: " + ", ".join(f"{row['name']}@{row['version']}" for row in pending), file=sys.stderr)
    print(f"Release {plan['tag']} ({plan['release_id']}), source {plan['revision']}, bundle {plan['bundle_sha256']}", file=sys.stderr)
    print(f"Intended crates.io owner: {OWNER} ({OWNER_ID}). No token identity API exists.", file=sys.stderr)
    print("Your confirmation asserts this token was created in that account. Ownership is checked AFTER publication.", file=sys.stderr)
    require(input(f"Type PUBLISH AS {OWNER}: ") == f"PUBLISH AS {OWNER}", "owner token-account assertion not confirmed")
    # Prevent an OS core dump retaining terminal credentials. Python strings
    # cannot promise secure erasure; use a short-lived token and revoke it after use.
    import resource
    resource.setrlimit(resource.RLIMIT_CORE, (0, 0))
    with warnings.catch_warnings():
        warnings.simplefilter("error", getpass.GetPassWarning)
        try:
            token = getpass.getpass("One-use crates.io publish-new token (hidden): ")
        except getpass.GetPassWarning:
            raise Refusal("secure hidden token input unavailable") from None
    require(0 < len(token) <= 8192 and all(33 <= ord(char) <= 126 for char in token), "invalid token input")
    try:
        for row in pending:
            terminal_boundary()
            clean_source(plan["revision"])
            require(release_evidence(plan, read=owner_api) == snapshot, "source release changed before upload")
            public_owner()
            for dependency in dependencies[row["name"]]:
                require(registry_state(by_name[dependency]) == "exact-existing", "dependency is not visible with its exact checksum")
                owned(by_name[dependency])
            require(registry_state(row) == "absent", "coordinate is no longer absent; stop and re-run preflight")
            data = files[row["asset"]]
            require(sha256(data) == row["sha256"], "held archive changed")
            meta = metadata[row["name"]]
            meta_bytes = json.dumps(meta, sort_keys=True, separators=(",", ":")).encode()
            reserve_intent(plan, row, sha256(meta_bytes))
            # An intent is durable before any possible PUT. A later refusal leaves
            # it intact; no process/host may adopt or retry that reservation.
            require(release_evidence(plan, read=owner_api) == snapshot, "source release changed after intent; stop")
            clean_source(plan["revision"])
            require(registry_state(row) == "absent", "coordinate changed after intent; stop")
            put_once(data, meta, token)
            confirm_publication(row)
            print(f"Confirmed {row['name']}@{row['version']} exact bytes and intended owner.")
    finally:
        token = None
        print("Revoke the one-use token in the owner account. Preserve every upload intent.", file=sys.stderr)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--execute", action="store_true", help="owner terminal only: explicitly publish the verified initial batch")
    parser.add_argument("--decision", required=True, type=Path)
    parser.add_argument("--bundle", required=True, type=Path)
    parser.add_argument("--checksums", required=True, type=Path)
    args = parser.parse_args(argv)
    try:
        plan = decode(bounded_file(args.decision, MAX_JSON))
        if args.execute:
            terminal_boundary()
        manifest, files, archives, states = preflight(plan, args.bundle, args.checksums)
        if args.execute:
            execute(plan, manifest, files, archives, states)
            return
        print(json.dumps({"release": plan, "coordinates": [
            {"name": row["name"], "version": row["version"], "sha256": row["sha256"], "state": states[row["name"]]}
            for row in manifest["packages"]], "publication_authorized": False}, indent=2))
    except (KeyboardInterrupt, EOFError):
        parser.exit(1, "owner publication: cancelled; preserve any existing upload intents\n")
    except Refusal as error:
        parser.exit(1, f"owner publication: {error}\n")
    except (ValueError, KeyError, TypeError, OSError, tarfile.TarError):
        parser.exit(1, "owner publication: malformed or unavailable evidence; preserve any upload intents\n")


if __name__ == "__main__":
    main()
