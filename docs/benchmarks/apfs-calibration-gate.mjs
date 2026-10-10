// Same-binary diagnostic only. Never accepts an optimization for adoption.
export function evaluateCalibration(data, expectedBinarySha256 = '33240072e0f00873ecf588f0917dab1e198a5f9bd246bc3a3d195ad981f1c80a') {
  const reasons = [];
  const median = values => {
    const sorted = [...values].sort((a, b) => a - b);
    return sorted.length ? (sorted[(sorted.length - 1) >> 1] + sorted[sorted.length >> 1]) / 2 : null;
  };
  if (data?.complete !== true || data.failure) reasons.push('fixture did not complete');
  if (data?.stageDiagnostics !== false || data?.rounds !== 8 || data?.concurrency !== 1) reasons.push('wrong comparison shape');
  const baseline = expectedBinarySha256;
  if (typeof baseline !== 'string' || !/^[a-f0-9]{64}$/.test(baseline)) reasons.push('invalid expected binary identity');
  if (data?.binarySha256 !== baseline || data?.candidateSha256 !== baseline) reasons.push('not the pinned identical binary');
  if (data?.final?.operations?.active_views !== 0 || !Array.isArray(data?.final?.bases) || data.final.bases.length ||
      !Array.isArray(data?.final?.diagnostic_issues) || data.final.diagnostic_issues.length) reasons.push('cleanup not verified');
  const samples = Array.isArray(data?.samples) ? data.samples : [];
  const keys = new Set();
  const cpu = {baseline: [], advice: []};
  for (const s of samples) {
    const anchor = s?.round === 0 && s.label === 'baseline' && s.worker === 0;
    const measured = Number.isInteger(s?.round) && s.round >= 1 && s.round <= 8 &&
      ['baseline', 'advice'].includes(s.label) && s.worker === 0;
    if (!anchor && !measured) reasons.push('invalid worker identity');
    const key = `${s?.round}-${s?.label}-${s?.worker}`;
    if (keys.has(key)) reasons.push('duplicate worker');
    keys.add(key);
    if (s?.success !== true || s.code !== 0 || s.timedOut !== false || s.error !== null || s.reused !== !anchor) reasons.push('unsuccessful worker');
    if (measured) {
      const match = s.resources?.join('\n').match(/([\d.]+) real\s+([\d.]+) user\s+([\d.]+) sys/);
      const seconds = match ? Number(match[2]) + Number(match[3]) : NaN;
      if (!Number.isFinite(seconds) || seconds <= 0) reasons.push('invalid CPU evidence');
      else cpu[s.label].push(seconds);
    }
  }
  if (samples.length !== 17 || keys.size !== 17) reasons.push('missing or extra workers');
  const batches = Array.isArray(data?.batches) ? data.batches : [];
  const batchMap = new Map();
  for (const b of batches) {
    if (!Number.isInteger(b?.round) || b.round < 1 || b.round > 8 || !['baseline', 'advice'].includes(b.label) ||
        b.concurrency !== 1 || !Number.isFinite(b.milliseconds) || b.milliseconds <= 0) reasons.push('invalid batch');
    const key = `${b?.round}-${b?.label}`;
    if (batchMap.has(key)) reasons.push('duplicate batch');
    batchMap.set(key, b?.milliseconds);
  }
  if (batches.length !== 16 || batchMap.size !== 16) reasons.push('missing or extra batches');
  if (reasons.length) return {validCalibration: false, reasons: [...new Set(reasons)]};
  const times = label => Array.from({length: 8}, (_, i) => batchMap.get(`${i + 1}-${label}`));
  const first = times('baseline'), second = times('advice');
  const ratios = second.map((value, i) => value / first[i]);
  const firstMedianMs = median(first), secondMedianMs = median(second);
  const medianPairedRatio = median(ratios), maximumPairedRatio = Math.max(...ratios);
  const firstMedianCpuSeconds = median(cpu.baseline), secondMedianCpuSeconds = median(cpu.advice);
  return {validCalibration: true, reasons: [], firstMedianMs, secondMedianMs,
    apparentReductionPercent: 100 * (1 - secondMedianMs / firstMedianMs),
    fasterPairs: ratios.filter(ratio => ratio < 1).length, ratios, medianPairedRatio, maximumPairedRatio,
    firstMedianCpuSeconds, secondMedianCpuSeconds,
    wouldCrossSerialLimits: secondMedianMs > 1.02 * firstMedianMs || medianPairedRatio > 1.02 ||
      maximumPairedRatio > 2 || secondMedianCpuSeconds > 1.05 * firstMedianCpuSeconds};
}
