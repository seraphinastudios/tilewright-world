#!/usr/bin/env python3
"""Publish the exact crate archives of the release that owns a crate version.

The first release from main at a workspace crate version owns that version
for all five public crates, and only its attested archives go to crates.io.
The release workflow's plan job runs `owner`; the "Publish crates" workflow
(.github/workflows/publish-crate.yml) runs the rest from main, reading the
release's own policy files from its tree as data:

  owner        which release owns the crate version at a revision (git only)
  verify       authenticate a release's bundle and plan its upload
  upload       upload the planned archives, at most once per attempt record
  confirm      check all five crates on crates.io
  consumer     write a crates.io consumer of the release's crate version
  accept-lock  check that consumer's lockfile against the release's archives

Standard library only. No Cargo, package or build code runs here. Messages
are static or built from validated values, never from server responses.
docs/RELEASING.md describes the procedure.
"""
from __future__ import annotations

import argparse
import base64
import hashlib
import http.client
import importlib.util
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import sys
import tarfile
import tempfile
import time
import tomllib
import typing
import urllib.error
import urllib.parse
import urllib.request

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[2]
REPOSITORY = "seraphinastudios/tilewright-world"
REPOSITORY_ID = 1423814231
REPO_URL = f"https://github.com/{REPOSITORY}"
# The release workflow that signs the bundle's provenance, and the publisher.
SIGNER = f"{REPO_URL}/.github/workflows/release.yml@refs/heads/main"
WORKFLOW_REF = f"{REPOSITORY}/.github/workflows/publish-crate.yml@refs/heads/main"
OWNER, OWNER_ID = "zoeyrose", 437663
VERSION = re.compile(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")
RELEASE = re.compile(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)")
SHA = re.compile(r"[0-9a-f]{40}")
DIGEST = re.compile(r"[0-9a-f]{64}")
RUN = re.compile(re.escape(REPO_URL) + r"/actions/runs/([1-9][0-9]{0,19})/attempts/([1-9][0-9]{0,4})")
SLSA = "https://slsa.dev/provenance/v1"
CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
USER_AGENT = "tilewright-world-publish-crates/1 (https://github.com/seraphinastudios/tilewright-world)"
HOSTS = {"api.github.com", "github.com", "crates.io", "index.crates.io", "static.crates.io"}
DOWNLOAD_HOSTS = {"github.com", "release-assets.githubusercontent.com", "objects.githubusercontent.com"}
MAX_JSON = 4 * 1024 * 1024
MAX_ARCHIVE = 128 * 1024 * 1024
MAX_ATTEMPTS = 3
POLLS, POLL_SECONDS = 12, 10
ATTESTATION_TRIES, ATTESTATION_SECONDS = 6, 20
# Records of uploads: the first publication's intents (read for the
# cross-check only), then one attempt record before each PUT and a refused
# record once attempt N is known not to have published: it was never sent,
# or crates.io refused it definitely (401, 403 or 429).
LEGACY, ATTEMPT, REFUSED = "crate-upload-intent", "crate-upload-attempt", "crate-upload-refused"
LEGACY_KIND, ATTEMPT_KIND, REFUSED_KIND = "world-first-crate-upload", "world-crate-upload", "world-crate-upload-refused"
# Why an attempt is known not to have published: its PUT was never sent, or
# crates.io refused it definitely (see PutOutcome).
REFUSED_REASONS = ("not-sent", "http-401", "http-403", "http-429")
RECORD_TRIES, RECORD_SECONDS = 3, 5


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


# Loaded from this file's own checkout (main), never from a release's tree.
CANDIDATES = load("world_release_candidates", ROOT / "tools/release/crate_candidates.py")
PREPARE = CANDIDATES.CANDIDATES
FIRST = load("world_owner_publish", ROOT / "tools/crates/owner_publish.py")


class Refusal(ValueError):
    """A static, stage-specific refusal, safe to print."""


class Backward(Refusal):
    """The crate version moves backward: the one error that stops a release."""


class Failure(Refusal):
    """A request that failed; `status` is its HTTP status, or None."""

    def __init__(self, service, status):
        super().__init__(f"{service} answered HTTP {status}" if status else f"{service} could not be reached")
        self.status = status


def require(ok, message):
    if not ok:
        raise Refusal(message)


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode()


def decode(data):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, "a response repeats a JSON field")
            result[key] = value
        return result
    return json.loads(data, object_pairs_hook=unique)


def semver(value):
    match = VERSION.fullmatch(value) if isinstance(value, str) else None
    require(match, "a crate version is not a release version")
    return tuple(int(part) for part in match.groups())


def summary(lines):
    path = os.environ.get("GITHUB_STEP_SUMMARY")
    if path:
        with open(path, "a", encoding="utf-8") as stream:
            stream.write("\n".join(lines) + "\n\n")


def outputs(values):
    path = os.environ.get("GITHUB_OUTPUT")
    require(path, "GITHUB_OUTPUT is not set")
    with open(path, "a", encoding="utf-8") as stream:
        for key, value in values.items():
            require("\n" not in str(value) and "\r" not in str(value), "an output holds a line break")
            stream.write(f"{key}={value}\n")


# --- Git, and which release owns a crate version ---------------------------

def git(arguments, root=ROOT, *, text=True):
    try:
        result = subprocess.run(["git", *arguments], cwd=root, stdin=subprocess.DEVNULL,
                                capture_output=True, timeout=120, check=False)
    except (OSError, subprocess.TimeoutExpired):
        raise Refusal("git could not run") from None
    require(result.returncode == 0, f"git {arguments[0]} failed")
    return result.stdout.decode() if text else result.stdout


def crate_version_at(ref, root=ROOT):
    try:
        value = tomllib.loads(git(["cat-file", "blob", f"{ref}:Cargo.toml"], root))["workspace"]["package"]["version"]
    except (tomllib.TOMLDecodeError, KeyError, TypeError, UnicodeDecodeError):
        raise Refusal(f"{ref} has no workspace crate version") from None
    require(isinstance(value, str) and VERSION.fullmatch(value), f"the crate version at {ref} is not a release version")
    return value


def changed_crates(base, revision, root=ROOT):
    """The public crates whose packaged content changed between two commits.

    A report, not a gate: each crate's files outside tests/ and benches/
    (its Cargo.toml included); what it inherits from the workspace and Cargo
    writes into its packaged Cargo.toml (dependency requirements, package
    fields such as rust-version and edition, lints); and the root README.md
    and LICENSE, which every crate packages. Cargo.lock and
    rust-toolchain.toml do not reach a library's users.
    """
    paths = set(git(["diff", "--name-only", "--no-renames", "-z", base, revision, "--"], root).split("\0")) - {""}
    shared = bool(paths & {"README.md", "LICENSE"})

    def workspace(ref):
        return tomllib.loads(git(["cat-file", "blob", f"{ref}:Cargo.toml"], root))["workspace"]

    def moved(section):
        old, new = before.get(section, {}), after.get(section, {})
        return {key for key in old.keys() | new.keys() if old.get(key) != new.get(key)}

    def inherits(value):
        return isinstance(value, dict) and value.get("workspace") is True

    before, after = workspace(base), workspace(revision)
    # The version is the same, or this release would own its own.
    requirements, fields = moved("dependencies"), moved("package") - {"version"}
    lints = before.get("lints") != after.get("lints")
    changed = []
    for name in PREPARE.PUBLIC:
        prefix = f"crates/{name}/"
        files = [path for path in paths if path.startswith(prefix) and
                 not path.startswith((prefix + "tests/", prefix + "benches/"))]
        manifest = tomllib.loads(git(["cat-file", "blob", f"{revision}:{prefix}Cargo.toml"], root))
        inherited = set()
        for table in [manifest, *manifest.get("target", {}).values()]:
            for section in ("dependencies", "build-dependencies", "dev-dependencies"):
                inherited |= {key for key, value in table.get(section, {}).items() if inherits(value)}
        package = {key for key, value in manifest.get("package", {}).items() if inherits(value)}
        if (shared or files or inherited & requirements or package & fields or
                (lints and inherits(manifest.get("lints")))):
            changed.append(name)
    return changed


