import { readFile, readdir } from "node:fs/promises";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const scriptPath = fileURLToPath(import.meta.url);
const defaultRepositoryRoot = path.resolve(path.dirname(scriptPath), "..", "..");

// One source of truth with the launcher, so a name cannot be tolerated here
// while the runtime still tells that platform to reinstall.
const { UNPUBLISHED_PACKAGES } = createRequire(scriptPath)("../lib/platform.js");

function defaultRunNpm(arguments_, options = {}) {
  return spawnSync("npm", arguments_, options);
}

function defaultWait(milliseconds) {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

function commandOutput(result) {
  return `${result.stdout ?? ""}\n${result.stderr ?? ""}`;
}

async function packageManifest(packageDirectory) {
  return JSON.parse(
    await readFile(path.join(packageDirectory, "package.json"), "utf8"),
  );
}

async function releasePackages(repositoryRoot) {
  const platformsDirectory = path.join(repositoryRoot, "package", "platforms");
  const platformPackages = (
    await readdir(platformsDirectory, { withFileTypes: true })
  )
    .filter((entry) => entry.isDirectory())
    .map((entry) => path.join(platformsDirectory, entry.name))
    .sort();
  return [...platformPackages, path.join(repositoryRoot, "dist", "npm-root")];
}

function lookupExactVersion(runNpm, identifier) {
  const lookup = runNpm(["view", identifier, "version", "--json"], {
    encoding: "utf8",
  });
  if (lookup.error) throw lookup.error;
  if (lookup.status === 0) {
    let version;
    try {
      version = JSON.parse(lookup.stdout);
    } catch (error) {
      throw new Error(`npm returned invalid version metadata for ${identifier}`, {
        cause: error,
      });
    }
    return { published: true, version };
  }
  if (commandOutput(lookup).includes("E404")) {
    return { published: false };
  }
  if (lookup.stderr) process.stderr.write(lookup.stderr);
  throw new Error(`could not determine whether ${identifier} exists`);
}

export async function publishPackages({
  repositoryRoot = defaultRepositoryRoot,
  runNpm = defaultRunNpm,
  wait = defaultWait,
  verificationAttempts = 5,
} = {}) {
  if (!Number.isInteger(verificationAttempts) || verificationAttempts < 1) {
    throw new Error("verificationAttempts must be a positive integer");
  }
  const packages = await releasePackages(repositoryRoot);
  const expected = [];

  for (const packageDirectory of packages) {
    // Optional dependencies do not make a missing native package fail npm's
    // install step. Gate the public launcher on *visible* platform versions,
    // not merely successful publish exits, so first-time installs can run.
    if (packageDirectory === packages.at(-1)) {
      await verifyVersions(expected, { runNpm, wait, verificationAttempts });
    }
    const manifest = await packageManifest(packageDirectory);
    const identifier = `${manifest.name}@${manifest.version}`;
    expected.push({ identifier, version: manifest.version });
    const lookup = lookupExactVersion(runNpm, identifier);

    if (lookup.published) {
      if (lookup.version !== manifest.version) {
        throw new Error(
          `${identifier} resolved to unexpected registry version ${JSON.stringify(lookup.version)}`,
        );
      }
      process.stdout.write(`${identifier} is already published; skipping\n`);
      continue;
    }

    const distributionTag = manifest.version.includes("-") ? "next" : "latest";
    const publish = runNpm(
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
    if (publish.error) throw publish.error;
    if (publish.status !== 0) {
      // A name the registry has already refused must not strand the packages
      // queued behind it — that is how v0.2.1 shipped without a launcher. Try
      // it anyway each release, since that is how we learn it was unblocked,
      // but drop it from the expected set and keep going when it fails again.
      if (!UNPUBLISHED_PACKAGES[manifest.name]) {
        throw new Error(`publishing ${identifier} failed`);
      }
      expected.pop();
      process.stderr.write(
        `${identifier} was refused again; continuing without it. ` +
          `Remove it from UNPUBLISHED_PACKAGES in package/lib/platform.js ` +
          `once the registry accepts the name.\n`,
      );
    }
  }

  await verifyVersions(expected, { runNpm, wait, verificationAttempts });
  process.stdout.write(
    `verified ${expected.length} exact npm package versions after publication\n`,
  );
  return expected.map(({ identifier }) => identifier);
}

async function verifyVersions(expected, { runNpm, wait, verificationAttempts }) {
  let missing = [];
  for (let attempt = 1; attempt <= verificationAttempts; attempt += 1) {
    missing = [];
    for (const { identifier, version } of expected) {
      const lookup = lookupExactVersion(runNpm, identifier);
      if (!lookup.published) {
        missing.push(identifier);
        continue;
      }
      if (lookup.version !== version) {
        throw new Error(
          `${identifier} resolved to unexpected registry version ${JSON.stringify(lookup.version)}`,
        );
      }
    }
    if (missing.length === 0) break;
    if (attempt < verificationAttempts) {
      await wait(2 ** (attempt - 1) * 1_000);
    }
  }
  if (missing.length > 0) {
    throw new Error(
      `${missing.join(", ")} still missing after the publish verification retries`,
    );
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === scriptPath) {
  await publishPackages();
}
