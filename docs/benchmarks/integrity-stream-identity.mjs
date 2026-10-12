import assert from 'node:assert/strict';

export function assertStreamComparisonIdentity(previous, candidate) {
  assert.equal(previous.commit, 'e3ce4c20d73dd68fa6ac6533b34167022cd2a6a6');
  assert.match(candidate.commit, /^[a-f0-9]{40}$/);
  assert.notEqual(candidate.commit, previous.commit);
  for (const record of [previous, candidate]) {
    assert.match(record.binarySha256, /^[a-f0-9]{64}$/);
    assert.equal(record.compiledFresh, true, 'isolated build must compile the test executable');
  }
  assert.notEqual(previous.binarySha256, candidate.binarySha256, 'comparison requires different binaries');
  assert.equal(previous.rustc, candidate.rustc, 'comparison requires the same compiler');
  assert.equal(previous.hasStreamingRegression, false, 'baseline must not contain the streaming regression');
  assert.equal(candidate.hasStreamingRegression, true, 'candidate must contain the streaming regression');
}
