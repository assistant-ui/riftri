// Nearest-rank percentiles; retain raw samples separately in results.json.
export function distribution(samples) {
  if (!samples.length) return null;
  if (!samples.every(value => Number.isFinite(value) && value >= 0)) throw new Error('Invalid latency sample');
  const sorted = [...samples].sort((a, b) => a - b);
  const percentile = fraction => sorted[Math.ceil(fraction * sorted.length) - 1];
  return { samples: sorted.length, min: sorted[0], p50: percentile(0.5), p95: percentile(0.95), max: sorted.at(-1) };
}
