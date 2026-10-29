"use strict";

// Reads commit messages the way the commit analyzer of semantic-release
// does with the conventionalcommits preset, and sums up the release they
// call for under the rules in release-policy.cjs. It has no dependencies,
// so the release planner runs only code from this repository.
//
// The parsing follows conventional-commits-parser 6.4.0 with the options of
// conventional-changelog-conventionalcommits 9.3.1. The handling of reverts
// follows conventional-commits-filter 5.0.0, and the combination follows
// @semantic-release/commit-analyzer 13.0.1, which the tilewright renderer
// and editor use. Only what decides a release is kept: a commit's type,
// whether it is breaking, and whether it reverts another. PROVENANCE.md
// records the adaptation, and THIRD_PARTY_LICENSES.md here holds the
// packages' MIT and ISC notices. test-commits.cjs holds messages whose
// results were checked against those packages.
//
// Two differences are deliberate. The parser copied each "-name-" field
// line onto the commit, so a field such as "-type-", "-revert-", "-hash-"
// or "-tag-" changed the analyzer's answer, and "-notes-",
// "-references-" or (with an @mention in the message) "-mentions-" made
// it throw. Here field lines only hide what follows
// them, as they did from the note search, and their contents are ignored.
// And where the analyzer threw on a note running into a trailing field,
// the note counts.

const {releaseForCommit} = require("./release-policy.cjs");

const HEADER = /^(\w*)(?:\((.*)\))?!?: (.*)$/;
const BREAKING_HEADER = /^(\w*)(?:\((.*)\))?!: (.*)$/;
const NOTE = /^[\s|*]*(BREAKING CHANGE|BREAKING-CHANGE)[:\s]+(.*)/i;
const REVERT = /^(?:Revert|revert:)\s"?([\s\S]+?)"?\s*This reverts commit (\w*)\./i;
const FIELD = /^-(.*?)-$/;
const GPG = /^\s*gpg:/;
const RELEASE_RANK = {patch: 1, minor: 2, major: 3};

// Leading and trailing line breaks removed, as the parser does.
function trimNewLines(input) {
  const first = input.search(/[^\r\n]/);
  if (first === -1) {
    return "";
  }
  let last = input.length - 1;
  while (input[last] === "\r" || input[last] === "\n") {
    last -= 1;
  }
  return input.substring(first, last + 1);
}

// The parts of a commit message that decide a release: {header, type,
// breaking, revert}. `revert` is null or {header, hash} of the reverted
// commit.
function parseCommit(message) {
  if (typeof message !== "string" || !message.trim()) {
    throw new TypeError("expected a commit message");
  }
  const lines = trimNewLines(message)
    .split(/\r?\n/)
    .filter((line) => !GPG.test(line));
  // The first line is the header; the preset has no merge pattern.
  const header = lines[0] || null;
  let type = null;
  if (header) {
    const matches = header.match(BREAKING_HEADER) || header.match(HEADER);
    if (matches) {
      type = matches[1] || null;
    }
  }

  // A commit is breaking if "type!:" says so or a line after the header
  // starts a BREAKING CHANGE note. The parser does not read lines inside a
  // field for notes: a "-name-" line starts one and a "--" line ends it.
  // Nothing after the first note changes the answer, so reading stops
  // there.
  let breaking = header !== null && BREAKING_HEADER.test(header);
  let field = null;
  for (const line of lines.slice(1)) {
    if (breaking) {
      break;
    }
    const matches = line.match(FIELD);
    if (matches) {
      field = matches[1] || null;
    } else if (!field && NOTE.test(line)) {
      breaking = true;
    }
  }
  const reverted = message.match(REVERT);
  return {
    header,
    type,
    breaking,
    revert: reverted ? {header: reverted[1] || null, hash: reverted[2] || null} : null,
  };
}

function same(left, right) {
  const a = typeof left === "string" ? left.trim() : left;
  const b = typeof right === "string" ? right.trim() : right;
  return a === b;
}

// The commits that count, newest first as git log lists them: a revert and
// the commit it reverts (matched by header and hash) cancel out when both
// are in the list and the revert is the newer.
function withoutReverted(commits) {
  const pending = [];
  const kept = [];
  for (const commit of commits) {
    const revert = pending.findIndex((candidate) =>
      same(commit.header, candidate.revert.header) && same(commit.hash, candidate.revert.hash));
    if (revert !== -1) {
      pending.splice(revert, 1);
    } else if (commit.revert) {
      pending.push(commit);
    } else {
      kept.push(commit);
    }
  }
  return [...kept, ...pending];
}

// The release that commits ({hash, message}, newest first) call for on
// `branch`: "major", "minor", "patch" or null for none.
function releaseLevel({branch, commits, pre10}) {
  const parsed = commits
    .filter(({message}) => typeof message === "string" && message.trim())
    .map(({hash, message}) => ({hash, ...parseCommit(message)}));
  let level = null;
  for (const commit of withoutReverted(parsed)) {
    // The analyzer's default rules applied only when no rule of the
    // policy matched. Without fields they match nothing the policy's
    // rules do not.
    const release = releaseForCommit(
      branch,
      {type: commit.type, breaking: commit.breaking, revert: commit.revert !== null},
      pre10,
    );
    if (release !== null && (level === null || RELEASE_RANK[release] > RELEASE_RANK[level])) {
      level = release;
    }
  }
  return level;
}

module.exports = {parseCommit, releaseLevel};
