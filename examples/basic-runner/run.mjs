#!/usr/bin/env node
// The core harness loop: recover, create a workspace per task, run something
// in it, clean up.
//
//   node examples/basic-runner/run.mjs <repository> [task...]
//
// Start here. The other examples add one idea each on top of this shape.

import path from "node:path";
import { riftri, run } from "../lib/riftri.mjs";

const repository = path.resolve(process.argv[2] ?? ".");
const tasks = process.argv.slice(3);
const taskNames = tasks.length ? tasks : ["alpha", "beta"];

async function main() {
  // 1. Recover first. Your runner can die mid-operation, and repair resumes or
  //    rolls back whatever was interrupted. It is safe to run every startup.
  const recovery = await riftri(["repair"], { cwd: repository });
  if (recovery?.errors?.length) {
    console.error("repair needs attention:", recovery.errors);
    process.exit(1);
  }
  console.log(`repair: scanned ${recovery?.scanned ?? 0} operation(s)`);

  for (const task of taskNames) {
    const destination = path.join(path.dirname(repository), `task-${task}`);

    // 2. Create the workspace. The report tells you which backend was used and
    //    whether an existing immutable base was reused.
    let created;
    try {
      created = await riftri(
        ["worktree", "add", destination, "-b", `agent/${task}`],
        { cwd: repository },
      );
    } catch (error) {
      // Exit 3 means Riftri refused and changed nothing. Retrying the same
      // command fails identically, so surface it instead of looping.
      if (error.isPolicyRefusal) {
        console.error(`skip ${task}: ${error.message}`);
        continue;
      }
      throw error;
    }
    console.log(
      `created ${task}: backend=${created.backend} reused_base=${created.reused_base}`,
    );

    // 3. Do the work. Anything here is ordinary: it is a real Git worktree.
    await run("git", ["status", "--porcelain=v1"], { cwd: destination });

    // 4. Clean removal refuses a dirty worktree, which is what you want —
    //    it stops a crashed task's work from disappearing silently.
    try {
      await riftri(["worktree", "remove", destination], { cwd: repository });
      console.log(`removed ${task}`);
    } catch (error) {
      if (error.isPolicyRefusal) {
        console.warn(`kept ${task}: ${error.message}`);
        continue; // Left in place on purpose. Inspect it, do not force blindly.
      }
      throw error;
    }
  }

  // 5. Reclaim immutable bases nothing references any more. Without --apply
  //    this only reports a plan, so it is safe to run on a schedule.
  const collected = await riftri(["gc", "--apply", "--yes"], { cwd: repository });
  console.log(`gc: applied=${collected?.applied ?? false}`);
}

main().catch((error) => {
  if (error.isUsageError) console.error("usage error:", error.message);
  else if (error.isStorageFull) console.error("free disk space, then recover:", error.message);
  else if (error.needsRepair) console.error("run riftri repair:", error.message);
  else console.error(error.message);
  process.exit(1);
});
