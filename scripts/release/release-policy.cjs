"use strict";

// The release policy: which commits release, and how far they move the
// version. `.releaserc.json` loads the rules through `release-rules.cjs`, and
// `plan.mjs` uses them to plan each release. As in tilewright-renderer and
// tilewright-editor, every release from main bumps the minor version, and
// patch versions belong to maintenance branches named MAJOR.MINOR.x.

const MAINTENANCE_BRANCH = /^([0-9]+)\.([0-9]+)\.x$/;
const RELEASE_TAG = /^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$/;
const RELEASE_RANK = {patch: 1, minor: 2, major: 3};

// The version of the first release, which has no earlier tag to count from.
const INITIAL_VERSION = "0.1.0";

// Releases stay at 0.x until world format 1.0 is frozen. Until then a
// breaking change bumps the minor version, which Cargo and Go both treat as
// incompatible for 0.x versions. The pull request that freezes format 1.0
// sets this to false and has a breaking-change title ("feat!: ..."), so that
// its release is 1.0.0.
const PRE_1_0 = true;

function currentBranch() {
  const refName = process.env.GITHUB_REF_NAME;
  const branch =
    process.env.TILEWRIGHT_RELEASE_BRANCH ||
    (typeof refName === "string" && /^\d+\/merge$/.test(refName)
      ? process.env.GITHUB_BASE_REF || "main"
      : refName || process.env.GITHUB_HEAD_REF);
  if (typeof branch !== "string" || branch.length === 0) {
    throw new Error("TILEWRIGHT_RELEASE_BRANCH is required");
  }
  return branch;
}

function rulesForBranch(branch, pre10 = PRE_1_0) {
  const breaking = pre10 ? "minor" : "major";
  if (branch === "main") {
    return [
      {breaking: true, release: breaking},
      {revert: true, release: "minor"},
      {type: "*", release: "minor"},
    ];
  }
  if (MAINTENANCE_BRANCH.test(branch)) {
    return [
      {breaking: true, release: breaking},
      {type: "feat", release: "minor"},
      {revert: true, release: "patch"},
      {type: "*", release: "patch"},
    ];
  }
  throw new Error("unsupported release branch: " + branch);
}

// The release a parsed commit causes under the rules, as the commit analyzer
// decides it: the highest release of every rule that matches. A commit
// without a Conventional Commits type causes none.
function releaseForCommit(branch, commit, pre10 = PRE_1_0) {
  let release = null;
  for (const rule of rulesForBranch(branch, pre10)) {
    const matches = rule.breaking
      ? commit.breaking === true
      : rule.revert
        ? commit.revert === true
        : typeof commit.type === "string" && (rule.type === "*" || rule.type === commit.type);
    if (matches && (release === null || RELEASE_RANK[rule.release] > RELEASE_RANK[release])) {
      release = rule.release;
    }
  }
  return release;
}

function parseVersion(value) {
  const match = RELEASE_TAG.exec("v" + value);
  if (!match) {
    throw new Error("invalid release version: " + value);
  }
  return match.slice(1).map(Number);
}

function compareVersions(left, right) {
  const a = parseVersion(left);
  const b = parseVersion(right);
  for (let index = 0; index < 3; index += 1) {
    if (a[index] !== b[index]) {
      return a[index] - b[index];
    }
  }
  return 0;
}

function nextVersion(value, release) {
  if (!Object.hasOwn(RELEASE_RANK, release)) {
    throw new Error("invalid release level: " + release);
  }
  let [major, minor, patch] = parseVersion(value);
  if (release === "major") {
    major += 1;
    minor = 0;
    patch = 0;
  } else if (release === "minor") {
    minor += 1;
    patch = 0;
  } else {
    patch += 1;
  }
  return major + "." + minor + "." + patch;
}

// The version to release on `branch`, given the latest release tag that
// branch contains (or null) and the release level of the commits since it
// (or null for none). Returns null when nothing is to be released.
function planVersion({branch, lastTag, level, pre10 = PRE_1_0}) {
  rulesForBranch(branch, pre10);
  const maintenance = MAINTENANCE_BRANCH.exec(branch);
  if (lastTag !== null && !RELEASE_TAG.test(lastTag)) {
    throw new Error("invalid release tag: " + lastTag);
  }
  if (maintenance) {
    const line = maintenance[1] + "." + maintenance[2] + ".";
    if (lastTag === null || !lastTag.startsWith("v" + line)) {
      throw new Error("maintenance branch " + branch + " has no v" + line + "* release to continue");
    }
  }
  if (level === null || level === undefined) {
    return null;
  }
  if (lastTag === null) {
    return INITIAL_VERSION;
  }
  const last = lastTag.slice(1);
  if (pre10 && parseVersion(last)[0] >= 1) {
    throw new Error("PRE_1_0 is set, but " + lastTag + " is already 1.0 or later");
  }
  const version = nextVersion(last, level);
  if (maintenance && !version.startsWith(maintenance[1] + "." + maintenance[2] + ".")) {
    throw new Error("a " + level + " release cannot come from maintenance branch " + branch);
  }
  return version;
}

module.exports = {
  INITIAL_VERSION,
  MAINTENANCE_BRANCH,
  PRE_1_0,
  RELEASE_TAG,
  compareVersions,
  currentBranch,
  nextVersion,
  planVersion,
  releaseForCommit,
  rulesForBranch,
};
