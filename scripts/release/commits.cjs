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

const {releaseForCommit} = require("./release-policy.cjs");

const HEADER = /^(\w*)(?:\((.*)\))?!?: (.*)$/;
const BREAKING_HEADER = /^(\w*)(?:\((.*)\))?!: (.*)$/;
const NOTE = /^[\s|*]*(BREAKING CHANGE|BREAKING-CHANGE)[:\s]+(.*)/i;
const REVERT = /^(?:Revert|revert:)\s"?([\s\S]+?)"?\s*This reverts commit (\w*)\./i;
const FIELD = /^-(.*?)-$/;
const GPG = /^\s*gpg:/;
const URL = /\b(?:https?):\/\/(?:www\.)?([-a-zA-Z0-9@:%_+.~#?&//=])+\b/;
const ACTIONS = "close|closes|closed|fix|fixes|fixed|resolve|resolves|resolved";
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

// Whether a line holds an issue reference such as "#12" or "fixes #12".
// A reference ends a breaking-change note, which decides whether the line
// after it is read for another note.
function hasReference(line) {
  const references = new RegExp(`(${ACTIONS})(?:\\s+(.*?))(?=(?:${ACTIONS})|$)`, "gi");
  const sentences = [];
  if (references.test(line)) {
    references.lastIndex = 0;
    for (let match = references.exec(line); match; match = references.exec(line)) {
      sentences.push(match[2] || "");
    }
  } else {
    sentences.push(line);
  }
  const parts = new RegExp("(?:.*?)??\\s*([\\w-\\.\\/]*?)??(#)([\\w-]+)(?=\\s|$|[,;)\\]])", "gi");
  return sentences.some((sentence) => !URL.test(sentence) && parts.test(sentence));
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
  let index = 0;
  let notes = 0;
  const available = () => index < lines.length;

  // The first line is the header; the preset has no merge pattern.
  const header = lines[index] || null;
  index += 1;
  let type = null;
  if (header) {
    const matches = header.match(BREAKING_HEADER) || header.match(HEADER);
    if (matches) {
      type = matches[1] || null;
    }
  }

  // A "-field-" line starts a field that takes every line after it, so
  // nothing after it is read for notes. A "--" line is skipped.
  function readFields() {
    let field = null;
    let read = false;
    while (available()) {
      const matches = lines[index].match(FIELD);
      if (matches) {
        field = matches[1] || null;
        index += 1;
      } else if (field) {
        read = true;
        index += 1;
      } else {
        break;
      }
    }
    return read;
  }

  // A note runs until a field, another note, or a line with a reference.
  function readNote() {
    if (!available() || !NOTE.test(lines[index])) {
      return false;
    }
    notes += 1;
    index += 1;
    while (available()) {
      if (readFields() || readNote()) {
        return true;
      }
      if (!available()) {
        break;
      }
      const reference = hasReference(lines[index]);
      index += 1;
      if (reference) {
        break;
      }
    }
    return true;
  }

  while (available()) {
    readFields();
    readNote();
    // The line after a note is taken as body or footer without being
    // read for a field or a note.
    if (available()) {
      index += 1;
    }
  }

  // "type!:" is a breaking change when no note says so.
  if (notes === 0 && header && BREAKING_HEADER.test(header)) {
    notes = 1;
  }
  const reverted = message.match(REVERT);
  return {
    header,
    type,
    breaking: notes > 0,
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
    // The analyzer's default rules apply only when no rule of the policy
    // matches. They match nothing the policy's rules do not.
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
