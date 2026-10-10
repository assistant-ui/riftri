// Evidence-only acceptance gate for the predeclared #693 replication.
// Never infer successful correctness checks from a fast partial result.
export function evaluateReplicationFixture(data, concurrency) {
  const rounds = 8, labels = ['baseline', 'advice'], reasons = [];
  const samples = Array.isArray(data?.samples) ? data.samples : [];
  const batches = Array.isArray(data?.batches) ? data.batches : [];
  if (data?.complete !== true || data.failure) reasons.push('fixture did not complete');
  if (data?.stageDiagnostics !== false) reasons.push('not an uninstrumented run');
  if (!Number.isInteger(concurrency) || concurrency < 1 || concurrency > 10 || data?.rounds !== rounds || data?.concurrency !== concurrency) reasons.push('wrong comparison shape');
  const checksum = value => typeof value === 'string' && /^[a-f0-9]{64}$/.test(value);
  if (!checksum(data?.binarySha256) || !checksum(data?.candidateSha256) || data.binarySha256 === data.candidateSha256) reasons.push('distinct baseline/candidate binaries not identified');
  if (data?.final?.operations?.active_views !== 0 || !Array.isArray(data?.final?.bases) || data.final.bases.length !== 0 ||
      !Array.isArray(data?.final?.diagnostic_issues) || data.final.diagnostic_issues.length !== 0) reasons.push('final cleanup not verified');
  const sampleKeys = new Set();
  let candidateTimeouts = 0, baselineTimeouts = 0;
  for (const sample of samples) {
    if (sample?.timedOut && sample.label === 'advice') candidateTimeouts++;
    if (sample?.timedOut && sample.label === 'baseline') baselineTimeouts++;
    const anchor = sample?.round === 0 && sample.label === 'baseline' && sample.worker === 0;
    const measured = Number.isInteger(sample?.round) && sample.round >= 1 && sample.round <= rounds &&
      labels.includes(sample.label) && Number.isInteger(sample.worker) && sample.worker >= 0 && sample.worker < concurrency;
    if (!anchor && !measured) { reasons.push('invalid worker identity'); continue; }
    const key = `${sample.round}-${sample.label}-${sample.worker}`;
    if (sampleKeys.has(key)) reasons.push('duplicate worker');
    sampleKeys.add(key);
    if (sample.success !== true || sample.code !== 0 || sample.timedOut !== false || sample.error !== null) reasons.push('unsuccessful worker');
    if (sample.reused !== !anchor) reasons.push('wrong cold/cached worker state');
  }
  if (samples.length !== 1 + 2 * rounds * concurrency || sampleKeys.size !== 1 + 2 * rounds * concurrency) reasons.push('missing or extra workers');
  const batchMap = new Map();
  for (const batch of batches) {
    if (!Number.isInteger(batch?.round) || batch.round < 1 || batch.round > rounds || !labels.includes(batch.label) ||
        batch.concurrency !== concurrency || !Number.isFinite(batch.milliseconds) || batch.milliseconds <= 0) {
      reasons.push('invalid batch'); continue;
    }
    const key = `${batch.round}-${batch.label}`;
    if (batchMap.has(key)) reasons.push('duplicate batch');
    batchMap.set(key, batch.milliseconds);
  }
  if (batches.length !== 2 * rounds || batchMap.size !== 2 * rounds) reasons.push('missing or extra batches');
  if (candidateTimeouts) reasons.push('candidate deadline failure');
  const completeComparison = reasons.length === 0;
  let baselineMedianMilliseconds = null, candidateMedianMilliseconds = null, fasterPairs = null;
  if (completeComparison) {
    const median = values => { values.sort((a,b) => a-b); return (values[3] + values[4]) / 2; };
    baselineMedianMilliseconds = median(Array.from({ length:rounds }, (_, index) => batchMap.get(`${index + 1}-baseline`)));
    candidateMedianMilliseconds = median(Array.from({ length:rounds }, (_, index) => batchMap.get(`${index + 1}-advice`)));
    fasterPairs = Array.from({ length:rounds }, (_, index) => index + 1)
      .filter(round => batchMap.get(`${round}-advice`) < batchMap.get(`${round}-baseline`)).length;
    if (candidateMedianMilliseconds >= baselineMedianMilliseconds) reasons.push('candidate median did not improve');
    if (fasterPairs < 6) reasons.push('fewer than six faster pairs');
  }
  return { completeComparison, candidateTimeouts, baselineTimeouts, baselineMedianMilliseconds, candidateMedianMilliseconds,
    fasterPairs, gatePassed:reasons.length === 0, reasons:[...new Set(reasons)] };
}
