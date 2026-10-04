import assert from 'node:assert/strict';
import test from 'node:test';
import { settleWorkers } from './settle-workers.mjs';

test('a failed batch waits for every worker and records ordered outcomes without retrying', async () => {
  let release;
  const gate = new Promise(resolve => { release = resolve; });
  const calls = [];
  let receipt;
  let done = false;
  const first = new Error('Git baseline failed');
  const last = new Error('second failure');
  const batch = settleWorkers(['first', 'slow', 'last'], async label => {
    calls.push(label);
    if (label === 'first') throw first;
    if (label === 'last') throw last;
    await gate;
    done = true;
    return { label, seconds: 2 };
  }, outcomes => {
    assert.equal(done, true);
    receipt = outcomes;
  });
  const rejected = assert.rejects(batch, error => {
    assert.ok(error instanceof AggregateError);
    assert.deepEqual(error.errors, [first, last]);
    assert.match(error.message, /first.*last/);
    return true;
  });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(receipt, undefined);
  release();
  await rejected;
  assert.deepEqual(calls, ['first', 'slow', 'last']);
  assert.deepEqual(receipt.map(entry => [entry.label, entry.status]), [
    ['first', 'rejected'], ['slow', 'fulfilled'], ['last', 'rejected'],
  ]);
  assert.deepEqual(receipt[1].value, { label: 'slow', seconds: 2 });
  assert.equal(receipt[0].reason, first);
});

test('successful batches retain input order and do not write a failure receipt', async () => {
  const records = await settleWorkers(['a', 'b'], async label => label, () => assert.fail('unexpected failure'));
  assert.deepEqual(records, ['a', 'b']);
  assert.deepEqual(await settleWorkers([], () => assert.fail(), () => assert.fail()), []);
});

test('synchronous launch errors still allow remaining workers to settle', async () => {
  const calls = [];
  let receipt;
  await assert.rejects(settleWorkers(['missing', 'healthy'], label => {
    calls.push(label);
    if (label === 'missing') throw new Error('spawn failed');
    return label;
  }, outcomes => { receipt = outcomes; }), AggregateError);
  assert.deepEqual(calls, ['missing', 'healthy']);
  assert.equal(receipt[1].value, 'healthy');
});
