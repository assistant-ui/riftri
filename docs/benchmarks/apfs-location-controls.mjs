import assert from 'node:assert/strict';
import {spawn, spawnSync} from 'node:child_process';
import {createHash} from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import {pathToFileURL} from 'node:url';
import {prepareEvaluation} from './adaptive-evaluation-ci.mjs';
import {evaluateCalibration} from './apfs-calibration-gate.mjs';

const sourceCommit = '038cd9f82b418afe9e6d0080648738f78586fbca';
const sourceTree = 'c4de7922b24126e860cb77652f5d080e04c8c396';
const hash = bytes => createHash('sha256').update(bytes).digest('hex');

export function caseOrder(order = 'forward') {
  assert.ok(['forward', 'reverse'].includes(order));
  const cases = [
    ['host', 1, 1], ['image', 1, 1], ['image', 4, 1], ['host', 4, 1],
    ['image', 1, 2], ['host', 1, 2], ['host', 4, 2], ['image', 4, 2],
  ].map(([location, concurrency, repeat]) => ({location, concurrency, repeat,
    name: `${location}-${concurrency === 1 ? 'serial' : 'four'}-${repeat}`}));
  return order === 'forward' ? cases : cases.reverse();
}

export function evaluateLocationControl(data, binarySha256, concurrency) {
  const evaluation = evaluateCalibration(data, binarySha256, concurrency);
  const fixtureMatches = data?.sourceCommit === sourceCommit && data.sourceTree === sourceTree &&
    data.tree === sourceTree && data.files === 5864 && data.logicalBytes === 73235115;
  const reasons = [];
  if (!fixtureMatches) reasons.push('wrong reference tree or size');
  if (!evaluation.validCalibration) reasons.push(...evaluation.reasons);
  const validControl = fixtureMatches && evaluation.validCalibration;
  if (validControl) {
    if (Math.abs(evaluation.secondMedianMs / evaluation.firstMedianMs - 1) > 0.05) reasons.push('median drift exceeds 5%');
    if (Math.abs(evaluation.medianPairedRatio - 1) > 0.05) reasons.push('paired median drift exceeds 5%');
    if (Math.min(...evaluation.ratios) < 0.5 || evaluation.maximumPairedRatio > 2) reasons.push('paired tail outside [0.5, 2]');
    if (Math.abs(evaluation.secondMedianCpuSeconds / evaluation.firstMedianCpuSeconds - 1) > 0.05) reasons.push('CPU drift exceeds 5%');
  }
  return {validControl, stable: reasons.length === 0, reasons, evaluation};
}

const within = (parent, child) => {
  const relative = path.relative(parent, child);
  return relative === '' || (!relative.startsWith(`..${path.sep}`) && relative !== '..' && !path.isAbsolute(relative));
};

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {timeout: 120000, maxBuffer: 16 * 1024 ** 2, ...options});
  assert.ifError(result.error);
  assert.equal(result.status, 0, `${command}: ${result.stderr}`);
  return result.stdout;
}

function layout(hostRoot, imageRoot, imageFile) {
  assert.ok(!within(hostRoot, imageRoot) && !within(imageRoot, hostRoot), 'measurement roots overlap');
  assert.notEqual(fs.statSync(hostRoot).dev, fs.statSync(imageRoot).dev, 'distinct mounted volumes required');
  assert.equal(fs.statSync(hostRoot).dev, fs.statSync(imageFile).dev, 'host lane must use the image backing filesystem');
  assert.deepEqual(fs.readdirSync(hostRoot), [], 'host root must be a fresh empty directory');
  assert.deepEqual(fs.readdirSync(imageRoot), [], 'image root must be a fresh empty directory');
  const plist = run('/usr/bin/hdiutil', ['info', '-plist']);
  const info = JSON.parse(run('/usr/bin/plutil', ['-convert', 'json', '-o', '-', '-'], {input: plist}));
  const selected = info.images.filter(image => fs.realpathSync(image['image-path']) === imageFile);
  assert.equal(selected.length, 1, 'exact image must be attached once');
  const mounts = selected[0]['system-entities'].flatMap(entity => entity['mount-point'] ? [fs.realpathSync(entity['mount-point'])] : []);
  assert.equal(mounts.filter(mount => within(mount, imageRoot)).length, 1, 'image lane must reside in the exact owned image');
  const allMounts = info.images.flatMap(image => image['system-entities']).flatMap(entity => entity['mount-point'] ? [fs.realpathSync(entity['mount-point'])] : []);
  assert.ok(allMounts.every(mount => !within(mount, hostRoot)), 'host lane must not be inside an attached disk image');
  const describe = root => {
    const stats = fs.statfsSync(root);
    return {path: root, device: fs.statSync(root).dev, filesystemType: stats.type,
      blockSize: stats.bsize, availableBytes: stats.bavail * stats.bsize};
  };
  return {host: describe(hostRoot), image: describe(imageRoot), imageFile, imageMount: mounts.find(mount => within(mount, imageRoot))};
}