def ownership(revision, tag=None, root=ROOT):
    """Which release owns the workspace crate version at `revision`, from git tags.

    The previous release is the highest release tag merged into the revision
    and not pointing at it. The revision's release owns version V when there
    is none or its crate version is lower than V; otherwise the first release
    at V owns it. A version lower than the previous release's is Backward.
    """
    require(SHA.fullmatch(revision or ""), "the revision must be a full commit SHA")
    require(tag is None or RELEASE.fullmatch(tag), "the release tag must be vX.Y.Z")
    merged = [line for line in git(["tag", "--merged", revision, "--list", "v*"], root).split()
              if RELEASE.fullmatch(line)]
    here = [line for line in git(["tag", "--points-at", revision, "--list", "v*"], root).split()
            if RELEASE.fullmatch(line)]
    require(len(here) <= 1, "more than one release tag points at the revision")
    require(tag is None or set(here) <= {tag}, "another release tag points at the revision")
    previous = sorted((line for line in merged if line not in here), key=lambda line: semver(line[1:]))
    version = crate_version_at(revision, root)
    history = [(line, crate_version_at(line, root)) for line in previous]
    for (_, earlier), (later, value) in zip(history, history[1:]):
        require(semver(value) >= semver(earlier), f"the crate version moved backward at {later}")
    previous_tag, previous_version = history[-1] if history else (None, None)
    if previous_version is not None and semver(version) < semver(previous_version):
        raise Backward(f"the crate version moves backward, from {previous_version} in {previous_tag} to {version}")
    owner = previous_version is None or semver(version) > semver(previous_version)
    owner_tag = tag if owner else next(line for line, value in history if value == version)
    return {"crate_version": version, "previous_tag": previous_tag, "previous_crate_version": previous_version,
            "owner": owner, "owner_tag": owner_tag, "tags_at_revision": here,
            "changed": [] if owner else changed_crates(owner_tag, revision, root)}


def owner_summary(result, tag, enabled):
    version = result["crate_version"]
    if result["owner"]:
        who = f"{tag} is the first release at crate version {version}, so it owns it." if tag else (
            f"Merging this pull request makes its release the first at crate version {version}, which owns it.")
        how = ("Once it is released, \"Start crate publication\" starts the \"Publish crates\" workflow, which "
               "uploads its archives to crates.io." if enabled else
               "crates.io publication is off (the repository variable TILEWRIGHT_CRATES_PUBLISH is not true), so "
               "\"Start crate publication\" prints the command that would start it.")
        return [who, how]
    lines = [f"Crate version {version} is owned by {result['owner_tag']}, the first release at it; crates.io gets "
             "that release's archives, not this one's."]
    if result["changed"]:
        lines.append(f"Changed since {result['owner_tag']}, so on crates.io only with the next crate version: "
                     + ", ".join(result["changed"]) + ".")
    return lines


def owner_command(args):
    """The plan job's step. Only a proven backward move fails it."""
    enabled = os.environ.get("TILEWRIGHT_CRATES_PUBLISH") == "true"
    tag = None if args.pull_request else args.tag
    try:
        require(args.pull_request or RELEASE.fullmatch(args.tag or ""), "the release tag must be vX.Y.Z")
        require(not args.previous_tag or RELEASE.fullmatch(args.previous_tag), "the previous tag must be vX.Y.Z")
        result = ownership(args.revision, tag)
        require(not result["tags_at_revision"], "a release tag already points at the revision")
        require((result["previous_tag"] or "") == args.previous_tag, "the previous release tag differs from the plan's")
    except Backward as error:
        print(f"::error title=Crate version::{error}. Release a higher crate version instead; see "
              "\"Crate versions\" in docs/RELEASING.md.")
        summary(["### Crate version", f"Stopped: {error}."])
        return 1
    except Exception as error:  # Anything else must not hold up the release.
        message = str(error) if isinstance(error, Refusal) else f"an unexpected {type(error).__name__}"
        print(f"::warning title=Crate version::could not decide crate version ownership: {message}. "
              "The release goes ahead, and \"Start crate publication\" fails after it.")
        summary(["### Crate version", f"Could not decide crate version ownership: {message}."])
        if args.github_output:
            outputs({"crate_version": "", "crate_owner": "error", "crate_owner_tag": "", "crate_changed": ""})
        return 0
    if args.github_output:
        outputs({"crate_version": result["crate_version"], "crate_owner": str(result["owner"]).lower(),
                 "crate_owner_tag": result["owner_tag"] or "", "crate_changed": " ".join(result["changed"])})
    summary(["### Crate version", *owner_summary(result, tag, enabled)])
    print(json.dumps(result, indent=2))
    return 0


# --- The publication boundary ---------------------------------------------

def dispatch_inputs():
    values = {name: os.environ.get(name, "") for name in
              ("OPERATION", "RELEASE_TAG", "SOURCE_REVISION", "BUNDLE_SHA256")}
    require(values["OPERATION"] in ("verify", "publish") and RELEASE.fullmatch(values["RELEASE_TAG"]) and
            SHA.fullmatch(values["SOURCE_REVISION"]) and DIGEST.fullmatch(values["BUNDLE_SHA256"]),
            "the dispatch inputs are malformed")
    return values


def boundary(*, upload=False, root=ROOT):
    """Run only as main's publish-crate.yml, from a clean checkout of it.

    Only the upload step may hold the crates.io token or the intent token,
    and it holds no other GitHub token.
    """
    require(not any(os.environ.get(name) for name in
                    ("CARGO_REGISTRIES_CRATES_IO_TOKEN", "CARGO_REGISTRY_BOOTSTRAP_TOKEN")),
            "an unexpected registry credential is set")
    if upload:
        require(not any(os.environ.get(name) for name in ("GH_TOKEN", "GITHUB_TOKEN", "GH_READ_TOKEN")),
                "the upload step holds a GitHub token besides the intent token")
    else:
        require(not os.environ.get("CARGO_REGISTRY_TOKEN") and not os.environ.get("GH_INTENT_TOKEN"),
                "a publication credential is set outside the upload step")
    expected = {"GITHUB_ACTIONS": "true", "GITHUB_REPOSITORY": REPOSITORY,
                "GITHUB_REPOSITORY_ID": str(REPOSITORY_ID), "GITHUB_REF": "refs/heads/main",
                "GITHUB_EVENT_NAME": "workflow_dispatch", "GITHUB_WORKFLOW_REF": WORKFLOW_REF}
    require(all(os.environ.get(key) == value for key, value in expected.items()),
            "this runs only in publish-crate.yml, dispatched on main of seraphinastudios/tilewright-world")
    revision = os.environ.get("GITHUB_SHA", "")
    require(SHA.fullmatch(revision) and os.environ.get("GITHUB_WORKFLOW_SHA") == revision,
            "the workflow's revision is not the run's")
    require(git(["rev-parse", "HEAD"], root).strip() == revision, "the checkout is not the workflow's revision")
    require(not git(["status", "--porcelain", "--untracked-files=all"], root).strip(), "the checkout is not clean")


def credential(value, name):
    require(isinstance(value, str) and 0 < len(value) <= 8192 and all(33 <= ord(char) <= 126 for char in value),
            f"the {name} is missing or malformed")
    return value


# --- HTTPS: fixed hosts, no proxies, bounded, GitHub API only with a token -

