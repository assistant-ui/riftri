import assert from 'node:assert/strict';
import test from 'node:test';
import { distribution } from './latency-summary.mjs';

test('nearest-rank summaries preserve outliers and do not mutate samples', () => {
  const samples = [100, 1, 4, 2, 3];
  assert.deepEqual(distribution(samples), { samples: 5, min: 1, p50: 3, p95: 100, max: 100 });
  assert.deepEqual(samples, [100, 1, 4, 2, 3]);
  assert.equal(distribution([]), null);
  assert.deepEqual(distribution([2]), { samples: 1, min: 2, p50: 2, p95: 2, max: 2 });
  assert.throws(() => distribution([NaN]));
  assert.throws(() => distribution([-1]));
});
