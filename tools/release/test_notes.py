"""Tests for tools/release/notes.sh. Release notes are immutable once out,
and the tags are pushed before they are written, so the crate version
wording must never stop the script.

Run from the repository root: python3 -m unittest discover -s tools/release
"""

from __future__ import annotations

import os
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "notes.sh"
REVISION = "a" * 40
BUNDLE = "tilewright-world-0.8.0-crate-candidates.tar.gz"


class NotesTests(unittest.TestCase):
    def setUp(self):
        scratch = tempfile.TemporaryDirectory()
        self.addCleanup(scratch.cleanup)
        self.assets = Path(scratch.name)
        (self.assets / "SHA256SUMS").write_text(
            f"{'1' * 64}  {BUNDLE}\n{'2' * 64}  tilewright-0.8.0-linux-amd64.tar.gz\n")

    def notes(self, **crates):
        environment = {"PATH": os.environ["PATH"], "TAG": "v0.8.0", "GO_TAG": "go/v0.8.0",
                       "GH_REPO": "seraphinastudios/tilewright-world", "BRANCH": "main", "REVISION": REVISION}
        environment.update(crates)
        result = subprocess.run(["bash", str(SCRIPT), str(self.assets)], env=environment, text=True,
                                capture_output=True, check=False)
        self.assertEqual((result.returncode, result.stderr), (0, ""))
        return result.stdout

    def test_owner(self):
        notes = self.notes(CRATE_VERSION="0.2.0", CRATE_OWNER="true", CRATE_OWNER_TAG="v0.8.0", CRATE_CHANGED="")
        self.assertIn(f"| `{BUNDLE}` | Rust crate archives (crate version 0.2.0) |", notes)
        self.assertIn("This release owns crate version 0.2.0. The \"Publish crates\" workflow uploads these exact "
                      "archives to crates.io; see crates.io for the result.", notes)
        self.assertNotIn("first released in", notes)

    def test_non_owner(self):
        notes = self.notes(CRATE_VERSION="0.2.0", CRATE_OWNER="false", CRATE_OWNER_TAG="v0.7.0",
                           CRATE_CHANGED="tilewright-edit tilewright-source")
        self.assertIn("Crate version 0.2.0 was first released in v0.7.0; crates.io receives that release's archives, "
                      "not these.", notes)
        self.assertIn("Changed since v0.7.0, so on crates.io only with a new crate version: `tilewright-edit`, "
                      "`tilewright-source`.", notes)
        self.assertNotIn("This release owns", notes)
        unchanged = self.notes(CRATE_VERSION="0.2.0", CRATE_OWNER="false", CRATE_OWNER_TAG="v0.7.0")
        self.assertNotIn("Changed since", unchanged)

    def test_empty_error_and_malformed_values_give_neutral_wording(self):
        neutral = "The crate archive bundle is not by itself a crates.io publication."
        for crates in ({}, {"CRATE_OWNER": "error"}, {"CRATE_VERSION": "", "CRATE_OWNER": "", "CRATE_OWNER_TAG": ""},
                       {"CRATE_VERSION": "0.2", "CRATE_OWNER": "true"},
                       {"CRATE_VERSION": "0.2.0", "CRATE_OWNER": "false", "CRATE_OWNER_TAG": "$(touch pwned)"},
                       {"CRATE_VERSION": "0.2.0\n# injected", "CRATE_OWNER": "true"}):
            with self.subTest(crates=crates):
                notes = self.notes(**crates)
                self.assertIn(neutral, notes)
                label = " (crate version 0.2.0)" if crates.get("CRATE_VERSION") == "0.2.0" else ""
                self.assertIn(f"| `{BUNDLE}` | Rust crate archives{label} |", notes)
                self.assertNotIn("injected", notes)
                self.assertFalse((self.assets / "pwned").exists())
        notes = self.notes(CRATE_VERSION="0.2.0", CRATE_OWNER="false", CRATE_OWNER_TAG="v0.7.0",
                           CRATE_CHANGED="tilewright-edit * `rm` tilewright-Source")
        self.assertIn("new crate version: `tilewright-edit`.", notes)

    def test_the_rest_is_unchanged(self):
        notes = self.notes()
        self.assertTrue(notes.startswith("## Assets\n"))
        self.assertIn("go get github.com/seraphinastudios/tilewright-world/go@v0.8.0", notes)
        self.assertIn(f"--source-ref refs/heads/main --source-digest {REVISION}", notes)


if __name__ == "__main__":
    unittest.main()
