"use strict";

const assert = require("node:assert/strict");
const {
  INITIAL_VERSION,
  PRE_1_0,
  compareVersions,
  currentBranch,
  nextVersion,
  planVersion,
  releaseForCommit,
  rulesForBranch,
} = require("./release-policy.cjs");

// Until format 1.0, main releases minor versions, breaking or not.
assert.equal(PRE_1_0, true);
assert.equal(INITIAL_VERSION, "0.1.0");
assert.equal(releaseForCommit("main", {type: "docs"}), "minor");
assert.equal(releaseForCommit("main", {type: "fix"}), "minor");
assert.equal(releaseForCommit("main", {type: "feat", breaking: true}), "minor");
assert.equal(releaseForCommit("main", {type: null, revert: true}), "minor");
assert.equal(releaseForCommit("main", {type: null}), null);
assert.equal(releaseForCommit("main", {type: "feat", breaking: true}, false), "major");

// Maintenance branches release patches; anything more is out of their range.
assert.equal(releaseForCommit("0.3.x", {type: "fix"}), "patch");
assert.equal(releaseForCommit("0.3.x", {type: "feat"}), "minor");
assert.equal(releaseForCommit("0.3.x", {type: "fix", breaking: true}), "minor");
assert.throws(() => rulesForBranch("release"), /unsupported/);
assert.throws(() => rulesForBranch("0.3.y"), /unsupported/);

assert.equal(nextVersion("0.1.0", "minor"), "0.2.0");
assert.equal(nextVersion("0.3.4", "patch"), "0.3.5");
assert.equal(nextVersion("0.9.2", "major"), "1.0.0");
assert.throws(() => nextVersion("0.1", "minor"), /invalid release version/);
assert.throws(() => nextVersion("01.1.0", "minor"), /invalid release version/);
assert.throws(() => nextVersion("0.1.0", "none"), /invalid release level/);
assert.ok(compareVersions("0.10.0", "0.9.9") > 0);
assert.ok(compareVersions("1.0.0", "0.99.0") > 0);
assert.equal(compareVersions("0.2.1", "0.2.1"), 0);

// The first release takes the initial version; later ones count from the
// latest tag; nothing to release plans no version.
assert.equal(planVersion({branch: "main", lastTag: null, level: "minor"}), "0.1.0");
assert.equal(planVersion({branch: "main", lastTag: null, level: null}), null);
assert.equal(planVersion({branch: "main", lastTag: "v0.4.0", level: "minor"}), "0.5.0");
assert.equal(planVersion({branch: "main", lastTag: "v0.4.0", level: null}), null);
assert.equal(planVersion({branch: "0.4.x", lastTag: "v0.4.0", level: "patch"}), "0.4.1");
assert.throws(() => planVersion({branch: "0.4.x", lastTag: "v0.4.0", level: "minor"}), /cannot come from maintenance/);
assert.throws(() => planVersion({branch: "0.4.x", lastTag: null, level: "patch"}), /has no v0\.4\.\* release/);
assert.throws(() => planVersion({branch: "0.4.x", lastTag: "v0.3.2", level: "patch"}), /has no v0\.4\.\* release/);
assert.throws(() => planVersion({branch: "main", lastTag: "v1.0.0", level: "minor"}), /PRE_1_0 is set/);
assert.throws(() => planVersion({branch: "main", lastTag: "v0.4", level: "minor"}), /invalid release tag/);
// Freezing format 1.0: a breaking change after PRE_1_0 is cleared.
assert.equal(planVersion({branch: "main", lastTag: "v0.9.0", level: "major", pre10: false}), "1.0.0");

const saved = {...process.env};
try {
  delete process.env.TILEWRIGHT_RELEASE_BRANCH;
  process.env.GITHUB_REF_NAME = "42/merge";
  process.env.GITHUB_BASE_REF = "0.3.x";
  assert.equal(currentBranch(), "0.3.x");
  process.env.GITHUB_REF_NAME = "main";
  assert.equal(currentBranch(), "main");
  process.env.TILEWRIGHT_RELEASE_BRANCH = "0.2.x";
  assert.equal(currentBranch(), "0.2.x");
  delete require.cache[require.resolve("./release-rules.cjs")];
  assert.equal(require("./release-rules.cjs").at(-1).release, "patch");
  process.env.TILEWRIGHT_RELEASE_BRANCH = "main";
  delete require.cache[require.resolve("./release-rules.cjs")];
  assert.equal(require("./release-rules.cjs").at(-1).release, "minor");
} finally {
  for (const key of Object.keys(process.env)) {
    if (!(key in saved)) delete process.env[key];
  }
  Object.assign(process.env, saved);
}

console.log("release policy: main, maintenance, first-release and 1.0 rules verified");
