"use strict";

// Tests commits.cjs. The table's expectations are what
// @semantic-release/commit-analyzer 13.0.1 with the conventionalcommits
// preset 9.3.1 answered for the same messages and rules, which the
// planner used before commits.cjs replaced it.

const assert = require("node:assert/strict");
const {parseCommit, releaseLevel} = require("./commits.cjs");

const H1 = "0".repeat(39) + "1";
const hash = (n) => n.toString(16).padStart(40, "0");

// [message, type, breaking, revert, release on main, on main after 1.0,
// on a maintenance branch]
const cases = [
  ["feat: add", "feat", false, null, "minor", "minor", "minor"],
  ["fix(scope): repair", "fix", false, null, "minor", "minor", "patch"],
  ["feat!: change", "feat", true, null, "minor", "major", "minor"],
  ["fix(core)!: break", "fix", true, null, "minor", "major", "minor"],
  ["docs: explain", "docs", false, null, "minor", "minor", "patch"],
  ["Feat: capitalised", "Feat", false, null, "minor", "minor", "patch"],
  ["tidy up", null, false, null, null, null, null],
  ["feat:no space", null, false, null, null, null, null],
  [": empty type", null, false, null, null, null, null],
  ["feat(): empty scope", "feat", false, null, "minor", "minor", "minor"],
  ["fix: repair\n\nBREAKING CHANGE: readers must update", "fix", true, null, "minor", "major", "minor"],
  ["fix: x\n\nBREAKING-CHANGE: y", "fix", true, null, "minor", "major", "minor"],
  ["fix: x\n\nbreaking change: lower case", "fix", true, null, "minor", "major", "minor"],
  ["fix: x\n\nBREAKING CHANGES: plural", "fix", false, null, "minor", "minor", "patch"],
  ["fix: x\n\n  * BREAKING CHANGE: in a list", "fix", true, null, "minor", "major", "minor"],
  ["tidy\n\nBREAKING CHANGE: untyped", null, true, null, "minor", "major", "minor"],
  ["fix: x\n\n---\n\nBREAKING CHANGE: after a rule", "fix", false, null, "minor", "minor", "patch"],
  ["fix: x\n\n--\nBREAKING CHANGE: after two dashes", "fix", true, null, "minor", "major", "minor"],
  ["fix: x\n\nBREAKING CHANGE: a\nfixes #12\nnext\nBREAKING CHANGE: after a reference", "fix", true, null, "minor", "major", "minor"],
  ["fix: x\n\nBREAKING CHANGE: a\nsee #12\nBREAKING CHANGE: right after a reference", "fix", true, null, "minor", "major", "minor"],
  ["fix: x\r\n\r\nBREAKING CHANGE: crlf", "fix", true, null, "minor", "major", "minor"],
  ["gpg: signature\nfeat: after gpg", "feat", false, null, "minor", "minor", "minor"],
  ["\n\nfeat: leading newlines\n\n", "feat", false, null, "minor", "minor", "minor"],
  [`Revert "feat: add"\n\nThis reverts commit ${H1}.`, null, false, {header: "feat: add", hash: H1}, "minor", "minor", "patch"],
  ["Revert \"feat: add\"", null, false, null, null, null, null],
  ["Revert feat: add This reverts commit 123.", null, false, {header: "feat: add", hash: "123"}, "minor", "minor", "patch"],
];

for (const [message, type, breaking, revert, main, main1, maintenance] of cases) {
  const parsed = parseCommit(message);
  assert.deepEqual(
    {type: parsed.type, breaking: parsed.breaking, revert: parsed.revert},
    {type, breaking, revert},
    JSON.stringify(message),
  );
  const commits = [{hash: hash(99), message}];
  assert.equal(releaseLevel({branch: "main", commits, pre10: true}), main, JSON.stringify(message));
  assert.equal(releaseLevel({branch: "main", commits, pre10: false}), main1, JSON.stringify(message));
  assert.equal(releaseLevel({branch: "0.3.x", commits, pre10: true}), maintenance, JSON.stringify(message));
}

// A revert and the commit it reverts cancel out when both are in the range;
// a revert of a revert brings the commit back. Lists are newest first.
const added = {hash: hash(1), message: "feat: add"};
const reverted = {hash: hash(2), message: `Revert "feat: add"\n\nThis reverts commit ${hash(1)}.`};
const restored = {hash: hash(3), message: `Revert "Revert "feat: add""\n\nThis reverts commit ${hash(2)}.`};
const untyped = {hash: hash(4), message: "tidy"};
const fixed = {hash: hash(5), message: "fix: repair"};
const level = (branch, commits) => releaseLevel({branch, commits, pre10: true});
assert.equal(level("main", [reverted, added]), null);
assert.equal(level("main", [reverted, untyped, added]), null);
assert.equal(level("0.3.x", [reverted, fixed, added]), "patch");
assert.equal(level("0.3.x", [restored, reverted, added]), "minor");
// A revert older than its commit, or one whose commit is outside the range,
// counts as a revert.
assert.equal(level("0.3.x", [added, reverted]), "minor");
assert.equal(level("0.3.x", [reverted]), "patch");
assert.equal(level("main", [{hash: hash(6), message: "  "}, untyped]), null);
assert.equal(level("main", []), null);

// The analyzer failed on a note that runs into a trailing "-field-" line;
// the note counts.
assert.equal(parseCommit("fix: x\n\nBREAKING CHANGE: a\n-f-").breaking, true);
assert.throws(() => parseCommit("  \n"), /expected a commit message/);

console.log("release commits: " + cases.length + " messages and the revert rules match the commit analyzer");