class Redirects(urllib.request.HTTPRedirectHandler):
    """Follows redirects only to the given HTTPS hosts."""

    def __init__(self, hosts):
        super().__init__()
        self.hosts = hosts

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        parsed = urllib.parse.urlparse(newurl)
        if parsed.scheme != "https" or parsed.hostname not in self.hosts:
            raise Refusal("a redirect was refused")
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def request(url, *, token=None, method="GET", body=None, absent=False, maximum=MAX_JSON, expect=(200,)):
    """One bounded HTTPS request. `absent` makes a 404 (or the statuses it
    lists) answer None instead of failing."""
    parsed = urllib.parse.urlparse(url)
    require(parsed.scheme == "https" and parsed.hostname in HOSTS, "an unsupported service")
    api = parsed.hostname == "api.github.com"
    # GitHub API reads are authenticated (the anonymous hourly limit is
    # small), and the token goes nowhere else.
    require(api == bool(token), "a GitHub token goes only, and always, to the GitHub API")
    headers = {"User-Agent": USER_AGENT, "Accept": "application/json", "Cache-Control": "no-cache"}
    if api:
        headers.update({"Authorization": f"Bearer {token}", "Accept": "application/vnd.github+json",
                        "X-GitHub-Api-Version": "2022-11-28"})
    if body is not None:
        headers["Content-Type"] = "application/json"
    hosts = set() if api else DOWNLOAD_HOSTS if parsed.hostname == "github.com" else {parsed.hostname}
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), Redirects(hosts))
    service = "GitHub" if parsed.hostname.endswith("github.com") else parsed.hostname
    try:
        with opener.open(urllib.request.Request(url, data=body, method=method, headers=headers),
                         timeout=30) as response:
            require(response.status in expect, f"{service} answered an unexpected status")
            data = response.read(maximum + 1)
    except urllib.error.HTTPError as error:
        status = error.code
        error.close()
        if absent and status in ((404,) if absent is True else absent):
            return None
        raise Failure(service, status) from None
    except (urllib.error.URLError, OSError, http.client.HTTPException):
        raise Failure(service, None) from None
    require(len(data) <= maximum, f"a {service} response exceeds its size limit")
    return data


def github(suffix, token, *, absent=False, readback=False):
    """An authenticated GitHub read; only transient failures are retried."""
    url = f"https://api.github.com/repos/{REPOSITORY}" + (f"/{suffix}" if suffix else "")
    for attempt in range(3):
        try:
            data = request(url, token=token, absent=absent)
            return None if data is None else decode(data)
        except Failure as error:
            if not (error.status in (None, 502, 503, 504) or (readback and error.status == 404)) or attempt == 2:
                raise
        time.sleep(2)


def github_write(suffix, body, token):
    """One create; never retried, since its outcome may be unknown. A failure
    after GitHub answered 201 is marked `acknowledged`: the create happened."""
    data = request(f"https://api.github.com/repos/{REPOSITORY}/{suffix}", token=token, method="POST",
                   body=canonical(body), expect=(201,))
    try:
        value = decode(data)
        require(isinstance(value, dict), "GitHub's answer to a create is not an object")
    except ValueError as error:
        error.acknowledged = True
        raise
    return value


# --- The release, its bundle and its provenance ---------------------------

def snapshot(release):
    """The release's immutable identity; download counters can change."""
    identity = {key: release.get(key) for key in
                ("id", "node_id", "tag_name", "draft", "prerelease", "immutable", "published_at")}
    identity["assets"] = sorted(({key: asset.get(key) for key in
                                  ("id", "node_id", "name", "size", "digest", "state", "browser_download_url")}
                                 for asset in release.get("assets", [])),
                                key=lambda asset: (str(asset["name"]), str(asset["id"])))
    return identity


def release_evidence(values, token, root=ROOT):
    tag, revision = values["RELEASE_TAG"], values["SOURCE_REVISION"]
    repository = github("", token)
    require(repository.get("id") == REPOSITORY_ID and repository.get("full_name") == REPOSITORY and
            repository.get("default_branch") == "main" and repository.get("archived") is False,
            "the repository's identity or default branch changed")
    require(github("branches/main", token).get("protected") is True, "main is not protected")
    release = github(f"releases/tags/{tag}", token)
    require(release.get("tag_name") == tag and release.get("draft") is False and
            release.get("prerelease") is False and release.get("immutable") is True and
            release.get("published_at"), f"{tag} is not a published, immutable, stable release")
    names = [asset.get("name") for asset in release.get("assets", [])]
    require(len(names) == len(set(names)), f"{tag} has two assets of one name")
    require(all(asset.get("state") == "uploaded" for asset in release["assets"]), f"{tag} has an incomplete asset")
    for name in (tag, "go/" + tag):
        require(github("commits/" + urllib.parse.quote(name, safe=""), token).get("sha") == revision,
                f"{name} does not point at {revision}")
        require(git(["rev-parse", "--verify", "--end-of-options", f"refs/tags/{name}^{{commit}}"], root).strip()
                == revision, f"the local {name} does not point at {revision}")
    comparison = github(f"compare/{revision}...main", token)
    require(comparison.get("status") in ("ahead", "identical") and
            comparison.get("base_commit", {}).get("sha") == revision, f"{revision} is not on main")
    return release


def asset(release, name, maximum):
    rows = [row for row in release["assets"] if row.get("name") == name]
    require(len(rows) == 1, f"the release does not have one {name}")
    row = rows[0]
    url = f"https://github.com/{REPOSITORY}/releases/download/{release['tag_name']}/{name}"
    require(row.get("browser_download_url") == url and isinstance(row.get("size"), int) and
            0 < row["size"] <= maximum, f"the release's {name} has an unexpected location or size")
    data = request(url, maximum=maximum)
    require(len(data) == row["size"] and row.get("digest") == "sha256:" + sha256(data),
            f"the downloaded {name} differs from GitHub's digest")
    return data


def check_sums(sums, name, digest):
    try:
        lines = sums.decode("ascii").splitlines()
    except UnicodeDecodeError:
        raise Refusal("SHA256SUMS is not ASCII") from None
    entries = [line.split("  ") for line in lines]
    require(all(len(entry) == 2 and DIGEST.fullmatch(entry[0]) for entry in entries), "SHA256SUMS is malformed")
    require([entry for entry in entries if entry[1] == name] == [[digest, name]],
            "SHA256SUMS does not list the bundle once with its digest")


def attestations(digest, token, sleep=None):
    """The bundle's build provenance bundles, as JSON lines; read through the API."""
    query = f"attestations/sha256:{digest}?per_page=30&predicate_type=" + urllib.parse.quote(SLSA, safe="")
    for attempt in range(ATTESTATION_TRIES):
        value = github(query, token, absent=True)
        items = [] if value is None else value.get("attestations")
        require(isinstance(items, list) and len(items) <= 30, "the attestation listing is malformed")
        if items:
            require(all(isinstance(item, dict) and item.get("repository_id") == REPOSITORY_ID and
                        isinstance(item.get("bundle"), dict) for item in items),
                    "an attestation is from another repository or lacks its bundle")
            lines = b"".join(canonical(item["bundle"]) + b"\n" for item in items)
            require(len(lines) <= MAX_JSON, "the attestations exceed their size limit")
            return lines
        if attempt + 1 < ATTESTATION_TRIES:
            (sleep or time.sleep)(ATTESTATION_SECONDS)
    raise Refusal("the bundle's build provenance attestation is not readable yet; dispatch again later")


def gh(arguments, directory, token=None):
    """The runner image's GitHub CLI, with no token unless one is given."""
    environment = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": str(directory),
                   "GH_CONFIG_DIR": str(directory / "gh"), "GH_PROMPT_DISABLED": "1",
                   "GH_NO_UPDATE_NOTIFIER": "1", "NO_COLOR": "1", "LANG": "C.UTF-8"}
    if token:
        environment["GH_TOKEN"] = token
    try:
        return subprocess.run(["gh", *arguments], cwd=directory, env=environment, stdin=subprocess.DEVNULL,
                              capture_output=True, timeout=180, check=False)
    except (OSError, subprocess.TimeoutExpired):
        raise Refusal("the GitHub CLI could not run") from None


