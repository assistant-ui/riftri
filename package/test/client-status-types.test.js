const assert = require("node:assert/strict");
const { test } = require("node:test");
const { fixture, assertTypes } = require("./helpers/client-contract.js");

test("status types match real operation counters, not an array", async (t) => {
  const { directory, client } = fixture(t);
  const report = await client.status();
  assert.equal(Array.isArray(report.operations), false);
  for (const value of Object.values(report.operations)) assert.equal(typeof value, "number");
  assertTypes(directory, `
    const report: StatusReport = ${JSON.stringify(report)};
    const active: number = report.operations.active_views;
    const pending: number = report.operations.pending_adds;
    const complete: boolean = report.counts_complete;
    // @ts-expect-error Operation counters are not an array.
    report.operations.map(() => 0);
    // @ts-expect-error Unknown counters must not silently type-check.
    report.operations.nonexistent_counter;
    async function useClient(client: Riftri) {
      const count: number = (await client.status()).operations.pending_removals;
    }
  `);
});
