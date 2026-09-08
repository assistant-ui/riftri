import { readFile, readdir } from "node:fs/promises";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const repositoryRoot = path.resolve(scriptDirectory, "..", "..");
const platformsDirectory = path.join(repositoryRoot, "npm", "platforms");
const platformPackages = (await readdir(platformsDirectory, { withFileTypes: true }))
  .filter((entry) => entry.isDirectory())
  .map((entry) => path.join(platformsDirectory, entry.name))
  .sort();

for (const packageDirectory of [...platformPackages, repositoryRoot]) {
  const manifest = JSON.parse(
    await readFile(path.join(packageDirectory, "package.json"), "utf8"),
  );
  const identifier = `${manifest.name}@${manifest.version}`;
  const lookup = spawnSync("npm", ["view", identifier, "version", "--json"], {
    encoding: "utf8",
  });

  if (lookup.status === 0) {
    process.stdout.write(`${identifier} is already published; skipping\n`);
    continue;
  }
  if (!`${lookup.stdout}\n${lookup.stderr}`.includes("E404")) {
    process.stderr.write(lookup.stderr);
    throw new Error(`could not determine whether ${identifier} exists`);
  }

  const distributionTag = manifest.version.includes("-") ? "next" : "latest";
  const publish = spawnSync(
    "npm",
    [
      "publish",
      packageDirectory,
      "--access",
      "public",
      "--provenance",
      "--tag",
      distributionTag,
    ],
    { stdio: "inherit" },
  );
  if (publish.status !== 0) {
    throw new Error(`publishing ${identifier} failed`);
  }
}