def provenance(data, name, lines, revision, digest):
    """Verify the build provenance offline, then bind its certificates.

    Every certificate must name release.yml on main at the revision, a
    GitHub-hosted runner and a release run and attempt. Two runs can attest
    one bundle (a start-over at the same commit rebuilds the same bytes), so
    the runs are recorded, not required in advance.
    """
    with tempfile.TemporaryDirectory(prefix="tilewright-world-provenance-") as temporary:
        directory = Path(temporary)
        version = gh(["version"], directory)
        found = re.match(rb"gh version ([0-9]{1,4})\.([0-9]{1,4})\.([0-9]{1,4})", version.stdout)
        require(version.returncode == 0 and found and tuple(map(int, found.groups())) >= (2, 68, 0),
                "GitHub CLI 2.68 or later is required")
        (directory / name).write_bytes(data)
        (directory / "bundles.jsonl").write_bytes(lines)
        result = gh(["attestation", "verify", "--bundle", str(directory / "bundles.jsonl"), "--repo", REPOSITORY,
                     "--cert-identity", SIGNER, "--source-ref", "refs/heads/main", "--source-digest", revision,
                     "--deny-self-hosted-runners", "--format", "json", "--", str(directory / name)], directory)
        require(result.returncode == 0 and len(result.stdout) <= MAX_JSON,
                "the bundle's build provenance did not verify")
    return producer(decode(result.stdout), revision, digest)


def producer(verified, revision, digest):
    expected = {"subjectAlternativeName": SIGNER, "issuer": "https://token.actions.githubusercontent.com",
                "sourceRepositoryURI": REPO_URL, "sourceRepositoryDigest": revision,
                "sourceRepositoryRef": "refs/heads/main", "sourceRepositoryIdentifier": str(REPOSITORY_ID),
                "buildConfigURI": SIGNER, "buildConfigDigest": revision, "runnerEnvironment": "github-hosted"}
    require(isinstance(verified, list) and 0 < len(verified) <= 30, "the verified attestations are malformed")
    runs = set()
    for item in verified:
        certificate = item["verificationResult"]["signature"]["certificate"]
        statement = item["verificationResult"]["statement"]
        require(all(str(certificate.get(key)) == value for key, value in expected.items()),
                "an attestation certificate does not name release.yml on main at the revision")
        require(statement.get("predicateType") == SLSA and
                any(subject.get("digest", {}).get("sha256") == digest for subject in statement.get("subject", [])),
                "an attestation does not cover the bundle")
        run = RUN.fullmatch(str(certificate.get("runInvocationURI")))
        require(run, "an attestation certificate does not name a release run and attempt")
        runs.add((int(run[1]), int(run[2])))
    return [f"{REPO_URL}/actions/runs/{run}/attempts/{attempt}" for run, attempt in sorted(runs)]


def release_attestation(tag, name, data, token, sleep=None):
    """GitHub's own immutable-release attestation for the bundle (needs a read token)."""
    with tempfile.TemporaryDirectory(prefix="tilewright-world-release-asset-") as temporary:
        directory = Path(temporary)
        (directory / name).write_bytes(data)
        for attempt in range(ATTESTATION_TRIES):
            result = gh(["release", "verify-asset", "--repo", REPOSITORY, "--", tag, str(directory / name)],
                        directory, token)
            if result.returncode == 0:
                return
            if attempt + 1 < ATTESTATION_TRIES:
                (sleep or time.sleep)(ATTESTATION_SECONDS)
    raise Refusal("GitHub's release attestation does not cover the bundle")


def tree(revision, directory, root=ROOT):
    """The release's own policy files, as data: main verifies older releases."""
    for path in ("Cargo.toml", "rust-toolchain.toml", "LICENSE", "tools/crates/candidates.json",
                 *(f"crates/{name}/Cargo.toml" for name in PREPARE.PUBLIC + PREPARE.PRIVATE)):
        target = directory / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(git(["cat-file", "blob", f"{revision}:{path}"], root, text=False))
    return directory


def main_abandoned(token):
    """The abandoned list on main's tip now, which a re-run of an older
    publish run would otherwise miss: its checkout is that run's main."""
    value = github("contents/tools/crates/candidates.json?ref=main", token)
    require(isinstance(value, dict) and value.get("type") == "file" and value.get("encoding") == "base64" and
            isinstance(value.get("content"), str) and isinstance(value.get("size"), int) and
            0 < value["size"] <= 64 * 1024, "main's tools/crates/candidates.json is unreadable")
    try:
        data = base64.b64decode(value["content"])
    except ValueError:
        raise Refusal("main's tools/crates/candidates.json is unreadable") from None
    require(len(data) == value["size"], "main's tools/crates/candidates.json is unreadable")
    with tempfile.TemporaryDirectory(prefix="tilewright-world-main-policy-") as temporary:
        path = Path(temporary) / "tools/crates/candidates.json"
        path.parent.mkdir(parents=True)
        path.write_bytes(data)
        return PREPARE.candidates(Path(temporary))


def candidate_bundle(data, revision, root=ROOT):
    """The byte-only verifier, against the release tree's policy. No Cargo."""
    with tempfile.TemporaryDirectory(prefix="tilewright-world-release-tree-") as temporary:
        policy = tree(revision, Path(temporary), root)
        files = CANDIDATES.verify(data, revision, policy)
        manifest, archives = PREPARE.verify_files(files, policy)
        version, _ = PREPARE.policy(policy)
    require(VERSION.fullmatch(manifest["toolchain"]), "the bundle's toolchain is not a release version")
    return manifest, files, archives, version


# --- crates.io ------------------------------------------------------------

class Registry(typing.NamedTuple):
    state: str  # absent, indeterminate or present
    version: dict | None  # the API's version row
    versions: list  # every version the sparse index lists


def registry_state(row):
    """absent (API and index lack it), indeterminate (one has it, or the CDN
    lags) or present: both have it with the archive's checksum, unyanked, the
    CDN serves the same bytes and zoeyrose owns the crate. Anything else stops."""
    name, version, expected = row["name"], row["version"], row["sha256"]
    raw = request(f"https://crates.io/api/v1/crates/{name}/{version}", absent=True)
    index = request(f"https://index.crates.io/{name[:2]}/{name[2:4]}/{name}", absent=True)
    found = None
    if raw is not None:
        found = decode(raw).get("version")
        require(isinstance(found, dict) and found.get("crate") == name and found.get("num") == version,
                f"crates.io's API answered for another coordinate than {name} {version}")
    lines = [] if index is None else [decode(line) for line in index.splitlines() if line.strip()]
    require(all(isinstance(line, dict) and line.get("name") == name and isinstance(line.get("vers"), str)
                for line in lines), f"crates.io's index entry for {name} is malformed")
    matches = [line for line in lines if line["vers"] == version]
    require(len(matches) <= 1, f"crates.io's index lists {name} {version} twice")
    for value, key in ((found, "checksum"), (matches[0] if matches else None, "cksum")):
        if value is not None:
            require(value.get(key) == expected and value.get("yanked") is False,
                    f"crates.io has {name} {version} with other bytes than the release's, or yanked")
    versions = [line["vers"] for line in lines]
    if found is None and not matches:
        return Registry("absent", None, versions)
    if found is None or not matches:
        return Registry("indeterminate", found, versions)
    # static.crates.io answers 403, not 404, for a file it does not serve.
    data = request(f"https://static.crates.io/crates/{name}/{name}-{version}.crate", absent=(403, 404),
                   maximum=MAX_ARCHIVE)
    if data is None:
        return Registry("indeterminate", found, versions)
    require(sha256(data) == expected, f"crates.io serves other bytes for {name} {version}")
    users = decode(request(f"https://crates.io/api/v1/crates/{name}/owners")).get("users")
    require(isinstance(users, list) and any(isinstance(user, dict) and user.get("kind") == "user" and
                                            user.get("login") == OWNER and user.get("id") == OWNER_ID
                                            for user in users), f"{OWNER} is not an owner of {name} on crates.io")
    return Registry("present", found, versions)


def newer(registry, version):
    """Versions on crates.io above `version`, for any of the five crates."""
    found = set()
    for state in registry.values():
        for value in state.versions:
            require(VERSION.fullmatch(value), "crates.io lists a crate version this tool does not handle")
            if semver(value) > semver(version):
                found.add(value)
    return sorted(found, key=semver)


# --- Upload records ---------------------------------------------------------

def release_record(plan):
    return {"repository": REPOSITORY, "release_id": plan["release"]["id"], "tag": plan["tag"],
            "revision": plan["revision"], "bundle_sha256": plan["bundle_sha256"],
            "manifest_sha256": plan["manifest_sha256"], "runs": plan["runs"]}


