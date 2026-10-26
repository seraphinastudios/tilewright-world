"""Tests for tools/release/tags.sh, with a stand-in gh and a local bare
repository as origin, so the pushes are real.

Run from the repository root: python3 -m unittest discover -s tools/release
"""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "tags.sh"

# Answers the gh api calls tags.sh makes. Tags come from the bare
# repository, releases from a JSON state file. Like gh, a failed request
# prints its error body on stdout and exits with 1. The state can make the
# ref or release lookups fail, or hide the refs, as if a tag were created
# after the lookup.
FAKE_GH = r'''#!/usr/bin/env python3
import json, os, subprocess, sys

arguments = sys.argv[1:]
assert arguments[0] == "api", arguments
arguments = arguments[1:]
method = "GET"
paginate = False
jq = None
path = None
while arguments:
    argument = arguments.pop(0)
    if argument == "--method":
        method = arguments.pop(0)
    elif argument == "--paginate":
        paginate = True
    elif argument == "--jq":
        jq = arguments.pop(0)
    else:
        path = argument
state_path = os.environ["FAKE_GH_STATE"]
with open(state_path) as stream:
    state = json.load(stream)
prefix = "repos/" + os.environ["GH_REPO"] + "/"
assert path.startswith(prefix), path
path = path[len(prefix):]


def fail(status, message):
    print(json.dumps({"message": message, "status": str(status)}))
    sys.exit(1)


def answer(value):
    if jq is None:
        print(json.dumps(value))
    else:
        print(subprocess.run(["jq", "-r", jq], input=json.dumps(value), text=True, check=True,
                             stdout=subprocess.PIPE).stdout, end="")


if path.startswith("git/matching-refs/tags/") and method == "GET":
    assert paginate
    if state.get("fail_ref_lookups"):
        fail(502, "Server Error")
    prefix = "refs/tags/" + path[len("git/matching-refs/tags/"):]
    listed = subprocess.run(
        ["git", "-C", os.environ["FAKE_GH_BARE"], "for-each-ref", "--format=%(refname) %(objecttype) %(objectname)",
         "refs/tags/"], text=True, check=True, stdout=subprocess.PIPE).stdout.split("\n")
    refs = []
    for line in filter(None, listed):
        ref, kind, sha = line.split(" ")
        if ref.startswith(prefix) and not state.get("hide_refs"):
            refs.append({"ref": ref, "object": {"type": kind, "sha": sha}})
    answer(refs)
elif path == "releases?per_page=100" and method == "GET":
    assert paginate
    if state.get("fail_release_lookups"):
        fail(502, "Server Error")
    answer(state["releases"])
elif path.startswith("releases/") and method == "DELETE":
    identifier = int(path[len("releases/"):])
    state["deleted"].append(identifier)
    state["releases"] = [release for release in state["releases"] if release["id"] != identifier]
    with open(state_path, "w") as stream:
        json.dump(state, stream)
else:
    fail(404, "Not Found")
'''


def git(root: Path, *arguments: str) -> str:
    return subprocess.run(
        ["git", *arguments], cwd=root, check=True, stdout=subprocess.PIPE, text=True
    ).stdout.strip()