export async function runLocationControls(binaryArgument, hostArgument, imageArgument, sourceArgument, imageFileArgument, outputArgument, order = 'forward') {
  assert.equal(process.platform, 'darwin', 'requires macOS native COW and time -l');
  const cases = caseOrder(order);
  const [binary, hostRoot, imageRoot, source, imageFile] = [binaryArgument, hostArgument, imageArgument, sourceArgument, imageFileArgument].map(p => fs.realpathSync(p));
  const output = path.join(fs.realpathSync(path.dirname(path.resolve(outputArgument))), path.basename(outputArgument));
  assert.ok(!within(hostRoot, output) && !within(imageRoot, output), 'reports must be outside measured roots');
  assert.equal(run('/usr/bin/git', ['-C', source, 'rev-parse', 'HEAD']).toString().trim(), sourceCommit);
  assert.equal(run('/usr/bin/git', ['-C', source, 'rev-parse', 'HEAD^{tree}']).toString().trim(), sourceTree);
  const volumes = layout(hostRoot, imageRoot, imageFile);
  const binarySha256 = hash(fs.readFileSync(binary));
  fs.mkdirSync(output); // Never overwrite a prior run, even an incomplete one.
  const prepared = path.join(output, 'prepared');
  prepareEvaluation(prepared); // Exact existing full-verification harness, no modifications.
  const harness = path.join(prepared, 'docs/benchmarks/apfs-read-ahead.mjs');
  const report = {schemaVersion: 1, kind: 'same-binary-location-controls',
    binary, binarySha256, sourceCommit, sourceTree, volumes, order, cases,
    harnessSha256: hash(fs.readFileSync(harness)), startedAt: new Date().toISOString(),
    complete: false, allControlsStable: false, optimizationClaim: false, results: []};
  const receipt = path.join(output, 'controls.json');
  const save = () => fs.writeFileSync(receipt, JSON.stringify(report, null, 2) + '\n');
  save();
  for (const item of cases) {
    const destination = path.join(item.location === 'host' ? hostRoot : imageRoot, item.name);
    assert.ok(!fs.existsSync(destination), 'case output already exists');
    console.log(`Starting ${item.name}: identical binary on both labels.`);
    const log = fs.openSync(path.join(output, `${item.name}.log`), 'wx');
    const outcome = await new Promise(resolve => {
      const child = spawn(process.execPath, [harness, binary, destination, '4096', '8', binary, '8192', source, String(item.concurrency)],
        {env: {...process.env, RIFTRI_BENCH_STAGE_DIAGNOSTICS: '0'}, stdio: ['ignore', log, log]});
      child.on('error', error => resolve({code: null, error: String(error)}));
      child.on('close', (code, signal) => resolve({code, signal}));
    });
    fs.closeSync(log);
    let raw = null, data, readError;
    try { raw = fs.readFileSync(path.join(destination, 'results.json')); data = JSON.parse(raw); }
    catch (error) { readError = String(error); }
    const evaluation = evaluateLocationControl(data, binarySha256, item.concurrency);
    report.results.push({...item, destination, outcome, readError, rawSha256: raw ? hash(raw) : null, evaluation});
    save();
    console.log(JSON.stringify(report.results.at(-1)));
    if (outcome.code !== 0 || outcome.error || readError || !evaluation.validControl) {
      report.failure = `operational or evidence failure in ${item.name}; no later fixtures run and no force cleanup`;
      save();
      return report;
    }
  }
  report.complete = true;
  report.allControlsStable = report.results.every(result => result.evaluation.stable);
  report.completedAt = new Date().toISOString();
  save();
  return report;
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  const args = process.argv.slice(2);
  assert.ok(args.length === 6 || args.length === 7,
    'expected BINARY EMPTY_HOST_DIR EMPTY_IMAGE_DIR SOURCE_REPO ATTACHED_IMAGE NEW_REPORT_DIR [forward|reverse]');
  const result = await runLocationControls(...args);
  // Unstable controls are a diagnostic finding, not a correctness failure or
  // an optimization pass. Only a complete run exits successfully.
  if (!result.complete) process.exitCode = 1;
}
