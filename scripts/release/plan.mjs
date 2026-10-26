// Plans a release from the checked-out commit without any credentials: finds
// the latest release tag the branch contains, analyzes the commits since it
// with the commit analyzer and the rules in .releaserc.json, and prints one
// JSON object saying what to release and whether to publish it.
//
// Environment:
//   TILEWRIGHT_RELEASE_BRANCH   the branch being released (default: from GITHUB_REF_NAME)
//   TILEWRIGHT_RELEASE_TRIGGER  push, workflow_dispatch or pull_request
//   NODE_PATH                   where the pinned tools from package.json are installed

import {execFileSync} from "node:child_process";
import {readFileSync} from "node:fs";
import {createRequire} from "node:module";
import {join} from "node:path";
import {pathToFileURL} from "node:url";

const require = createRequire(import.meta.url);
const policy = require("./release-policy.cjs");
const {analyzeCommits} = await import(pathToFileURL(require.resolve("@semantic-release/commit-analyzer")).href);

const TRIGGERS = new Set(["push", "workflow_dispatch", "pull_request"]);
const trigger = process.env.TILEWRIGHT_RELEASE_TRIGGER;
if (!TRIGGERS.has(trigger)) {
  throw new Error("TILEWRIGHT_RELEASE_TRIGGER must be push, workflow_dispatch or pull_request");
}

const cwd = process.cwd();
const configuration = JSON.parse(readFileSync(join(cwd, ".releaserc.json"), "utf8"));
if (configuration.tagFormat !== "v${version}") {
  throw new Error("release tags must have the form v${version}");
}
const analyzer = configuration.plugins.find(
  (plugin) => Array.isArray(plugin) && plugin[0] === "@semantic-release/commit-analyzer",
);
if (!analyzer) {
  throw new Error(".releaserc.json configures no commit analyzer");
}

const git = (...args) => execFileSync("git", args, {cwd, encoding: "utf8", maxBuffer: 64 * 1024 * 1024}).trim();
const lines = (text) => text.split("\n").filter((line) => line.length > 0);

const revision = git("rev-parse", "HEAD");
if (!/^[0-9a-f]{40}$/.test(revision)) {
  throw new Error("invalid revision: " + revision);
}
const branch = policy.currentBranch();
policy.rulesForBranch(branch);
const maintenance = policy.MAINTENANCE_BRANCH.exec(branch);

const tags = lines(git("tag", "--merged", "HEAD", "--list", "v*"))
  .filter((tag) => policy.RELEASE_TAG.test(tag))
  .filter((tag) => !maintenance || tag.startsWith("v" + maintenance[1] + "." + maintenance[2] + "."))
  .sort((left, right) => policy.compareVersions(right.slice(1), left.slice(1)));
const lastTag = tags.length > 0 ? tags[0] : null;

const records = git("log", "--format=%H%x00%B%x00", lastTag ? lastTag + "..HEAD" : "HEAD").split("\0");
const commits = [];
for (let index = 0; index + 1 < records.length; index += 2) {
  const hash = records[index].trim();
  if (hash) {
    commits.push({hash, message: records[index + 1]});
  }
}
const quiet = {log() {}, error() {}, success() {}, warn() {}};
const level = await analyzeCommits(analyzer[1], {cwd, branch: {name: branch}, commits, logger: quiet});
const version = policy.planVersion({branch, lastTag, level});

let tag = null;
let goTag = null;
if (version !== null) {
  tag = "v" + version;
  goTag = "go/" + tag;
  // A tag the branch does not contain may still exist elsewhere. A pull
  // request never publishes, and its base may be behind main's newest
  // release (a pull request into another pull request's branch, or a
  // re-run of an old check), so only releases stop at such a tag.
  if (trigger !== "pull_request") {
    for (const existing of [tag, goTag]) {
      if (lines(git("tag", "--list", existing)).length > 0) {
        throw new Error("tag " + existing + " already exists");
      }
    }
  }
}

const firstRelease = lastTag === null;
let publish = false;
let reason;
if (version === null) {
  reason = lastTag
    ? "no commit since " + lastTag + " calls for a release"
    : "no commit calls for a release";
} else if (trigger === "pull_request") {
  reason = "a pull request only checks the release build";
} else if (maintenance && !policy.MAINTENANCE_RELEASES) {
  reason = "releases from maintenance branches are off until those branches are protected like main";
} else if (firstRelease && trigger !== "workflow_dispatch") {
  reason = "the first release is started by hand: run the Release workflow on " + branch;
} else {
  publish = true;
  reason = "release " + tag + " from " + revision;
}

process.stdout.write(
  JSON.stringify({
    revision,
    branch,
    trigger,
    previous_tag: lastTag,
    commits: commits.length,
    level,
    version,
    tag,
    go_tag: goTag,
    first_release: firstRelease,
    latest: branch === "main",
    publish,
    reason,
  }) + "\n",
);
