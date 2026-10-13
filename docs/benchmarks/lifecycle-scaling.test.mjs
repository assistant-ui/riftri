import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import test from 'node:test';
import {createProcessScope} from './process-scope.mjs';

const binary = process.env.RIFTRI_BENCH_TEST_BINARY;
for (const command of ['repair', 'gc']) {
  test(`lifecycle scaling verifies real ${command} reports and cleanup`, {skip: !binary, timeout: 180_000}, async () => {
    const parent = fs.mkdtempSync(path.join(os.tmpdir(), 'riftri-lifecycle-scaling-'));
    const output = path.join(parent, 'evidence');
    const scope = createProcessScope({budgetMs: 170_000});
    try {
      const receipt = await scope.run(process.execPath, [
        fileURLToPath(new URL('./lifecycle-scaling.mjs', import.meta.url)),
        binary, binary, output, command, '2', '4',
      ], {timeoutMs: 160_000});
      assert.equal(receipt.code, 0, JSON.stringify(receipt));
      const result = JSON.parse(fs.readFileSync(path.join(output, 'results.json')));
      assert.equal(result.complete, true);
      assert.equal(result.samples.length, 24);
      assert.equal(result.hashes.before, result.hashes.after);
      assert.equal(typeof result.controlsPassed, 'boolean');
      // Same-binary/noisy results must not make correctness tests flaky.
      for (const sample of result.samples) {
        const raw = JSON.parse(fs.readFileSync(path.join(output, sample.receipt)));
        assert.equal(raw.code, 0);
        assert.equal(raw.milliseconds, sample.milliseconds);
      }
    } finally {
      await scope.close();
      // Keep failed fixtures and receipts for diagnosis. Successful runs have
      // already removed every managed view through Riftri, not filesystem rm.
    }
  });
}