@unittest.skipIf(os.name == "nt", "tags.sh runs on the Linux release runner")
class TagsTest(unittest.TestCase):
    def setUp(self) -> None:
        directory = tempfile.TemporaryDirectory(prefix="tilewright-tags-test-")
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.bare = self.root / "origin.git"
        git(self.root, "init", "--quiet", "--bare", str(self.bare))
        self.work = self.root / "work"
        git(self.root, "init", "--quiet", str(self.work))
        for key, value in (
            ("user.name", "Release fixture"),
            ("user.email", "release-fixture@example.invalid"),
            ("commit.gpgsign", "false"),
            ("tag.gpgsign", "false"),
        ):
            git(self.work, "config", key, value)
        git(self.work, "commit", "--quiet", "--allow-empty", "-m", "feat: one")
        self.older = git(self.work, "rev-parse", "HEAD")
        git(self.work, "commit", "--quiet", "--allow-empty", "-m", "feat: two")
        self.revision = git(self.work, "rev-parse", "HEAD")
        git(self.work, "remote", "add", "origin", str(self.bare))
        git(self.work, "push", "--quiet", "origin", "HEAD:refs/heads/main")
        self.bin = self.root / "bin"
        self.bin.mkdir()
        (self.bin / "gh").write_text(FAKE_GH, encoding="utf-8")
        (self.bin / "gh").chmod(0o755)
        self.state = self.root / "state.json"
        self.output = self.root / "output"
        self.output.write_text("", encoding="utf-8")
        self.write_state(releases=[])

    def write_state(self, releases: list[dict], **failures: bool) -> None:
        state = {"releases": releases, "deleted": [], **failures}
        self.state.write_text(json.dumps(state), encoding="utf-8")

    def remote_tag(self, tag: str) -> str | None:
        found = subprocess.run(
            ["git", "-C", str(self.bare), "rev-parse", "--verify", "--quiet", f"refs/tags/{tag}"],
            stdout=subprocess.PIPE,
            text=True,
        )
        return found.stdout.strip() if found.returncode == 0 else None

    def run_script(self) -> subprocess.CompletedProcess:
        environment = {
            **os.environ,
            "PATH": f"{self.bin}{os.pathsep}{os.environ['PATH']}",
            "TAG": "v0.2.0",
            "GO_TAG": "go/v0.2.0",
            "REVISION": self.revision,
            "GH_REPO": "example/world",
            "GH_TOKEN": "not-a-token",
            "GITHUB_OUTPUT": str(self.output),
            "FAKE_GH_STATE": str(self.state),
            "FAKE_GH_BARE": str(self.bare),
        }
        return subprocess.run(
            ["bash", str(SCRIPT)],
            cwd=self.work,
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )

    def test_creates_both_tags_at_the_revision(self) -> None:
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertEqual(self.remote_tag("v0.2.0"), self.revision)
        self.assertEqual(self.remote_tag("go/v0.2.0"), self.revision)
        self.assertEqual(self.output.read_text(encoding="utf-8"), "published=false\n")

    def test_accepts_a_tag_a_failed_attempt_left_at_the_revision(self) -> None:
        git(self.work, "push", "--quiet", "origin", f"{self.revision}:refs/tags/v0.2.0")
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("::notice::v0.2.0 already points at", result.stdout)
        self.assertEqual(self.remote_tag("go/v0.2.0"), self.revision)

    def test_refuses_a_tag_at_another_commit(self) -> None:
        git(self.work, "push", "--quiet", "origin", f"{self.older}:refs/tags/v0.2.0")
        result = self.run_script()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("::error::v0.2.0 exists and does not point at", result.stdout)
        self.assertEqual(self.remote_tag("v0.2.0"), self.older)
        self.assertIsNone(self.remote_tag("go/v0.2.0"))

    def test_refuses_an_annotated_tag(self) -> None:
        git(self.work, "tag", "--annotate", "-m", "not by the pipeline", "v0.2.0", self.revision)
        git(self.work, "push", "--quiet", "origin", "refs/tags/v0.2.0")
        result = self.run_script()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("::error::v0.2.0 exists and does not point at", result.stdout)

    def test_only_the_exact_tag_counts(self) -> None:
        # matching-refs also lists tags that merely start with the name.
        git(self.work, "push", "--quiet", "origin", f"{self.older}:refs/tags/v0.2.0-rc.1")
        git(self.work, "push", "--quiet", "origin", f"{self.older}:refs/tags/go/v0.2.01")
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertEqual(self.remote_tag("v0.2.0"), self.revision)
        self.assertEqual(self.remote_tag("go/v0.2.0"), self.revision)

    def test_a_failed_tag_lookup_stops_before_pushing(self) -> None:
        self.write_state(releases=[], fail_ref_lookups=True)
        result = self.run_script()
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIsNone(self.remote_tag("v0.2.0"))
        self.assertIsNone(self.remote_tag("go/v0.2.0"))
        self.assertEqual(self.output.read_text(encoding="utf-8"), "")

    def test_a_tag_created_after_the_lookup_is_not_moved(self) -> None:
        # The lookup misses the tag, so the script pushes both; the push
        # refuses the existing tag and, being atomic, creates neither.
        git(self.work, "push", "--quiet", "origin", f"{self.older}:refs/tags/v0.2.0")
        self.write_state(releases=[], hide_refs=True)
        result = self.run_script()
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertEqual(self.remote_tag("v0.2.0"), self.older)
        self.assertIsNone(self.remote_tag("go/v0.2.0"))

    def test_a_failed_release_lookup_stops_without_an_answer(self) -> None:
        # Not knowing whether the release is published must not lead to
        # publishing it again.
        self.write_state(releases=[], fail_release_lookups=True)
        result = self.run_script()
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertEqual(self.output.read_text(encoding="utf-8"), "")

    def test_a_published_release_is_checked_not_recreated(self) -> None:
        git(self.work, "push", "--quiet", "origin", f"{self.revision}:refs/tags/v0.2.0")
        git(self.work, "push", "--quiet", "origin", f"{self.revision}:refs/tags/go/v0.2.0")
        self.write_state(releases=[{"id": 5, "tag_name": "v0.2.0", "draft": False}])
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertEqual(self.output.read_text(encoding="utf-8"), "published=true\n")
        self.assertEqual(json.loads(self.state.read_text(encoding="utf-8"))["deleted"], [])

    def test_drafts_left_for_the_tag_are_deleted(self) -> None:
        self.write_state(
            releases=[
                {"id": 3, "tag_name": "v0.1.0", "draft": True},
                {"id": 4, "tag_name": "v0.1.0", "draft": False},
                {"id": 7, "tag_name": "v0.2.0", "draft": True},
                {"id": 9, "tag_name": "v0.2.0", "draft": True},
            ]
        )
        result = self.run_script()
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertEqual(json.loads(self.state.read_text(encoding="utf-8"))["deleted"], [7, 9])
        self.assertEqual(self.output.read_text(encoding="utf-8"), "published=false\n")


if __name__ == "__main__":
    unittest.main()
