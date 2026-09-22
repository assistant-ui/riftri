#!/usr/bin/env node
// Using Riftri as your default workspace layer, safely.
//
//   node examples/fallback-detection/run.mjs <repository>
//
// Optimized worktrees need APFS, Btrfs, reflink XFS, ReFS, or OverlayFS, so a
// harness that hard-requires Riftri breaks for users who have none of them.
// Detect once, use Riftri when it is available, fall back to plain Git when it
// is not. Both paths produce a real Git worktree.

import path from "node:path";
import { riftri, run } from "../lib/riftri.mjs";

const repository = path.resolve(process.argv[2] ?? ".");
const destination = path.join(path.dirname(repository), "task-detected");
const branch = "agent/detected";

/** Decide once, before committing to a path. `doctor` creates nothing. */
async function chooseBackend() {
  let report;
  try {
    report = await riftri(["doctor", "--destination", destination], {
      cwd: repository,
    });
  } catch (error) {
    if (error.code === "ENOENT") return { use: "git", why: "riftri not installed" };
    return { use: "git", why: `doctor failed: ${error.message}` };
  }

  if (!report.cow_backend_active) {
    return { use: "git", why: "no copy-on-write backend on this volume" };
  }
  if (!report.repository_enabled) {
    // Opting in is a one-time, per-repository decision. Do it explicitly
    // rather than silently enabling someone's repository behind their back.
    return { use: "riftri", enableFirst: true, why: "repository not yet enabled" };
  }
  return { use: "riftri", why: `${report.destination_readiness?.backend} ready` };
}

async function main() {
  const choice = await chooseBackend();
  console.log(`backend: ${choice.use} (${choice.why})`);

  if (choice.use === "git") {
    // The fallback is an ordinary worktree: more disk, same semantics.
    const code = await run("git", ["worktree", "add", destination, "-b", branch], {
      cwd: repository,
    });
    console.log(code === 0 ? "created with plain git" : `git failed: ${code}`);
    return;
  }

  if (choice.enableFirst) {
    await riftri(["enable"], { cwd: repository, json: false });
    console.log("enabled riftri for this repository");
  }

  try {
    const created = await riftri(
      ["worktree", "add", destination, "-b", branch],
      { cwd: repository },
    );
    console.log(`created with riftri: backend=${created.backend}`);
  } catch (error) {
    // A refusal at this point means this specific checkout cannot be
    // reproduced exactly — sparse checkout, submodules, custom filters. The
    // request changed nothing, so plain Git is a well-defined fallback.
    if (error.isPolicyRefusal) {
      console.log(`riftri refused (${error.receipt?.code}), falling back to git`);
      await run("git", ["worktree", "add", destination, "-b", branch], {
        cwd: repository,
      });
      console.log("created with plain git");
      return;
    }
    throw error;
  }
}

main().catch((error) => {
  console.error(error.message);
  process.exit(1);
});
