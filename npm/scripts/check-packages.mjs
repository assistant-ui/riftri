import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const repositoryRoot = path.resolve(scriptDirectory, "..", "..");
const rootPackage = JSON.parse(
  await readFile(path.join(repositoryRoot, "package.json"), "utf8"),
);
const packageLock = JSON.parse(
  await readFile(path.join(repositoryRoot, "package-lock.json"), "utf8"),
);
const cargoManifest = await readFile(
  path.join(repositoryRoot, "Cargo.toml"),
  "utf8",
);
const cargoVersion = cargoManifest.match(
  /\[workspace\.package\][\s\S]*?\nversion\s*=\s*"([^"]+)"/,
)?.[1];
const platformsDirectory = path.join(repositoryRoot, "npm", "platforms");
const platformDirectories = (
  await readdir(platformsDirectory, { withFileTypes: true })
)
  .filter((entry) => entry.isDirectory())
  .map((entry) => entry.name)
  .sort();
const optionalPackages = Object.keys(rootPackage.optionalDependencies).sort();

assert.equal(cargoVersion, rootPackage.version, "Cargo and npm versions must match");
assert.equal(
  packageLock.version,
  rootPackage.version,
  "package-lock version must match package.json",
);
assert.equal(
  packageLock.packages[""].version,
  rootPackage.version,
  "package-lock root package version must match package.json",
);

assert.deepEqual(
  platformDirectories,
  optionalPackages,
  "platform workspaces must exactly match optionalDependencies",
);

for (const packageName of platformDirectories) {
  const manifest = JSON.parse(
    await readFile(
      path.join(platformsDirectory, packageName, "package.json"),
      "utf8",
    ),
  );
  assert.equal(manifest.name, packageName, `${packageName} has the wrong name`);
  assert.equal(
    manifest.version,
    rootPackage.version,
    `${packageName} version must match riftri`,
  );
  assert.equal(
    rootPackage.optionalDependencies[packageName],
    `file:npm/platforms/${packageName}`,
    `${packageName} source dependency must point to its local package`,
  );
}

process.stdout.write(
  `validated ${platformDirectories.length} native package manifests at ${rootPackage.version}\n`,
);
