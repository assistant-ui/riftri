// Regenerates every checked-in copy of the Homebrew formula from a published
// release's SHA256SUMS manifest, or verifies them without writing (--check).
//
//   node package/scripts/sync-homebrew-tap.mjs            # sync to the latest release
//   node package/scripts/sync-homebrew-tap.mjs v0.2.3     # sync to a specific tag
//   node package/scripts/sync-homebrew-tap.mjs --check    # fail on drift, write nothing
//
// Unlike update-homebrew-formula.mjs, which reads a local SHA256SUMS file, this
// script downloads the manifest from the release itself, so what it pins is by
// construction what the release publishes.

import { mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { pathToFileURL } from "node:url";

import {
  formulaPaths,
  parseChecksums,
  renderHomebrewFormula,
} from "./update-homebrew-formula.mjs";

const repository = "assistant-ui/riftri";

/** Accept `1.2.3` or `v1.2.3`; return the bare version. */
export function normalizeVersion(input) {
  const match = /^v?(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)$/.exec(input);
  if (!match) {
    throw new Error(`expected a release version such as v0.2.3, received ${input}`);
  }
  return match[1];
}

function requestHeaders() {
  const headers = { "user-agent": `${repository} homebrew sync` };
  const token = process.env.GH_TOKEN || process.env.GITHUB_TOKEN;
  if (token) {
    headers.authorization = `Bearer ${token}`;
  }
  return headers;
}

async function fetchOrThrow(url, { fetchImpl = fetch, ...options } = {}) {
  const response = await fetchImpl(url, { headers: requestHeaders(), ...options });
  if (!response.ok) {
    throw new Error(`${url} responded ${response.status} ${response.statusText}`);
  }
  return response;
}

/** Resolve the latest published release version from the GitHub API. */
export async function resolveLatestVersion({ fetchImpl = fetch } = {}) {
  const response = await fetchOrThrow(
    `https://api.github.com/repos/${repository}/releases/latest`,
    { fetchImpl },
  );
  const release = await response.json();
  if (typeof release.tag_name !== "string") {
    throw new Error("the latest-release response did not include a tag name");
  }
  return normalizeVersion(release.tag_name);
}

/** Download and parse the SHA256SUMS manifest published with a release tag. */
export async function fetchReleaseChecksums(version, { fetchImpl = fetch } = {}) {
  const url = `https://github.com/${repository}/releases/download/v${version}/SHA256SUMS`;
  const response = await fetchOrThrow(url, { fetchImpl });
  return parseChecksums(await response.text());
}

/**
 * Render the formula for `version` from its published checksums, then either
 * write every checked-in copy or, with `check`, compare byte-for-byte and
 * return the paths that drift.
 */
export async function syncHomebrewTap({
  version,
  check = false,
  targets = formulaPaths,
  fetchImpl = fetch,
} = {}) {
  const checksums = await fetchReleaseChecksums(version, { fetchImpl });
  const formula = renderHomebrewFormula({ version, checksums });
  const drifted = [];
  for (const target of targets) {
    const existing = await readFile(target, "utf8").catch(() => null);
    if (existing === formula) {
      continue;
    }
    if (check) {
      drifted.push(target);
    } else {
      await mkdir(path.dirname(target), { recursive: true });
      await writeFile(target, formula);
    }
  }
  return { version, targets, drifted };
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  const args = process.argv.slice(2);
  const check = args.includes("--check");
  const positional = args.filter((argument) => argument !== "--check");
  if (positional.length > 1) {
    throw new Error("usage: node package/scripts/sync-homebrew-tap.mjs [--check] [version]");
  }
  const version = positional.length
    ? normalizeVersion(positional[0])
    : await resolveLatestVersion();

  const { drifted, targets } = await syncHomebrewTap({ version, check });
  if (check) {
    for (const target of drifted) {
      process.stderr.write(
        `${target} does not match the v${version} release; run this script without --check.\n`,
      );
    }
    if (drifted.length) {
      process.exit(1);
    }
    process.stdout.write(`Every formula copy matches the v${version} release.\n`);
  } else {
    process.stdout.write(`Synced to v${version}:\n${targets.join("\n")}\n`);
  }
}
