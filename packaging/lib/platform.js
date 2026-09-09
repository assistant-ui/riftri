"use strict";

const fs = require("node:fs");
const path = require("node:path");

const PLATFORM_PACKAGES = Object.freeze({
  "darwin-arm64": "riftri-darwin-arm64",
  "darwin-x64": "riftri-darwin-x64",
  "linux-arm64-gnu": "riftri-linux-arm64-gnu",
  "linux-x64-gnu": "riftri-linux-x64-gnu",
  "win32-arm64": "riftri-win32-arm64",
  "win32-x64": "riftri-win32-x64",
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
    throw new Error(
      `the optional native package ${packageName} is missing; reinstall without --omit=optional, or build Riftri from source`,
    );
  }

  const candidate = path.join(path.dirname(packageJson), "bin", binaryName(platform));
  return assertUsableBinary(candidate, packageName);
}

module.exports = {
  PLATFORM_PACKAGES,
  binaryName,
  detectLinuxLibc,
  packageNameForPlatform,
  platformKey,
  resolveBinary,
};
