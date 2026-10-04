// Keep failures visible, but let every launched worker finish before reporting
// the batch. A rejected worker is never retried or counted as a timing sample.
export async function settleWorkers(labels, create, onFailure) {
  const settled = await Promise.allSettled(labels.map(label => Promise.resolve().then(() => create(label))));
  const outcomes = settled.map((outcome, index) => ({ label: labels[index], ...outcome }));
  const failures = outcomes.filter(outcome => outcome.status === 'rejected');
  if (failures.length) {
    await onFailure(outcomes);
    throw new AggregateError(failures.map(outcome => outcome.reason),
      `Benchmark workers failed: ${failures.map(outcome => outcome.label).join(', ')}`);
  }
  return outcomes.map(outcome => outcome.value);
}
