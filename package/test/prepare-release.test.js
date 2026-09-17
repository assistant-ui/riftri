"use strict";

const assert = require("node:assert/strict");
const { spawnSync } = require("node:child_process");
const { createHash } = require("node:crypto");
const {
  access,
  cp,
  mkdir,
  mkdtemp,
  readFile,
  readdir,
  rm,
  stat,
  writeFile,
} = require("node:fs/promises");
const os = require("node:os");
const path = require("node:path");
const { test } = require("node:test");

test("stages native packages and the launcher from the selected repository offline", async (t) => {
  const repositoryRoot = path.resolve(__dirname, "..", "..");
  const temporary = await mkdtemp(path.join(os.tmpdir(), "riftri-release-"));
  t.after(() => rm(temporary, { recursive: true, force: true }));
  const artifactsDirectory = path.join(temporary, "artifacts");
  const platformsDirectory = path.join(temporary, "platforms");
  const releaseDirectory = path.join(temporary, "release");
  const rootPackageDirectory = path.join(temporary, "npm-root");
  const selectedRepository = path.join(temporary, "selected");
  await mkdir(selectedRepository);
  const manifest = JSON.parse(
    await readFile(path.join(repositoryRoot, "package.json"), "utf8"),
  );
  manifest.version = "9.9.9";
  await writeFile(
    path.join(selectedRepository, "package.json"),
    JSON.stringify(manifest),
  );
  const selectedFiles = [
    ["README.md", "Selected repository README\n"],
    ["LICENSE", "Selected repository license\n"],
    ["package/bin/riftri.js", "console.log('selected launcher');\n"],
    ["package/lib/platform.js", "module.exports = 'selected platform';\n"],
  ];
  for (const [name, contents] of selectedFiles) {
    await mkdir(path.dirname(path.join(selectedRepository, name)), {
      recursive: true,
    });
    await writeFile(path.join(selectedRepository, name), contents);
  }
  const packageNames = Object.keys(manifest.optionalDependencies).sort();

  await cp(
    path.join(repositoryRoot, "package", "platforms"),
    platformsDirectory,
    {
      recursive: true,
    },
  );
  for (const packageName of packageNames) {
    const nativeManifestPath = path.join(
      platformsDirectory,
      packageName,
      "package.json",
    );
    const nativeManifest = JSON.parse(
      await readFile(nativeManifestPath, "utf8"),
    );
    nativeManifest.version = manifest.version;
    await writeFile(nativeManifestPath, JSON.stringify(nativeManifest));
    const executable = packageName.includes("win32") ? "riftri.exe" : "riftri";
    const artifactDirectory = path.join(artifactsDirectory, packageName);
    await mkdir(artifactDirectory, { recursive: true });
    await writeFile(
      path.join(artifactDirectory, executable),
      `native:${packageName}\n`,
    );
  }

  const { prepareRelease } = await import("../scripts/prepare-release.mjs");
  const result = await prepareRelease({
    repositoryRoot: selectedRepository,
    tag: `v${manifest.version}`,
    artifactsDirectory,
    platformsDirectory,
    releaseDirectory,
    rootPackageDirectory,
  });

  assert.equal(packageNames.length, 8);
  assert.equal(result.nativePackageCount, 8);
  assert.equal(result.releaseDirectory, releaseDirectory);
  assert.equal(result.rootPackageDirectory, rootPackageDirectory);
  assert.deepEqual(
    (await readdir(releaseDirectory)).sort(),
    [
      "SHA256SUMS",
      ...packageNames.map(
        (packageName) => `${packageName}-v${manifest.version}.tar.gz`,
      ),
    ].sort(),
  );

  const checksumLines = (
    await readFile(path.join(releaseDirectory, "SHA256SUMS"), "utf8")
  )
    .trim()
    .split("\n");
  assert.equal(checksumLines.length, packageNames.length);
  for (const [index, packageName] of packageNames.entries()) {
    const archiveName = `${packageName}-v${manifest.version}.tar.gz`;
    const archive = await readFile(path.join(releaseDirectory, archiveName));
    assert.equal(
      checksumLines[index],
      `${createHash("sha256").update(archive).digest("hex")}  ${archiveName}`,
    );

    const executable = packageName.includes("win32") ? "riftri.exe" : "riftri";
    const listing = spawnSync(
      "tar",
      ["-tzf", path.join(releaseDirectory, archiveName)],
      { encoding: "utf8" },
    );
    assert.equal(listing.status, 0, listing.stderr);
    assert.equal(listing.stdout.trim(), executable);
    assert.equal(
      await readFile(
        path.join(platformsDirectory, packageName, "bin", executable),
        "utf8",
      ),
      `native:${packageName}\n`,
    );
    if (process.platform !== "win32" && executable === "riftri") {
      assert.notEqual(
        (
          await stat(
            path.join(platformsDirectory, packageName, "bin", executable),
          )
        ).mode & 0o111,
        0,
      );
    }
  }

  const stagedManifest = JSON.parse(
    await readFile(path.join(rootPackageDirectory, "package.json"), "utf8"),
  );
  assert.equal(stagedManifest.name, "riftri");
  assert.equal(stagedManifest.version, "9.9.9");
  for (const version of Object.values(stagedManifest.optionalDependencies)) {
    assert.equal(version, "9.9.9");
  }
  for (const [name, contents] of selectedFiles) {
    assert.equal(
      await readFile(
        path.join(rootPackageDirectory, name.replace(/^package\//, "")),
        "utf8",
      ),
      contents,
    );
  }
  assert.equal(stagedManifest.bin.riftri, "bin/riftri.js");
  await access(path.join(rootPackageDirectory, "bin", "riftri.js"));
  await access(path.join(rootPackageDirectory, "lib", "platform.js"));
});
