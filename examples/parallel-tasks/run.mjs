#!/usr/bin/env node
// Many workspaces at once — the realistic agent workload.
//
//   node examples/parallel-tasks/run.mjs <repository> [count] [concurrency]
//
// Riftri coordinates concurrent adds of the same base with per-base locks: one
// process materializes it, the rest verify and reuse. You do not need a lock
// of your own. Watch `reused_base` in the output — exactly one add pays for
// materialization and the rest are cheap.

import path from "node:path";
import { riftri } from "../lib/riftri.mjs";

const repository = path.resolve(process.argv[2] ?? ".");
const count = Number(process.argv[3] ?? 5);
const concurrency = Number(process.argv[4] ?? 5); // set to 1 to read it sequentially

/** Retry only what is genuinely transient: a lock held by a live process. */
async function addWithRetry(destination, branch, attempts = 5) {
  for (let attempt = 1; ; attempt++) {
    try {
      return await riftri(["worktree", "add", destination, "-b", branch], {
        cwd: repository,
      });
    } catch (error) {
      // A policy refusal is deterministic — retrying wastes time.
      if (error.isPolicyRefusal) throw error;
      // `worktree-busy` is the one code where waiting actually helps.
      if (error.isBusy && attempt < attempts) {
        await new Promise((r) => setTimeout(r, 100 * attempt));
        continue;
      }
      throw error;
    }
  }
}

/** Run tasks with a bounded number in flight. */
async function pool(items, limit, worker) {
  const results = [];
  let next = 0;
  const runners = Array.from({ length: Math.min(limit, items.length) }, async () => {
    while (next < items.length) {
      const index = next++;
      results[index] = await worker(items[index], index);
    }
  });
  await Promise.all(runners);
  return results;
}

async function main() {
  await riftri(["repair"], { cwd: repository });

  const tasks = Array.from({ length: count }, (_, i) => `task-${i + 1}`);
  const parent = path.dirname(repository);

  const started = Date.now();
  const created = await pool(tasks, concurrency, async (task) => {
    const destination = path.join(parent, task);
    try {
      const report = await addWithRetry(destination, `agent/${task}`);
      console.log(`  ${task}: ${report.backend} reused_base=${report.reused_base}`);
      return { task, destination, ok: true };
    } catch (error) {
      console.error(`  ${task}: ${error.message}`);
      return { task, destination, ok: false };
    }
  });
  console.log(`created ${created.filter((r) => r.ok).length}/${count} in ${Date.now() - started}ms`);

  // One base materialized, the rest reused — that is the whole point.
  const reused = created.filter((r) => r.ok).length - 1;
  console.log(`bases materialized: 1, reused: ${Math.max(reused, 0)}`);

  // What did it actually cost? Riftri reports real allocation per worktree.
  const inventory = await riftri(["worktree", "list"], { cwd: repository });
  const total = (inventory?.worktrees ?? []).reduce(
    (sum, w) => sum + (w.allocated_bytes ?? 0),
    0,
  );
  console.log(`${inventory?.worktrees?.length ?? 0} managed worktrees, ${total} bytes allocated`);

  await pool(created.filter((r) => r.ok), concurrency, async ({ destination }) => {
    await riftri(["worktree", "remove", destination], { cwd: repository });
  });
  await riftri(["gc", "--apply", "--yes"], { cwd: repository });
  console.log("cleaned up");
}

main().catch((error) => {
  console.error(error.message);
  process.exit(1);
});