def check_record(ref, target, kind, row, values, number, token):
    """A record must be an annotated tag at the revision naming this release."""
    other = f"a record for {row['name']} {row['version']} names another release or is malformed"
    require(isinstance(target, dict) and target.get("type") == "tag" and SHA.fullmatch(str(target.get("sha"))),
            other)
    tag = github(f"git/tags/{target['sha']}", token)
    require(tag.get("sha") == target["sha"] and tag.get("tag") == ref.removeprefix("refs/tags/") and
            tag.get("object", {}).get("type") == "commit" and
            tag.get("object", {}).get("sha") == values["SOURCE_REVISION"] and isinstance(tag.get("message"), str),
            other)
    try:
        message = decode(tag["message"].strip())
    except ValueError:
        raise Refusal(other) from None
    release = message.get("release") if isinstance(message, dict) else None
    coordinate = message.get("coordinate") if isinstance(message, dict) else None
    require(isinstance(release, dict) and isinstance(coordinate, dict) and message.get("schema") == 1 and
            message.get("kind") == kind and message.get("repository_id") == REPOSITORY_ID and
            release.get("tag") == values["RELEASE_TAG"] and release.get("revision") == values["SOURCE_REVISION"] and
            release.get("bundle_sha256") == values["BUNDLE_SHA256"] and
            coordinate.get("name") == row["name"] and coordinate.get("version") == row["version"], other)
    if kind == REFUSED_KIND:
        require(message.get("attempt") == number and message.get("reason") in REFUSED_REASONS and
                message.get("attempt_ref") == f"refs/tags/{ATTEMPT}/{row['name']}/{row['version']}/{number}", other)
    else:
        require(coordinate.get("sha256") == row["sha256"] and (number is None or message.get("attempt") == number),
                other)


def records(row, values, token):
    """The coordinate's upload records, each checked to name this release."""
    name, version = row["name"], row["version"]
    found = {"legacy": False, "attempts": {}, "refused": {}}
    spaces = ((LEGACY, LEGACY_KIND, None), (ATTEMPT, ATTEMPT_KIND, "attempts"), (REFUSED, REFUSED_KIND, "refused"))
    for space, kind, key in spaces:
        base = f"refs/tags/{space}/{name}/{version}"
        listed = github(f"git/matching-refs/tags/{space}/{name}/{version}", token)
        require(isinstance(listed, list) and len(listed) <= 100, "GitHub's list of upload records is malformed")
        for item in listed:
            ref = item.get("ref") if isinstance(item, dict) else None
            if key is None:
                if ref == base:
                    check_record(ref, item.get("object"), kind, row, values, None, token)
                    found["legacy"] = True
                continue
            require(ref != base, f"a record at {base} blocks the numbered records under it")
            if not isinstance(ref, str) or not ref.startswith(base + "/"):
                continue
            number = ref[len(base) + 1:]
            require(re.fullmatch(r"[1-9]", number) and int(number) <= MAX_ATTEMPTS,
                    f"an unexpected record exists under {base}")
            check_record(ref, item.get("object"), kind, row, values, int(number), token)
            found[key][int(number)] = item["object"]["sha"]
    return found


def next_attempt(found, row):
    """The attempt an absent coordinate may make next, or a refusal."""
    coordinate = f"{row['name']} {row['version']}"
    require(not found["legacy"], f"{coordinate} has a first-publication intent but is not on crates.io; "
            "the owner investigates (docs/CRATE-FIRST-PUBLICATION.md)")
    attempts, refused = sorted(found["attempts"]), sorted(found["refused"])
    require(attempts == list(range(1, len(attempts) + 1)) and set(refused) <= set(attempts) and
            set(attempts[:-1]) <= set(refused),
            f"the upload records of {coordinate} are inconsistent; the owner investigates")
    if not attempts:
        return 1
    require(attempts[-1] in refused, f"attempt {attempts[-1]} for {coordinate} is unresolved: crates.io does not "
            "have it and no refusal was recorded; the owner investigates")
    require(len(attempts) < MAX_ATTEMPTS, f"all {MAX_ATTEMPTS} attempts for {coordinate} were refused or never "
            "sent; the owner decides what next")
    return len(attempts) + 1


def create_record(short, message, values, token):
    """Create an annotated tag at the revision and its ref, create-only.

    An exception carries the `stage` it stopped at and whether GitHub
    `acknowledged` creating the ref, so that the caller can settle it.
    """
    ref = "refs/tags/" + short
    stage, acknowledged = "existence check", False
    try:
        require(github(f"git/ref/tags/{short}", token, absent=True) is None, f"{ref} already exists")
        stage = "tag object"
        created = github_write("git/tags", {"tag": short, "message": canonical(message).decode(),
                                            "object": values["SOURCE_REVISION"], "type": "commit"}, token)
        sha = created.get("sha")
        require(isinstance(sha, str) and SHA.fullmatch(sha), "GitHub's tag object answer is malformed")
        stage = "reference"
        made = github_write("git/refs", {"ref": ref, "sha": sha}, token)
        acknowledged = True
        require(made.get("ref") == ref and made.get("object", {}).get("sha") == sha and
                made.get("object", {}).get("type") == "tag", "GitHub's reference answer differs")
        stage = "read-back"
        seen = github(f"git/ref/tags/{short}", token, readback=True)
        require(seen.get("ref") == ref and seen.get("object") == made.get("object"), "the record read back differs")
        return sha
    except BaseException as error:
        acknowledged = acknowledged or (stage == "reference" and getattr(error, "acknowledged", False))
        try:
            error.stage, error.acknowledged = stage, acknowledged
        except (AttributeError, TypeError):
            pass
        print(f"Creating {ref} stopped at its {stage}. If it exists, it stays; see \"When a crate publication "
              "fails\" in docs/RELEASING.md.", file=sys.stderr)
        raise


def publisher():
    run_id, run_attempt = os.environ.get("GITHUB_RUN_ID", ""), os.environ.get("GITHUB_RUN_ATTEMPT", "")
    require(re.fullmatch(r"[1-9][0-9]{0,19}", run_id) and re.fullmatch(r"[1-9][0-9]{0,4}", run_attempt),
            "the run's identity is malformed")
    return {"run_id": int(run_id), "run_attempt": int(run_attempt), "workflow_sha": os.environ["GITHUB_SHA"]}


def attempt_message(plan, row, number):
    return {"schema": 1, "kind": ATTEMPT_KIND, "repository_id": REPOSITORY_ID, "attempt": number,
            "release": release_record(plan),
            "coordinate": {"name": row["name"], "version": row["version"], "sha256": row["sha256"],
                           "metadata_sha256": sha256(canonical(plan["metadata"][row["name"]]))},
            "publisher": publisher()}


def refused_message(plan, row, number, attempt, reason):
    require(reason in REFUSED_REASONS, "an unknown refusal reason")
    return {"schema": 1, "kind": REFUSED_KIND, "repository_id": REPOSITORY_ID, "attempt": number,
            "attempt_ref": f"refs/tags/{ATTEMPT}/{row['name']}/{row['version']}/{number}",
            "attempt_object": attempt, "reason": reason,
            "release": {"tag": plan["tag"], "revision": plan["revision"], "bundle_sha256": plan["bundle_sha256"]},
            "coordinate": {"name": row["name"], "version": row["version"]}, "publisher": publisher()}


def existing_record(short, message, values, token):
    """The tag object `refs/tags/<short>` points at, if it is exactly this
    record: an annotated tag at the revision holding this canonical message.
    None if the ref is absent; a refusal if it holds anything else."""
    seen = github(f"git/ref/tags/{short}", token, absent=True)
    if seen is None:
        return None
    target = seen.get("object", {})
    tag = github(f"git/tags/{target.get('sha')}", token) if SHA.fullmatch(str(target.get("sha"))) else {}
    require(target.get("type") == "tag" and tag.get("tag") == short and
            tag.get("object", {}).get("type") == "commit" and
            tag.get("object", {}).get("sha") == values["SOURCE_REVISION"] and
            isinstance(tag.get("message"), str) and tag["message"].rstrip("\n") == canonical(message).decode(),
            f"refs/tags/{short} exists, but not as this run's record")
    return target["sha"]


