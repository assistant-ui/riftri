import { createHash } from "node:crypto";
import { chmod, copyFile, mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";

import { stageRootPackage } from "./stage-root-package.mjs";

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const defaultRepositoryRoot = path.resolve(scriptDirectory, "..", "..");

export async function prepareRelease(options) {
  const {
    tag,
    artifactsDirectory: artifactsOption = "artifacts",
    repositoryRoot: repositoryOption = defaultRepositoryRoot,
    platformsDirectory: platformsOption,
    releaseDirectory: releaseOption,
    rootPackageDirectory: rootPackageOption,
  } = options;
  if (!tag || !/^v\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/.test(tag)) {
    throw new Error("release tag must be v<semver>");
  }

  const repositoryRoot = path.resolve(repositoryOption);
  const artifactsDirectory = path.resolve(repositoryRoot, artifactsOption);
  const platformsDirectory = path.resolve(
    platformsOption ?? path.join(repositoryRoot, "package", "platforms"),
  );
  const releaseDirectory = path.resolve(
    releaseOption ?? path.join(repositoryRoot, "dist", "release"),
  );
  const rootPackageDirectory = path.resolve(
    rootPackageOption ?? path.join(repositoryRoot, "dist", "npm-root"),
  );
  const version = tag.slice(1);
  const rootPackage = JSON.parse(
    await readFile(path.join(repositoryRoot, "package.json"), "utf8"),
  );
  if (rootPackage.version !== version) {
    throw new Error(
      `release tag ${tag} does not match package version ${rootPackage.version}`,
    );
  }

  await mkdir(releaseDirectory, { recursive: true });
  const checksums = [];

  for (const packageName of Object.keys(rootPackage.optionalDependencies).sort()) {
    const platformDirectory = path.join(platformsDirectory, packageName);
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
    const tar = spawnSync(
      "tar",
      ["-czf", archive, "-C", binDirectory, executable],
      { encoding: "utf8" },
    );
    if (tar.error) {
      throw new Error(
        `could not run tar for ${path.basename(archive)}: ${tar.error.message}`,
      );
    }
    if (tar.status !== 0) {
      throw new Error(
        `could not create ${path.basename(archive)}: ${tar.stderr.trim()}`,
      );
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
  await stageRootPackage(rootPackageDirectory);
  return {
    nativePackageCount: checksums.length,
    releaseDirectory,
    rootPackageDirectory,
  };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [tag, artifactsArgument = "artifacts"] = process.argv.slice(2);
  if (!tag) {
    throw new Error("usage: prepare-release.mjs v<semver> [artifacts-directory]");
  }
  const result = await prepareRelease({
    tag,
    artifactsDirectory: artifactsArgument,
  });
  process.stdout.write(
    `staged ${result.nativePackageCount} native release archives for ${tag}\n`,
  );
}
