import assert from "node:assert/strict";
import { readFile, readdir } from "node:fs/promises";
import path from "node:path";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const scriptPath = fileURLToPath(import.meta.url);
const defaultRepositoryRoot = path.resolve(path.dirname(scriptPath), "..", "..");

// The same table the launcher resolves with at runtime. Deriving the expected
// manifest constraints from it means a published package can never advertise a
// platform the launcher would not look for it on.
const { PLATFORM_PACKAGES, binaryName } = createRequire(scriptPath)(
  "../lib/platform.js",
);

// The launcher keys libc by Rust target spelling (`gnu`); npm's manifest
// field uses the implementation name (`glibc`). Mapping here keeps the two
// vocabularies from being conflated in the manifests themselves.
const NPM_LIBC = Object.freeze({ gnu: "glibc", musl: "musl" });

const PLATFORM_CONSTRAINTS = new Map(
  Object.entries(PLATFORM_PACKAGES).map(([key, packageName]) => {
    const [operatingSystem, cpu, libc] = key.split("-");
    if (libc) {
      assert.ok(NPM_LIBC[libc], `no npm libc spelling for ${libc}`);
    }
    return [packageName, { operatingSystem, cpu, libc: NPM_LIBC[libc] }];
  }),
);

async function readJson(...segments) {
  return JSON.parse(await readFile(path.join(...segments), "utf8"));
}

/**
 * Every crate must inherit the workspace version.
 *
 * `[workspace.package].version` is what this script compares against npm, but
 * the binary reports its own crate's `CARGO_PKG_VERSION`. A crate that pins a
 * literal version therefore ships inside an npm package claiming a different
 * one, and nothing downstream notices: the installed-package smoke test reads
 * `--version` without comparing it.
 */
async function checkCrateVersions(repositoryRoot) {
  const cratesDirectory = path.join(repositoryRoot, "crates");
  const crates = (await readdir(cratesDirectory, { withFileTypes: true }))
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name)
    .sort();
  assert.ok(crates.length > 0, "the workspace must contain crates");

  for (const crate of crates) {
    const manifest = await readFile(
      path.join(cratesDirectory, crate, "Cargo.toml"),
      "utf8",
    );
    assert.match(
      manifest,
      /^version\.workspace\s*=\s*true\s*$/m,
      `${crate} must inherit the workspace version, or its binary reports a version the npm package does not claim`,
    );
  }
  return crates;
}

export async function checkPackages({
  repositoryRoot = defaultRepositoryRoot,
} = {}) {
  const rootPackage = await readJson(repositoryRoot, "package.json");
  const packageLock = await readJson(repositoryRoot, "package-lock.json");
  const cargoManifest = await readFile(
    path.join(repositoryRoot, "Cargo.toml"),
    "utf8",
  );
  const cargoVersion = cargoManifest.match(
    /\[workspace\.package\][\s\S]*?\nversion\s*=\s*"([^"]+)"/,
  )?.[1];

  const platformsDirectory = path.join(repositoryRoot, "package", "platforms");
  const platformDirectories = (
    await readdir(platformsDirectory, { withFileTypes: true })
  )
    .filter((entry) => entry.isDirectory())
    .map((entry) => entry.name)
    .sort();
  const optionalPackages = Object.keys(
    rootPackage.optionalDependencies ?? {},
  ).sort();

  assert.equal(
    cargoVersion,
    rootPackage.version,
    "Cargo and npm versions must match",
  );
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

  const crates = await checkCrateVersions(repositoryRoot);

  assert.deepEqual(
    platformDirectories,
    optionalPackages,
    "platform workspaces must exactly match optionalDependencies",
  );

  for (const packageName of platformDirectories) {
    const manifest = await readJson(
      platformsDirectory,
      packageName,
      "package.json",
    );
    assert.equal(manifest.name, packageName, `${packageName} has the wrong name`);
    assert.equal(
      manifest.version,
      rootPackage.version,
      `${packageName} version must match riftri`,
    );
    assert.equal(
      rootPackage.optionalDependencies[packageName],
      `file:package/platforms/${packageName}`,
      `${packageName} source dependency must point to its local package`,
    );

    const expected = PLATFORM_CONSTRAINTS.get(packageName);
    assert.ok(
      expected,
      `${packageName} is not a platform the launcher resolves; add it to PLATFORM_PACKAGES`,
    );

    // npm silently skips an optional dependency whose os/cpu/libc exclude the
    // host, so a wrong value here does not fail an install -- it produces a
    // launcher that cannot find its binary. Only three of these eight packages
    // are ever installed in CI, so the other five need checking here.
    assert.deepEqual(
      manifest.os,
      [expected.operatingSystem],
      `${packageName} must declare os ${expected.operatingSystem}`,
    );
    assert.deepEqual(
      manifest.cpu,
      [expected.cpu],
      `${packageName} must declare cpu ${expected.cpu}`,
    );
    assert.deepEqual(
      manifest.libc,
      expected.libc ? [expected.libc] : undefined,
      `${packageName} must declare libc ${expected.libc ?? "(none)"}`,
    );

    const binary = `bin/${binaryName(expected.operatingSystem)}`;
    assert.ok(
      Array.isArray(manifest.files) && manifest.files.includes(binary),
      `${packageName} must ship ${binary}; a published tarball without it installs cleanly and then cannot run`,
    );
  }

  return {
    version: rootPackage.version,
    platforms: platformDirectories,
    crates,
  };
}

if (process.argv[1] && path.resolve(process.argv[1]) === scriptPath) {
  const { version, platforms, crates } = await checkPackages();
  process.stdout.write(
    `validated ${platforms.length} native package manifests and ${crates.length} crates at ${version}\n`,
  );
}