def record_refusal(plan, row, number, attempt, reason, values, token, sleep=None):
    """Record that attempt `number` is known not to have published.

    Unlike the attempt record, this is retried a few times: a reference that
    already exists is accepted only if it holds exactly this record.
    """
    short = f"{REFUSED}/{row['name']}/{row['version']}/{number}"
    message = refused_message(plan, row, number, attempt, reason)
    for tried in range(RECORD_TRIES):
        try:
            return existing_record(short, message, values, token) or create_record(short, message, values, token)
        except Failure:
            if tried + 1 == RECORD_TRIES:
                raise
        (sleep or time.sleep)(RECORD_SECONDS)


def settle_attempt(short, message, values, token, acknowledged=False, sleep=None):
    """After creating an attempt record failed: what does its ref hold now?

    ("ours", sha) when it reads back as exactly this run's record (the
    message names this run and attempt, so no other run's matches).
    ("absent", None) only when every read found no ref and GitHub never
    acknowledged creating it; a ref not visible yet may still appear.
    ("unresolved", None) when it holds anything else, or reads kept failing,
    or an acknowledged ref never showed.
    """
    certain = not acknowledged
    for tried in range(RECORD_TRIES):
        try:
            sha = existing_record(short, message, values, token)
            if sha:
                return "ours", sha
        except Failure:
            certain = False
        except Refusal:
            return "unresolved", None
        if tried + 1 < RECORD_TRIES:
            (sleep or time.sleep)(RECORD_SECONDS)
    return ("absent", None) if certain else ("unresolved", None)


class Receipts:
    """Diagnostic evidence of each attempt, written before the next step."""

    def __init__(self):
        directory = os.environ.get("TILEWRIGHT_PUBLISH_RECEIPTS", "")
        require(directory and Path(directory).is_absolute(), "TILEWRIGHT_PUBLISH_RECEIPTS must be an absolute path")
        self.directory = Path(directory)
        self.directory.mkdir(parents=True, exist_ok=True)

    def write(self, plan, row, number, attempt, stage, outcome=None):
        receipt = {"schema": 1, "kind": "world-crate-upload-receipt", "stage": stage,
                   "repository_id": REPOSITORY_ID, "name": row["name"], "version": row["version"],
                   "archive_sha256": row["sha256"], "attempt": number,
                   "attempt_ref": f"refs/tags/{ATTEMPT}/{row['name']}/{row['version']}/{number}",
                   "attempt_object": attempt, "release": release_record(plan), "publisher": publisher()}
        if outcome is not None:
            receipt.update(http_status=outcome.status, category=outcome.category, detail=outcome.detail)
        try:
            encoded = canonical(receipt).decode() + "\n"
            print(encoded, end="", file=sys.stderr, flush=True)
            path = self.directory / f"{row['name']}-{row['version']}-{number}-{stage}.json"
            with path.open("x", encoding="ascii") as stream:
                stream.write(encoded)
                stream.flush()
                os.fsync(stream.fileno())
        except Exception:
            raise Refusal(f"the {stage} receipt for {row['name']} {row['version']} could not be written; "
                          f"attempt {number}'s record stays") from None


# --- The upload -------------------------------------------------------------

class PutOutcome(typing.NamedTuple):
    """One PUT. For a trusted-publishing token, crates.io answers 401 and 403
    (its token checks) before it reads the archive. Its only 429 for such a
    token is the daily limit of new versions for a crate, which it checks
    inside its database transaction, after reading the archive but before
    storing it, and rolls back. So after any of the three nothing was
    published, and they are definite refusals; anything else is uncertain."""
    status: int | None
    category: str  # acknowledged, refused, rate_limited or uncertain
    detail: str  # a fixed local label, never server text

    @property
    def definite(self):
        return self.category in ("refused", "rate_limited")


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None  # urllib then raises HTTPError with the redirect's status


def put_once(data, metadata, token):
    """The registry API's upload framing, with the exact archive bytes."""
    encoded = json.dumps(metadata, sort_keys=True, separators=(",", ":")).encode()
    endpoint = "https://crates.io/api/v1/crates/new"
    request_ = urllib.request.Request(endpoint, method="PUT", data=(
        struct.pack("<I", len(encoded)) + encoded + struct.pack("<I", len(data)) + data), headers={
        "Authorization": token, "Content-Type": "application/octet-stream", "Accept": "application/json",
        "User-Agent": USER_AGENT})
    status, detail = None, "unexpected_error"
    try:
        with urllib.request.build_opener(urllib.request.ProxyHandler({}), NoRedirect()).open(
                request_, timeout=60) as response:
            status = response.status
            raw = response.read(65537)
            if response.geturl() == endpoint and 200 <= status < 300 and len(raw) <= 65536:
                value = json.loads(raw)
                if isinstance(value, dict) and "errors" not in value:
                    return PutOutcome(status, "acknowledged", "acknowledged")
            return PutOutcome(status, "uncertain", "invalid_response")
    except urllib.error.HTTPError as error:
        status = error.code
        try:
            error.close()
        except Exception:
            pass  # The status was observed either way.
        category = "refused" if status in (401, 403) else "rate_limited" if status == 429 else "uncertain"
        return PutOutcome(status, category, "http_error")
    except (json.JSONDecodeError, UnicodeDecodeError):
        detail = "invalid_response"
    except TimeoutError:
        detail = "timeout"
    except ConnectionError:
        detail = "connection_error"
    except urllib.error.URLError as error:
        detail = "timeout" if isinstance(error.reason, TimeoutError) else "connection_error"
    except Exception:
        pass
    return PutOutcome(status, "uncertain", detail)


def confirm_upload(row, run_id, sleep=None):
    """After a PUT, whatever its outcome: is it on crates.io, from this run?"""
    for poll in range(POLLS):
        try:
            state = registry_state(row)
        except Failure:
            state = None  # A read that failed says nothing yet.
        if state is not None and state.state == "present":
            trust = state.version.get("trustpub_data")
            require(isinstance(trust, dict) and trust.get("provider") == "github" and
                    trust.get("repository") == REPOSITORY and str(trust.get("run_id")) == str(run_id) and
                    state.version.get("published_by") is None,
                    f"crates.io has {row['name']} {row['version']}, but not from this run's trusted publishing")
            return True
        if poll + 1 < POLLS:
            (sleep or time.sleep)(POLL_SECONDS)
    return False


def evidence_files(evidence):
    files = {}
    for name, limit in (("bundle", CANDIDATES.MAX_BUNDLE_BYTES), ("SHA256SUMS", MAX_JSON),
                        ("attestations.jsonl", MAX_JSON), ("release.json", MAX_JSON)):
        path = evidence / name
        require(path.is_file() and not path.is_symlink() and path.stat().st_size <= limit,
                "the verify step's evidence is missing")
        files[name] = path.read_bytes()
    return files


