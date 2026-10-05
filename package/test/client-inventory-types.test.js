const assert = require("node:assert/strict");
const { spawnSync } = require("node:child_process");
const { test } = require("node:test");
const { fixture, assertTypes } = require("./helpers/client-contract.js");

test("inventory overloads match single-state and all-state CLI reports", async (t) => {
  const { directory, repository, stateDir, client } = fixture(t);
  const config = spawnSync("git", ["-C", repository, "config", "--add", "riftri.stateDirectory", stateDir], { encoding: "utf8" });
  assert.equal(config.status, 0, config.stderr);
  const single = await client.worktree.list();
  const all = await client.worktree.list({ allStates: true });
  assert.equal(single.schema_version, 1);
  assert.equal(all.schema_version, 2);
  assert.equal("state_directory" in all, false);
  assert.ok(all.state_directories.some((state) => state.source === "registered"));
  assertTypes(directory, `
    type Single = Awaited<ReturnType<typeof singleList>>;
    type All = Awaited<ReturnType<typeof allList>>;
    const single: Single = ${JSON.stringify(single)};
    const all: All = ${JSON.stringify(all)};
    function singleList(client: Riftri) { return client.worktree.list(); }
    function allList(client: Riftri) { return client.worktree.list({ allStates: true }); }
    const path: string = single.state_directory;
    const scope: "all-registered-states" = all.scope;
    const paths: string[] = all.state_directories.map(state => state.path);
    // @ts-expect-error All-state reports have no single state_directory.
    all.state_directory;
    async function variants(client: Riftri, flag: boolean) {
      const explicitSingle = await client.worktree.list({ allStates: false });
      const singlePath: string = explicitSingle.state_directory;
      const emptyOptions = await client.worktree.list({});
      const emptyPath: string = emptyOptions.state_directory;
      const result = await client.worktree.list({ allStates: flag });
      // @ts-expect-error A dynamic flag requires narrowing the result.
      result.state_directory;
      if (result.schema_version === 1) {
        const path: string = result.state_directory;
      } else {
        const paths: string[] = result.state_directories.map(state => state.path);
        const viewStates: string[] = result.worktrees.map(view => view.state_directory);
        const diagnosticStates: (string | null)[] = result.diagnostic_issues.map(issue => issue.state_directory);
      }
    }
  `);
});
