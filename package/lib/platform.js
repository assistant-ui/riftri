"use strict";

const fs = require("node:fs");
const path = require("node:path");

const PLATFORM_PACKAGES = Object.freeze({
  "darwin-arm64": "riftri-darwin-arm64",
  "darwin-x64": "riftri-darwin-x64",
  "linux-arm64-gnu": "riftri-linux-arm64-gnu",
  "linux-arm64-musl": "riftri-linux-arm64-musl",
  "linux-x64-gnu": "riftri-linux-x64-gnu",
  "linux-x64-musl": "riftri-linux-x64-musl",
  "win32-arm64": "riftri-win32-arm64",
  "win32-x64": "riftri-win32-x64",
});

// Native packages the registry has refused, mapped to the route that does work.
// A name only belongs here while it is genuinely absent from npm; remove the
// entry in the release that first publishes it.
const UNPUBLISHED_PACKAGES = Object.freeze({
  "riftri-win32-arm64":
    "install it with the PowerShell installer instead: " +
    "https://riftri.dev/docs/installation",
});

function detectLinuxLibc(report = process.report) {
  if (!report || typeof report.getReport !== "function") {
    return "unknown";
  }

  const header = report.getReport()?.header;
  return header?.glibcVersionRuntime ? "gnu" : "musl";
}

function platformKey(
  platform = process.platform,
  architecture = process.arch,
  report = process.report,
) {
  if (platform === "linux") {
    return `${platform}-${architecture}-${detectLinuxLibc(report)}`;
  }
  return `${platform}-${architecture}`;
}

function packageNameForPlatform(platform, architecture, report) {
  return PLATFORM_PACKAGES[platformKey(platform, architecture, report)] ?? null;
}

function binaryName(platform = process.platform) {
  return platform === "win32" ? "riftri.exe" : "riftri";
}

function assertUsableBinary(candidate, source) {
  try {
    fs.accessSync(
      candidate,
      process.platform === "win32" ? fs.constants.F_OK : fs.constants.X_OK,
    );
  } catch {
    throw new Error(`${source} does not contain an executable Riftri binary: ${candidate}`);
  }
  return candidate;
}

function launcherVersion() {
  for (const candidate of [
    path.join(__dirname, "..", "package.json"),
    path.join(__dirname, "..", "..", "package.json"),
  ]) {
    try {
      const manifest = JSON.parse(fs.readFileSync(candidate, "utf8"));
      if (manifest.name === "riftri" && typeof manifest.version === "string") {
        return manifest.version;
      }
    } catch {
      // The source and staged package layouts put the root manifest at different depths.
    }
  }
  throw new Error("could not read the riftri launcher version");
}

function assertCompatiblePackageManifest(manifest, packageName, expectedVersion) {
  if (manifest.name !== packageName || manifest.version !== expectedVersion) {
    throw new Error(
      `the optional native package ${packageName} does not match riftri ${expectedVersion}; reinstall riftri and its optional dependencies together`,
    );
  }
}

function resolveBinary(options = {}) {
  const environment = options.environment ?? process.env;
  const platform = options.platform ?? process.platform;
  const architecture = options.architecture ?? process.arch;
  const report = options.report ?? process.report;

  if (environment.RIFTRI_BINARY) {
    const override = path.resolve(environment.RIFTRI_BINARY);
    return assertUsableBinary(override, "RIFTRI_BINARY");
  }

  const packageName = packageNameForPlatform(platform, architecture, report);
  if (!packageName) {
    const key = platformKey(platform, architecture, report);
    throw new Error(
      `no prebuilt native package is available for ${key}; supported packages: ${Object.keys(PLATFORM_PACKAGES).join(", ")}`,
    );
  }

  let packageJson;
  try {
    packageJson = require.resolve(`${packageName}/package.json`);
  } catch {
    // Reinstalling cannot help when the package was never published, so say so
    // instead of sending the user around the same loop.
    const unpublished = UNPUBLISHED_PACKAGES[packageName];
    throw new Error(
      unpublished
        ? `${packageName} is not published to npm, so riftri cannot run on this platform through npm; ${unpublished}`
        : `the optional native package ${packageName} is missing; reinstall without --omit=optional, or build Riftri from source`,
    );
  }

  let packageManifest;
  try {
    packageManifest = JSON.parse(fs.readFileSync(packageJson, "utf8"));
  } catch {
    throw new Error(`the optional native package ${packageName} has an invalid manifest`);
  }
  assertCompatiblePackageManifest(
    packageManifest,
    packageName,
    options.expectedVersion ?? launcherVersion(),
  );

  const candidate = path.join(path.dirname(packageJson), "bin", binaryName(platform));
  return assertUsableBinary(candidate, packageName);
}

module.exports = {
  PLATFORM_PACKAGES,
  UNPUBLISHED_PACKAGES,
  assertCompatiblePackageManifest,
  binaryName,
  detectLinuxLibc,
  packageNameForPlatform,
  platformKey,
  resolveBinary,
};