def verify_release(values, token, evidence, *, fresh, root=ROOT, sleep=None):
    """Authenticate the release's bundle and plan the upload (design 5.4).

    `fresh` downloads the evidence into a new directory and checks GitHub's
    release attestation with the read token. Otherwise the upload step
    checks the evidence the verify step kept against the live release, so
    no GitHub token reaches a subprocess there.
    """
    tag, revision, digest = values["RELEASE_TAG"], values["SOURCE_REVISION"], values["BUNDLE_SHA256"]
    name = CANDIDATES.name(tag[1:])
    release = release_evidence(values, token, root)
    assets = {row["name"]: row for row in release["assets"]}
    require(name in assets and "SHA256SUMS" in assets, f"{tag} lacks its crate archive bundle or SHA256SUMS")
    require(assets[name].get("digest") == "sha256:" + digest,
            "the bundle's GitHub digest differs from the dispatched one")
    if fresh:
        data, sums = asset(release, name, CANDIDATES.MAX_BUNDLE_BYTES), asset(release, "SHA256SUMS", MAX_JSON)
        lines = attestations(digest, token, sleep)
        release_attestation(tag, name, data, token, sleep)
        PREPARE.reserve_output(evidence)
        for file, content in (("bundle", data), ("SHA256SUMS", sums), ("attestations.jsonl", lines),
                              ("release.json", canonical(snapshot(release)))):
            (evidence / file).write_bytes(content)
    else:
        kept = evidence_files(evidence)
        data, sums, lines = kept["bundle"], kept["SHA256SUMS"], kept["attestations.jsonl"]
        require(decode(kept["release.json"]) == snapshot(release), "the release changed since it was verified")
        require(assets["SHA256SUMS"].get("digest") == "sha256:" + sha256(sums),
                "SHA256SUMS differs from GitHub's digest")
    require(sha256(data) == digest, "the bundle differs from the dispatched digest")
    check_sums(sums, name, digest)
    made_by = provenance(data, name, lines, revision, digest)
    manifest, files, archives, version = candidate_bundle(data, revision, root)
    # Both this run's checkout and main as it is now: either may list it.
    for refused_version, refused_tag in {*PREPARE.candidates(root), *main_abandoned(token)}:
        require(refused_version != version and refused_tag != tag,
                f"crate version {version} of {tag} is abandoned in tools/crates/candidates.json")
    owner = ownership(revision, tag, root)
    require(owner["tags_at_revision"] == [tag], f"the local {tag} is not the one release tag at {revision}")
    require(owner["crate_version"] == version, "the bundle's crate version differs from the release tree's")
    plan = {"tag": tag, "revision": revision, "bundle_sha256": digest, "release": snapshot(release),
            "runs": made_by, "manifest": manifest, "manifest_sha256": sha256(files["candidate.json"]),
            "files": files, "archives": archives, "version": version, "owner": owner["owner"],
            "owner_tag": owner["owner_tag"], "pending": []}
    if not owner["owner"]:
        return plan
    order, metadata, dependencies = FIRST.publication_order(manifest, archives)
    registry = {row["name"]: registry_state(row) for row in order}
    found = {row["name"]: records(row, values, token) for row in order}
    actions = {}
    for row in order:
        state = registry[row["name"]].state
        require(state != "indeterminate", f"crates.io shows {row['name']} {version} in only one of its API, index "
                "and download; nothing was written; dispatch again in a few minutes")
        actions[row["name"]] = "present" if state == "present" else f"attempt {next_attempt(found[row['name']], row)}"
    plan.update(order=order, metadata=metadata, dependencies=dependencies, registry=registry, records=found,
                actions=actions, pending=[row["name"] for row in order if registry[row["name"]].state == "absent"])
    # Once per cohort, before its first PUT: a newer crate version on
    # crates.io leaves this one unpublished. A cohort whose uploads started
    # in this run is finished.
    later = newer(registry, version)
    require(not plan["pending"] or not later, f"crates.io already has {later[-1] if later else ''}, newer than "
            f"{version}, so {tag}'s crate version is stale and is not published")
    return plan


def plan_summary(plan, heading):
    lines = [f"### {heading}", "",
             f"{plan['tag']} at {plan['revision']}, crate version {plan['version']}, bundle {plan['bundle_sha256']}, "
             f"attested by {', '.join(plan['runs'])}.", ""]
    if not plan["owner"]:
        return lines + [f"Crate version {plan['version']} is owned by {plan['owner_tag']}: dispatch the \"Publish "
                        f"crates\" workflow for {plan['owner_tag']} instead. Nothing is uploaded for {plan['tag']}."]
    lines += ["| Crate | crates.io | Records | Next |", "| --- | --- | --- | --- |"]
    for row in plan["order"]:
        found = plan["records"][row["name"]]
        held = (["first-publication intent"] if found["legacy"] else []) + \
            [f"attempt {n}" + (" (refused)" if n in found["refused"] else "") for n in sorted(found["attempts"])]
        lines.append(f"| {row['name']} {row['version']} | {plan['registry'][row['name']].state} | "
                     f"{', '.join(held) or 'none'} | {plan['actions'][row['name']]} |")
    return lines


def verify_command(args):
    values = dispatch_inputs()
    boundary()
    token = credential(os.environ.pop("GH_READ_TOKEN", None), "GitHub read token")
    plan = verify_release(values, token, Path(args.evidence), fresh=True)
    summary(plan_summary(plan, "Crate publication plan"))
    for row in plan.get("order", []) if plan["owner"] else []:
        found = plan["records"][row["name"]]
        if plan["registry"][row["name"]].state == "present" and not (found["legacy"] or found["attempts"]):
            print(f"::warning title=Publish crates::crates.io has {row['name']} {row['version']} with this "
                  "release's archive, but no upload record: it was published outside this workflow")
    if args.github_output:
        outputs({"owner": str(plan["owner"]).lower(), "owner_tag": plan["owner_tag"],
                 "crate_version": plan["version"], "pending": len(plan["pending"])})
    print(f"Verified {plan['tag']}'s crate archives at crate version {plan['version']}; " + (
        f"{len(plan['pending'])} to upload" if plan["owner"] else f"owned by {plan['owner_tag']}"))


def upload(plan, values, token, intent, receipts, sleep=None):
    """Each crate in dependency order: at most one PUT, behind its attempt record."""
    run_id = publisher()["run_id"]
    rows = {row["name"]: row for row in plan["manifest"]["packages"]}
    for row in plan["order"]:
        name, coordinate = row["name"], f"{row['name']} {row['version']}"
        state = registry_state(row).state
        found = records(row, values, intent)
        if state == "present":
            continue
        require(state == "absent", f"crates.io shows {coordinate} in only one of its API, index and download; "
                "nothing was written for it; dispatch again in a few minutes")
        number = next_attempt(found, row)
        for dependency in sorted(plan["dependencies"][name]):
            require(registry_state(rows[dependency]).state == "present",
                    f"{dependency} {row['version']} is not on crates.io yet, so {name} waits for it")
        require(snapshot(github(f"releases/tags/{plan['tag']}", intent)) == plan["release"],
                "the release changed during the upload")
        data = plan["files"][row["asset"]]
        require(sha256(data) == row["sha256"], "a held archive changed")
        short, message = f"{ATTEMPT}/{name}/{row['version']}/{number}", attempt_message(plan, row, number)
        try:
            attempt = create_record(short, message, values, intent)
        except ValueError as error:  # Refusal, Failure, a bad answer; never an interrupt
            # The ref may exist though the answer was lost or odd, or its
            # read-back failed. Nothing was sent either way; only an attempt
            # ref read back as exactly this run's gets a refused record.
            state, attempt = settle_attempt(short, message, values, intent,
                                            getattr(error, "acknowledged", False), sleep)
            if state == "ours":
                try:
                    record_refusal(plan, row, number, attempt, "not-sent", values, intent, sleep)
                except Exception:
                    print(f"Attempt {number} for {coordinate} sent nothing, but that could not be recorded; "
                          "it stays unresolved.", file=sys.stderr)
            elif state == "absent":
                print(f"Attempt {number} for {coordinate} was not recorded, as far as GitHub shows; a later run "
                      f"makes it again. If the record appears after all, that run reports attempt {number} as "
                      "unresolved.", file=sys.stderr)
            else:
                print(f"Attempt {number} for {coordinate} could not be read back as this run's. Dispatch with "
                      f"operation=verify: it reports attempt {number} as unresolved, or plans it again.",
                      file=sys.stderr)
            raise
        # From here until the PUT is sent, any failure means attempt
        # `number` sent nothing: record that, so a later run can go on.
        try:
            receipts.write(plan, row, number, attempt, "reserved")
            require(registry_state(row).state == "absent", f"{coordinate} changed on crates.io after attempt "
                    f"{number} was recorded, so nothing was uploaded; dispatch again to check it")
        except BaseException:
            try:
                record_refusal(plan, row, number, attempt, "not-sent", values, intent, sleep)
            except Exception:
                print(f"Attempt {number} for {coordinate} sent nothing, but that could not be recorded either; "
                      "it stays unresolved.", file=sys.stderr)
            raise
        outcome = put_once(data, plan["metadata"][name], token)
        print(f"Attempt {number} for {coordinate}: HTTP {outcome.status}, {outcome.category}", file=sys.stderr)
        if outcome.definite:
            record_refusal(plan, row, number, attempt, f"http-{outcome.status}", values, intent, sleep)
        receipts.write(plan, row, number, attempt, "put", outcome)
        if confirm_upload(row, run_id, sleep):
            print(f"crates.io has {coordinate} from attempt {number} (HTTP {outcome.status}, {outcome.category})")
            continue
        if outcome.definite:
            raise Refusal(f"crates.io refused attempt {number} for {coordinate} with HTTP {outcome.status}, without "
                          f"publishing it. Fix the cause, then dispatch again for attempt {number + 1}")
        raise Refusal(f"attempt {number} for {coordinate} is unresolved ({outcome.category}, {outcome.detail}): "
                      "crates.io does not show it; the owner investigates")


