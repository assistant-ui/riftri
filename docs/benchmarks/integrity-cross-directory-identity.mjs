import assert from 'node:assert/strict';
export function assertCrossDirectoryIdentity(previous, candidate) {
  assert.equal(previous.commit,'e3ce4c20d73dd68fa6ac6533b34167022cd2a6a6');
  assert.notEqual(candidate.commit,previous.commit);
  assert.match(candidate.commit,/^[a-f0-9]{40}$/);
  for(const record of [previous,candidate]) {
    assert.equal(record.compiledFresh,true);
    assert.match(record.binarySha256,/^[a-f0-9]{64}$/);
  }
  assert.notEqual(candidate.binarySha256,previous.binarySha256);
  assert.equal(previous.hasCrossDirectoryRegression,false);
  assert.equal(candidate.hasCrossDirectoryRegression,true);
}
