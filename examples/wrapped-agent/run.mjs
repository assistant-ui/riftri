#!/usr/bin/env node
// Optimizing an agent you do not control.
//
//   node examples/wrapped-agent/run.mjs <repository>
//
// The other examples call Riftri directly. This is the other shape: the agent
// creates its own worktree with plain `git worktree add`, and you wrap it in
// `riftri exec` so that call takes the optimized path. The agent is unchanged
// and unaware — see agent.sh, which only knows Git.

import path from "node:path";
import { fileURLToPath } from "node:url";
import { riftri, run } from "../lib/riftri.mjs";

const here = path.dirname(fileURLToPath(import.meta.url));
const repository = path.resolve(process.argv[2] ?? ".");
const destination = path.join(path.dirname(repository), "task-wrapped");

async function main() {
  await riftri(["enable"], { cwd: repository, json: false });

  // Everything inside this process tree sees a Git shim on PATH. `git worktree
  // add` is optimized; every other Git command is delegated unchanged.
  console.log("running the agent under riftri exec:");
  const code = await run(
    "riftri",
    ["exec", "--", path.join(here, "agent.sh"), destination, "agent/wrapped"],
    { cwd: repository },
  );
  if (code !== 0) throw new Error(`agent exited ${code}`);

  // The agent never mentioned Riftri, yet the worktree it made is managed.
  const inventory = await riftri(["worktree", "list"], { cwd: repository });
  const managed = (inventory?.worktrees ?? []).find((w) =>
    w.path.endsWith("task-wrapped"),
  );
  console.log(
    managed
      ? `riftri manages it: backend=${managed.backend} allocated=${managed.allocated_bytes}B`
      : "not managed — was the repository enabled?",
  );

  await riftri(["worktree", "remove", destination], { cwd: repository });
  await riftri(["gc", "--apply", "--yes"], { cwd: repository });
  console.log("cleaned up");
}

main().catch((error) => {
  console.error(error.message);
  process.exit(1);
});