def upload_command(args):
    token = os.environ.pop("CARGO_REGISTRY_TOKEN", None)
    intent = os.environ.pop("GH_INTENT_TOKEN", None)
    values = dispatch_inputs()
    boundary(upload=True)
    require(values["OPERATION"] == "publish", "only operation=publish uploads")
    require(os.environ.get("TILEWRIGHT_CRATES_PUBLISH") == "true",
            "crates.io publication is off: the repository variable TILEWRIGHT_CRATES_PUBLISH is not true")
    credential(token, "crates.io token")
    credential(intent, "intent token")
    receipts = Receipts()
    plan = verify_release(values, intent, Path(args.evidence), fresh=False)
    require(plan["owner"], f"crate version {plan['version']} is owned by {plan['owner_tag']}, not {plan['tag']}")
    try:
        upload(plan, values, token, intent, receipts)
    finally:
        token = intent = None
    if args.github_output:
        outputs({"complete": "true"})
    print(f"crates.io has all five crates at {plan['version']} from {plan['tag']}")


def offline_plan(values, evidence, root=ROOT):
    """The verify step's bundle, checked again by its bytes alone; no GitHub reads."""
    path = evidence / "bundle"
    require(path.is_file() and not path.is_symlink() and path.stat().st_size <= CANDIDATES.MAX_BUNDLE_BYTES,
            "the verify step's bundle is missing")
    data = path.read_bytes()
    require(sha256(data) == values["BUNDLE_SHA256"], "the verify step's bundle differs from the dispatched digest")
    manifest, files, archives, version = candidate_bundle(data, values["SOURCE_REVISION"], root)
    order, _, _ = FIRST.publication_order(manifest, archives)
    return {"manifest": manifest, "archives": archives, "version": version, "order": order}


def confirm_command(args, sleep=None):
    values = dispatch_inputs()
    boundary()
    plan = offline_plan(values, Path(args.evidence))
    states = {}
    for poll in range(POLLS):
        for row in plan["order"]:
            if states.get(row["name"]) != "present":
                try:
                    states[row["name"]] = registry_state(row).state
                except Failure:
                    states[row["name"]] = "unreadable"
        if all(state == "present" for state in states.values()) or poll + 1 == POLLS:
            break
        (sleep or time.sleep)(POLL_SECONDS)
    summary(["### crates.io", "", "| Crate | crates.io |", "| --- | --- |",
             *(f"| {name} {plan['version']} | {state} |" for name, state in states.items())])
    missing = [name for name, state in states.items() if state != "present"]
    require(not missing, f"crates.io does not have {', '.join(missing)} at {plan['version']}")
    print(f"crates.io has all five crates at {plan['version']} with the release's archives")


def consumer_command(args):
    values = dispatch_inputs()
    boundary()
    plan = offline_plan(values, Path(args.evidence))
    version = plan["version"]
    output = PREPARE.reserve_output(args.output)
    package = output / "consumer"
    lines = ["[package]", 'name = "tilewright-world-crates-consumer"', 'version = "0.0.0"', 'edition = "2024"',
             "publish = false", "", "[workspace]", "", "[dependencies]",
             *(f'{name} = "={version}"' for name in PREPARE.PUBLIC)]
    if args.patch:
        # The release's own archives stand in for crates.io's, so the
        # dependencies resolve fresh, as a consumer's would, before upload.
        lines += ["", "[patch.crates-io]"]
        for name in PREPARE.PUBLIC:
            base = output / "archives" / f"{name}-{version}"
            for path, data in plan["archives"][name].items():
                target = base / path
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(data)
            lines.append(f"{name} = {{ path = {json.dumps(str(base))} }}")
    (package / "src").mkdir(parents=True)
    (package / "Cargo.toml").write_text("\n".join(lines) + "\n")
    (package / "src/lib.rs").write_text("".join(f"pub use {name.replace('-', '_')};\n" for name in PREPARE.PUBLIC))
    if args.github_output:
        outputs({"toolchain": plan["manifest"]["toolchain"], "crate_version": version})
    print(f"Wrote a consumer of the five crates at ={version}" + (" from the release's archives" if args.patch else ""))


def accept_lock_command(args):
    values = dispatch_inputs()
    boundary()
    plan = offline_plan(values, Path(args.evidence))
    path = Path(args.lock)
    require(path.is_file() and not path.is_symlink() and path.stat().st_size <= MAX_JSON, "the lockfile is missing")
    packages = tomllib.loads(path.read_text()).get("package")
    require(isinstance(packages, list), "the lockfile lists no packages")
    for row in plan["manifest"]["packages"]:
        entries = [entry for entry in packages if isinstance(entry, dict) and
                   entry.get("name") == row["name"] and entry.get("version") == row["version"]]
        require(len(entries) == 1, f"the lockfile does not hold {row['name']} {row['version']} once")
        require(entries[0].get("source") == CRATES_IO, f"{row['name']} {row['version']} did not come from crates.io")
        require(entries[0].get("checksum") == row["sha256"],
                f"crates.io's {row['name']} {row['version']} differs from the release's archive")
    print(f"The consumer's lockfile holds the release's five archives at {plan['version']}, from crates.io")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    owner = commands.add_parser("owner", help="decide crate version ownership in the release plan (git only)")
    owner.add_argument("--revision", required=True)
    owner.add_argument("--previous-tag", default="")
    which = owner.add_mutually_exclusive_group(required=True)
    which.add_argument("--tag", help="the release tag the plan makes, vX.Y.Z")
    which.add_argument("--pull-request", action="store_true", help="a pull request's check: no release tag")
    owner.add_argument("--github-output", action="store_true")
    for name in ("verify", "upload", "confirm", "consumer", "accept-lock"):
        command = commands.add_parser(name)
        command.add_argument("--evidence", required=True, type=Path,
                             help="the verify step's evidence directory: new for verify, kept for the others")
        if name in ("verify", "upload", "consumer"):
            command.add_argument("--github-output", action="store_true")
        if name == "consumer":
            command.add_argument("--output", required=True, type=Path)
            command.add_argument("--patch", action="store_true", help="resolve the five from the release's archives")
        if name == "accept-lock":
            command.add_argument("--lock", required=True, type=Path)
    args = parser.parse_args(argv)
    if args.command == "owner":
        return owner_command(args)
    handlers = {"verify": verify_command, "upload": upload_command, "confirm": confirm_command,
                "consumer": consumer_command, "accept-lock": accept_lock_command}
    try:
        handlers[args.command](args)
        return 0
    except (Refusal, FIRST.Refusal) as error:
        message = str(error)
    except ValueError as error:
        # The candidate verifier's refusals are plain ValueErrors with static
        # text; other ValueErrors may quote their input.
        message = str(error) if type(error) is ValueError else f"malformed evidence ({type(error).__name__})"
    except (OSError, KeyError, TypeError, AttributeError, IndexError, subprocess.SubprocessError,
            tarfile.TarError, http.client.HTTPException) as error:
        message = f"malformed or unavailable evidence ({type(error).__name__})"
    print(f"::error title=Publish crates::{args.command}: {message}. See \"When a crate publication fails\" "
          "in docs/RELEASING.md.")
    return 1


if __name__ == "__main__":
    sys.exit(main())
