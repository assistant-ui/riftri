import assert from 'node:assert/strict';
import {test} from 'node:test';
import {assertStreamComparisonIdentity} from './integrity-stream-identity.mjs';

const fixture = () => [
  {commit: 'e3ce4c20d73dd68fa6ac6533b34167022cd2a6a6', binarySha256: 'a'.repeat(64), rustc: 'same compiler', compiledFresh: true, hasStreamingRegression: false},
  {commit: 'b'.repeat(40), binarySha256: 'b'.repeat(64), rustc: 'same compiler', compiledFresh: true, hasStreamingRegression: true},
];

test('distinct freshly compiled baseline and streaming candidate are required', () => {
  assert.doesNotThrow(() => assertStreamComparisonIdentity(...fixture()));
});

test('the observed shared-target stale executable cannot become candidate evidence', () => {
  const [previous, candidate] = fixture();
  candidate.binarySha256 = previous.binarySha256;
  assert.throws(() => assertStreamComparisonIdentity(previous, candidate), /different binaries/);
});

test('a cached artifact, wrong source test, commit or compiler fails before timing', () => {
  for (const mutate of [
    ([previous]) => { previous.compiledFresh = false; },
    ([, candidate]) => { candidate.compiledFresh = false; },
    ([previous]) => { previous.hasStreamingRegression = true; },
    ([, candidate]) => { candidate.hasStreamingRegression = false; },
    ([, candidate]) => { candidate.rustc = 'different compiler'; },
    ([previous]) => { previous.commit = 'c'.repeat(40); },
    ([previous, candidate]) => { candidate.commit = previous.commit; },
  ]) {
    const pair = fixture();
    mutate(pair);
    assert.throws(() => assertStreamComparisonIdentity(...pair));
  }
});
