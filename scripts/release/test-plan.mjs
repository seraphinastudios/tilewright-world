// Runs plan.mjs against small fixture repositories. Needs the pinned tools:
// run `npm ci --ignore-scripts` in this directory first.

import assert from "node:assert/strict";
import {execFileSync, spawnSync} from "node:child_process";
import {copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync} from "node:fs";
import {tmpdir} from "node:os";
import {dirname, join} from "node:path";
import {fileURLToPath} from "node:url";

const scripts = dirname(fileURLToPath(import.meta.url));
const repository = join(scripts, "../..");

function fixture({pre10 = true, maintenanceReleases = false} = {}) {
  const root = mkdtempSync(join(tmpdir(), "tilewright-world-plan-test-"));
  mkdirSync(join(root, "scripts/release"), {recursive: true});
  for (const name of ["plan.mjs", "release-rules.cjs"]) {
    copyFileSync(join(scripts, name), join(root, "scripts/release", name));
  }
  const policy = readFileSync(join(scripts, "release-policy.cjs"), "utf8");
  const marker = "const PRE_1_0 = true;";
  const maintenanceMarker = "const MAINTENANCE_RELEASES = false;";
  assert.ok(policy.includes(marker), "the policy sets PRE_1_0");
  assert.ok(policy.includes(maintenanceMarker), "the policy sets MAINTENANCE_RELEASES");
  let adjusted = pre10 ? policy : policy.replace(marker, "const PRE_1_0 = false;");
  if (maintenanceReleases) {
    adjusted = adjusted.replace(maintenanceMarker, "const MAINTENANCE_RELEASES = true;");
  }
  writeFileSync(join(root, "scripts/release/release-policy.cjs"), adjusted);
  symlinkSync(join(scripts, "node_modules"), join(root, "scripts/release/node_modules"), "dir");
  copyFileSync(join(repository, ".releaserc.json"), join(root, ".releaserc.json"));
  const git = (...args) => execFileSync("git", args, {cwd: root, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"]}).trim();
  git("init", "--quiet", "--initial-branch=main");
  git("config", "user.name", "Release fixture");
  git("config", "user.email", "release-fixture@example.invalid");
  git("config", "commit.gpgsign", "false");
  git("config", "tag.gpgsign", "false");
  git("add", ".");
  git("commit", "--quiet", "-m", "feat: start");
  const commit = (message) => {
    git("commit", "--quiet", "--allow-empty", "-m", message);
    return git("rev-parse", "HEAD");
  };
  const plan = (branch, trigger) => {
    const result = spawnSync(process.execPath, ["scripts/release/plan.mjs"], {
      cwd: root,
      encoding: "utf8",
      env: {...process.env, TILEWRIGHT_RELEASE_BRANCH: branch, TILEWRIGHT_RELEASE_TRIGGER: trigger},
    });
    if (result.status !== 0) {
      return {error: result.stderr};
    }
    return JSON.parse(result.stdout);
  };
  return {root, git, commit, plan, remove: () => rmSync(root, {recursive: true, force: true})};
}

function check(name, test) {
  const repo = fixture(test.options);
  try {
    test.run(repo);
  } catch (error) {
    error.message = name + ": " + error.message;
    throw error;
  } finally {
    repo.remove();
  }
}

check("first release", {
  run({git, plan}) {
    const pushed = plan("main", "push");
    assert.equal(pushed.version, "0.1.0");
    assert.equal(pushed.tag, "v0.1.0");
    assert.equal(pushed.go_tag, "go/v0.1.0");
    assert.equal(pushed.previous_tag, null);
    assert.equal(pushed.first_release, true);
    assert.equal(pushed.publish, false, "merging does not cut the first release");
    assert.match(pushed.reason, /started by hand/);
    assert.equal(pushed.revision, git("rev-parse", "HEAD"));
    const dispatched = plan("main", "workflow_dispatch");
    assert.equal(dispatched.version, "0.1.0");
    assert.equal(dispatched.publish, true);
    assert.equal(dispatched.latest, true);
    assert.equal(plan("main", "pull_request").publish, false);
    assert.match(plan("main", "schedule").error, /TILEWRIGHT_RELEASE_TRIGGER/);
    assert.match(plan("topic", "workflow_dispatch").error, /unsupported release branch/);
  },
});

check("every merge to main releases a minor version", {
  run({git, commit, plan}) {
    git("tag", "v0.1.0");
    git("tag", "go/v0.1.0");
    commit("docs: explain");
    git("tag", "v9.9.9-rc.1");
    const pushed = plan("main", "push");
    assert.equal(pushed.previous_tag, "v0.1.0");
    assert.equal(pushed.commits, 1);
    assert.equal(pushed.version, "0.2.0");
    assert.equal(pushed.first_release, false);
    assert.equal(pushed.publish, true);
  },
});

check("breaking changes stay 0.x before format 1.0", {
  run({git, commit, plan}) {
    git("tag", "v0.1.0");
    commit("feat!: change the pack format");
    assert.equal(plan("main", "push").version, "0.2.0");
    git("tag", "v0.2.0");
    commit("fix: repair\n\nBREAKING CHANGE: readers must update");
    assert.equal(plan("main", "push").version, "0.3.0");
  },
});

check("format 1.0 releases 1.0.0", {
  options: {pre10: false},
  run({git, commit, plan}) {
    git("tag", "v0.9.0");
    commit("feat!: freeze world format 1.0");
    assert.equal(plan("main", "push").version, "1.0.0");
  },
});

check("PRE_1_0 refuses a 1.x history", {
  run({git, commit, plan}) {
    git("tag", "v1.0.0");
    commit("feat: add");
    assert.match(plan("main", "push").error, /PRE_1_0 is set/);
  },
});

check("untyped commits release nothing", {
  run({git, commit, plan}) {
    git("tag", "v0.1.0");
    commit("tidy up");
    const pushed = plan("main", "push");
    assert.equal(pushed.version, null);
    assert.equal(pushed.tag, null);
    assert.equal(pushed.publish, false);
    assert.match(pushed.reason, /no commit since v0\.1\.0/);
  },
});

check("a revert releases", {
  run({git, commit, plan}) {
    const reverted = commit("feat: add");
    git("tag", "v0.1.0");
    commit('Revert "feat: add"\n\nThis reverts commit ' + reverted + ".");
    assert.equal(plan("main", "push").version, "0.2.0");
  },
});

check("maintenance branches plan but do not publish while unprotected", {
  run({git, commit, plan}) {
    git("tag", "v0.1.0");
    git("checkout", "--quiet", "-b", "0.1.x");
    commit("fix: backport");
    for (const trigger of ["push", "workflow_dispatch"]) {
      const planned = plan("0.1.x", trigger);
      assert.equal(planned.version, "0.1.1");
      assert.equal(planned.publish, false);
      assert.match(planned.reason, /maintenance branches are off/);
    }
  },
});

check("maintenance branches release patches once turned on", {
  options: {maintenanceReleases: true},
  run({git, commit, plan}) {
    git("tag", "v0.1.0");
    git("checkout", "--quiet", "-b", "0.1.x");
    commit("fix: backport");
    const pushed = plan("0.1.x", "push");
    assert.equal(pushed.version, "0.1.1");
    assert.equal(pushed.latest, false);
    assert.equal(pushed.publish, true);
    commit("feat: too much for a maintenance branch");
    assert.match(plan("0.1.x", "push").error, /cannot come from maintenance branch/);
    assert.match(plan("0.2.x", "push").error, /has no v0\.2\.\* release/);
  },
});

check("a maintenance branch counts from its own line", {
  options: {maintenanceReleases: true},
  run({git, commit, plan}) {
    git("tag", "v0.1.0");
    git("checkout", "--quiet", "-b", "0.1.x");
    commit("fix: backport");
    git("checkout", "--quiet", "main");
    commit("feat: next");
    git("tag", "v0.2.0");
    git("checkout", "--quiet", "0.1.x");
    commit("fix: another");
    assert.equal(plan("0.1.x", "push").version, "0.1.1");
  },
});

check("an existing tag elsewhere stops the plan", {
  run({git, commit, plan}) {
    git("tag", "v0.1.0");
    git("checkout", "--quiet", "-b", "elsewhere");
    commit("feat: elsewhere");
    git("tag", "v0.2.0");
    git("checkout", "--quiet", "main");
    commit("feat: here");
    assert.match(plan("main", "push").error, /tag v0\.2\.0 already exists/);
    git("tag", "-d", "v0.2.0");
    git("tag", "go/v0.2.0", "elsewhere");
    assert.match(plan("main", "push").error, /tag go\/v0\.2\.0 already exists/);
  },
});

console.log("release planner: first release, main, maintenance, 1.0, revert and tag-conflict fixtures passed");
