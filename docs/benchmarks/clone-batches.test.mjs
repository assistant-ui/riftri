import assert from 'node:assert/strict';
import fs from 'node:fs';
import test from 'node:test';
import { distribution } from './latency-summary.mjs';

test('clone-batch summaries retain every measured sample', () => {
  const report = JSON.parse(fs.readFileSync(new URL('./clone-batches-2026-10-09.json', import.meta.url)));
  assert.deepEqual(Object.keys(report.cases), ['small', 'medium', 'large']);
  for (const entry of Object.values(report.cases)) {
    for (const variant of ['unbounded', 'batched']) {
      const samples = entry[`${variant}_microseconds`];
      assert.equal(samples.length, entry.rounds);
      const summary = distribution(samples);
      assert.equal(entry[variant].p50_us, summary.p50);
      assert.equal(entry[variant].p95_us, summary.p95);
    }
  }
});

test('experimental worker-reuse summaries retain every measured sample', () => {
  const report = JSON.parse(fs.readFileSync(new URL('./clone-worker-reuse-2026-10-09.json', import.meta.url)));
  assert.equal(report.status, 'experimental_only');
  assert.deepEqual(Object.keys(report.cases), ['small', 'medium', 'large', 'medium_repeat', 'medium_verification']);
  for (const [name, entry] of Object.entries(report.cases)) {
    assert.equal(entry.rounds, name === 'medium_verification' ? 6 : 12);
    assert.equal(entry.batch_size, 1024);
    for (const variant of ['unbounded', 'restarting', 'reused']) {
      const samples = entry[`${variant}_microseconds`];
      assert.equal(samples.length, entry.rounds);
      const summary = distribution(samples);
      assert.equal(entry[variant].p50_us, summary.p50);
      assert.equal(entry[variant].p95_us, summary.p95);
    }
    for (const baseline of ['unbounded', 'restarting']) {
      const ratios = entry.reused_microseconds.map((value, i) => value / entry[`${baseline}_microseconds`][i]);
      assert.deepEqual(entry[`paired_reused_over_${baseline}`], distribution(ratios));
    }
  }
});
