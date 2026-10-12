import assert from 'node:assert/strict';
import test from 'node:test';
import {assertCrossDirectoryIdentity} from './integrity-cross-directory-identity.mjs';
const previous={commit:'e3ce4c20d73dd68fa6ac6533b34167022cd2a6a6',binarySha256:'a'.repeat(64),compiledFresh:true,hasCrossDirectoryRegression:false};
const candidate={commit:'b'.repeat(40),binarySha256:'c'.repeat(64),compiledFresh:true,hasCrossDirectoryRegression:true};
test('screen refuses a stale or identical candidate executable',()=>{
  assertCrossDirectoryIdentity(previous,candidate);
  assert.throws(()=>assertCrossDirectoryIdentity(previous,{...candidate,binarySha256:previous.binarySha256}));
  assert.throws(()=>assertCrossDirectoryIdentity(previous,{...candidate,hasCrossDirectoryRegression:false}));
  assert.throws(()=>assertCrossDirectoryIdentity(previous,{...candidate,compiledFresh:false}));
  assert.throws(()=>assertCrossDirectoryIdentity({...previous,commit:candidate.commit},candidate));
});
