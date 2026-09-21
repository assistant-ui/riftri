"use strict";

const assert = require("node:assert/strict");
const { spawnSync } = require("node:child_process");
const { createHash } = require("node:crypto");
const {
  mkdtemp,
  mkdir,
  readFile,
  rm,
  symlink,
  writeFile,
} = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");

function jobSource(workflow, name) {
  const start = workflow.indexOf(`  ${name}:\n`);
  assert.notEqual(start, -1, `missing ${name} job`);
  const rest = workflow.slice(start + 1);
  const nextJob = rest.search(/^  [a-z][a-z0-9_-]*:\n/m);
  return nextJob === -1
    ? workflow.slice(start)
    : workflow.slice(start, start + 1 + nextJob);
}

function stepRun(job, name) {
  const start = job.indexOf(`      - name: ${name}\n`);
  assert.notEqual(start, -1, `missing ${name} step`);
  const step = job.slice(start).split(/\n      - name:/, 1)[0];
  const run = step.match(/        run: \|\n((?:          .*\n|\n)*)/);
  assert.ok(run, `missing multiline run in ${name}`);
  return run[1].replace(/^          /gm, "");
}

async function releaseWorkflow() {
  return readFile(
    path.resolve(__dirname, "..", "..", ".github", "workflows", "release.yml"),
    "utf8",
  );
}

async function rootCargoToml() {
  return readFile(
    path.resolve(__dirname, "..", "..", "Cargo.toml"),
    "utf8",
  );
}

