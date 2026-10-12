import assert from 'node:assert/strict';
import test from 'node:test';
import {evaluatePairs} from './prefix-startup.mjs';

test('startup gate needs consistent end-to-end improvement, not a phase win', () => {
  assert.equal(evaluatePairs(Array.from({length:8},()=>({a:100,b:95})), 'cached', false).passed, true);
  assert.equal(evaluatePairs(Array.from({length:8},()=>({a:100,b:99})), 'cached', false).passed, false);
  assert.equal(evaluatePairs([{a:100,b:95},{a:100,b:96},{a:100,b:97}], 'cold', false).passed, true);
});
test('same-binary controls and tail regressions invalidate a result', () => {
  assert.equal(evaluatePairs(Array.from({length:8},()=>({a:100,b:94})), 'cached', true).passed, false);
  const pairs=Array.from({length:8},()=>({a:100,b:95}));
  pairs[0].b=126;
  assert.equal(evaluatePairs(pairs, 'cached', false).passed, false);
  assert.throws(()=>evaluatePairs(pairs.slice(1),'cached',false));
  assert.throws(()=>evaluatePairs([{a:0,b:1},{a:1,b:1},{a:1,b:1}], 'cold',false));
});
