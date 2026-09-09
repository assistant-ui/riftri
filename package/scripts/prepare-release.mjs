import { createHash } from "node:crypto";
import { chmod, copyFile, mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

import { stageRootPackage } from "./stage-root-package.mjs";

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const repositoryRoot = path.resolve(scriptDirectory, "..", "..");
const [tag, artifactsArgument = "artifacts"] = process.argv.slice(2);

if (!tag || !/^v\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(tag)) {
  throw new Error("usage: prepare-release.mjs v<semver> [artifacts-directory]");
}

const version = tag.slice(1);
const rootPackage = JSON.parse(
  await readFile(path.join(repositoryRoot, "package.json"), "utf8"),
);
if (rootPackage.version !== version) {
  throw new Error(
    `release tag ${tag} does not match package version ${rootPackage.version}`,
  );
}

const artifactsDirectory = path.resolve(repositoryRoot, artifactsArgument);
const releaseDirectory = path.join(repositoryRoot, "dist", "release");
await mkdir(releaseDirectory, { recursive: true });
const checksums = [];

for (const packageName of Object.keys(rootPackage.optionalDependencies).sort()) {
  const platformDirectory = path.join(
    repositoryRoot,
    "package",
    "platforms",
    packageName,
  );
  const manifest = JSON.parse(
    await readFile(path.join(platformDirectory, "package.json"), "utf8"),
  );
  if (manifest.version !== version) {
    throw new Error(`${packageName} is ${manifest.version}, expected ${version}`);
  }

  const executable = packageName.includes("win32") ? "riftri.exe" : "riftri";
  const source = path.join(artifactsDirectory, packageName, executable);
  const binDirectory = path.join(platformDirectory, "bin");
  const destination = path.join(binDirectory, executable);
  await mkdir(binDirectory, { recursive: true });
  await copyFile(source, destination);
  if (executable === "riftri") {
    await chmod(destination, 0o755);
  }

  const archive = path.join(releaseDirectory, `${packageName}-${tag}.tar.gz`);
  const tar = spawnSync("tar", ["-czf", archive, "-C", binDirectory, executable], {
    stdio: "inherit",
  });
  if (tar.status !== 0) {
    throw new Error(`could not create ${path.basename(archive)}`);
  }
  const contents = await readFile(archive);
  checksums.push(
    `${createHash("sha256").update(contents).digest("hex")}  ${path.basename(archive)}`,
  );
}

await writeFile(
  path.join(releaseDirectory, "SHA256SUMS"),
  `${checksums.join("\n")}\n`,
);
await stageRootPackage();
process.stdout.write(`staged ${checksums.length} native release archives for ${tag}\n`);