test("release binaries ship the tui feature and are stripped", async () => {
  const workflow = await releaseWorkflow();
  const build = jobSource(workflow, "build");
  const buildStep = build.match(/      - name: Build release binary\n        run: (.*)\n/);
  assert.ok(buildStep, "missing single-line release build run");
  const command = buildStep[1];
  // #290 gated the styled setup TUI behind an off-by-default `tui` feature.
  // Published archives must keep building it, or the release silently ships the
  // plain UI. Guard the exact release-build flags so a regression fails here.
  assert.match(command, /\bcargo build\b/);
  assert.match(command, /--release\b/);
  assert.match(command, /--locked\b/);
  assert.match(command, /-p riftri-cli\b/);
  assert.match(command, /--features tui\b/);

  // The same change strips release binaries; losing this quietly inflates every
  // published archive, so pin it in the workspace release profile.
  const cargoToml = await rootCargoToml();
  const releaseProfile = cargoToml.match(/\[profile\.release\]\n((?:[^\[].*\n?)*)/);
  assert.ok(releaseProfile, "missing [profile.release] in root Cargo.toml");
  assert.match(releaseProfile[1], /^strip = true$/m);
});

test("manual release rehearsals cannot receive publishing permissions", async () => {
  const workflow = await releaseWorkflow();
  const stage = jobSource(workflow, "stage");
  const publish = jobSource(workflow, "publish");
  const githubRelease = jobSource(workflow, "github-release");

  assert.match(workflow, /^  workflow_dispatch:\s*$/m);

  assert.match(stage, /Stage npm packages and GitHub assets/);
  assert.match(stage, /Confirm non-publishing rehearsal/);
  assert.doesNotMatch(stage, /contents: write/);
  assert.doesNotMatch(stage, /id-token: write/);
  assert.doesNotMatch(stage, /Publish npm packages/);
  assert.doesNotMatch(stage, /gh release/);

  assert.match(publish, /^    if: github\.event_name == 'push' && vars\.NPM_PUBLISH_ENABLED == 'true'$/m);
  assert.match(publish, /^      contents: read$/m);
  assert.match(publish, /^      id-token: write$/m);
  assert.match(publish, /Publish npm packages/);
  assert.doesNotMatch(publish, /contents: write|gh release/);

  assert.match(githubRelease, /^    if: github\.event_name == 'push'$/m);
  assert.match(githubRelease, /^      contents: write$/m);
  // id-token is granted solely so the job can sign artifact attestations;
  // npm publishing credentials must never appear here.
  assert.match(githubRelease, /^      id-token: write$/m);
  assert.match(githubRelease, /^      attestations: write$/m);
  assert.match(githubRelease, /Attest build provenance for release assets/);
  assert.doesNotMatch(githubRelease, /NPM_TOKEN|npm publish|registry\.npmjs\.org/);
});

test("npm publication stays paused until registry review explicitly enables it", async () => {
  const workflow = await releaseWorkflow();
  const publish = jobSource(workflow, "publish");
  const githubRelease = jobSource(workflow, "github-release");
  assert.match(publish, /^    if: github\.event_name == 'push' && vars\.NPM_PUBLISH_ENABLED == 'true'$/m);
  assert.doesNotMatch(githubRelease, /NPM_PUBLISH_ENABLED/);
});

test("direct downloads publish independently of npm from the inspected staged assets", async () => {
  const workflow = await releaseWorkflow();
  const stage = jobSource(workflow, "stage");
  const publish = jobSource(workflow, "publish");
  const githubRelease = jobSource(workflow, "github-release");

  assert.match(publish, /^    needs: stage$/m);
  assert.match(githubRelease, /^    needs: stage$/m);
  assert.match(stage, /Verify GitHub release checksums[\s\S]*Upload GitHub release assets/);
  assert.match(stage, /name: github-release-assets\n          path: dist\/release\//);
  assert.match(githubRelease, /name: github-release-assets\n          path: dist\/release/);
  assert.doesNotMatch(githubRelease, /prepare-release\.mjs|needs:.*publish/);
  assert.match(githubRelease, /Verify exact GitHub release assets[\s\S]*Create GitHub release/);
  const create = stepRun(githubRelease, "Create GitHub release");
  assert.match(create, /--verify-tag/);
  assert.match(create, /--prerelease/);
  assert.match(create, /gh release create/);
  assert.doesNotMatch(create, /--clobber|gh release (?:upload|delete|edit)|git (?:tag|push)/);
});

test("direct-download validation rejects missing, extra, corrupt, and unsafe assets", async (t) => {
  const workflow = await releaseWorkflow();
  const validate = stepRun(
    jobSource(workflow, "github-release"),
    "Verify exact GitHub release assets",
  );
  const nodeScript = /^node <<'NODE'\n([\s\S]*)\nNODE\s*$/.exec(validate);
  assert.ok(nodeScript, "asset verification must run the tested Node script");
  const root = await mkdtemp(path.join(os.tmpdir(), "riftri-download-assets-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const tag = "v0.1.1";
  const names = [
    "riftri-darwin-arm64",
    "riftri-darwin-x64",
    "riftri-linux-arm64-gnu",
    "riftri-linux-arm64-musl",
    "riftri-linux-x64-gnu",
    "riftri-linux-x64-musl",
    "riftri-win32-arm64",
    "riftri-win32-x64",
  ].map((name) => `${name}-${tag}.tar.gz`);
  const entries = names.map((name) => {
    const contents = Buffer.from(`fixture:${name}\n`);
    return {
      name,
      contents,
      checksum: createHash("sha256").update(contents).digest("hex"),
    };
  });
  const sums = entries.map(({ name, checksum }) => `${checksum}  ${name}\n`).join("");

  async function fixture(label, mutate, shouldPass = false) {
    const directory = path.join(root, label);
    await mkdir(directory);
    for (const { name, contents } of entries) {
      await writeFile(path.join(directory, name), contents);
    }
    await writeFile(path.join(directory, "SHA256SUMS"), sums);
    if (mutate) await mutate(directory);
    const result = spawnSync(process.execPath, ["-e", nodeScript[1]], {
      cwd: directory,
      env: { ...process.env, GITHUB_REF_NAME: tag },
      encoding: "utf8",
    });
    assert.ifError(result.error);
    if (shouldPass) assert.equal(result.status, 0, result.stderr);
    else assert.notEqual(result.status, 0, `${label} unexpectedly passed`);
  }

  await fixture("valid", null, true);
  await fixture("missing", (dir) => rm(path.join(dir, names[0])));
  await fixture("extra", (dir) =>
    writeFile(path.join(dir, "unexpected.tar.gz"), "extra"),
  );
  await fixture("extra-directory", (dir) => mkdir(path.join(dir, "nested")));
  await fixture("corrupt", (dir) => writeFile(path.join(dir, names[0]), "changed"));
  await fixture("extra-checksum", (dir) =>
    writeFile(path.join(dir, "SHA256SUMS"), sums + sums.split("\n")[0] + "\n"),
  );
  await fixture("duplicate-checksum", (dir) =>
    writeFile(
      path.join(dir, "SHA256SUMS"),
      sums.replace(sums.split("\n")[5], sums.split("\n")[0]),
    ),
  );
  await fixture("missing-checksum", (dir) =>
    writeFile(path.join(dir, "SHA256SUMS"), sums.split("\n").slice(1).join("\n")),
  );
  await fixture("malformed-checksum", (dir) =>
    writeFile(path.join(dir, "SHA256SUMS"), sums.replace(entries[0].checksum, "invalid")),
  );
  await fixture("unexpected-checksum", (dir) =>
    writeFile(path.join(dir, "SHA256SUMS"), sums.replace(names[0], "unexpected.tar.gz")),
  );
  await fixture("path-traversal", (dir) =>
    writeFile(path.join(dir, "SHA256SUMS"), sums.replace(names[0], `../${names[0]}`)),
  );
  if (process.platform !== "win32") {
    await fixture("symlink-archive", async (dir) => {
      await rm(path.join(dir, names[0]));
      await symlink(path.join(root, "valid", names[0]), path.join(dir, names[0]));
    });
    await fixture("symlink-checksums", async (dir) => {
      await rm(path.join(dir, "SHA256SUMS"));
      await symlink(path.join(root, "valid", "SHA256SUMS"), path.join(dir, "SHA256SUMS"));
    });
  }
});
