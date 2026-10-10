import {timedProcess} from './timed-process.mjs';

// A benchmark-wide budget must expire before its CI step deadline. Both the
// setup/verification commands and measured workers use this same owner; no
// synchronous subprocess can block delivery of an outer cancellation.
export function createProcessScope({budgetMs, killGraceMs = 1000, onCancel = () => {}} = {}) {
  if (!Number.isFinite(budgetMs) || budgetMs <= 0 || budgetMs > 2147483647) throw new TypeError('positive Node timer budget required');
  if (!Number.isFinite(killGraceMs) || killGraceMs < 0 || killGraceMs > 2147483647) throw new TypeError('valid Node timer grace required');
  const controller = new AbortController(), pending = new Set();
  let closed = false;
  const cancel = reason => {
    if (controller.signal.aborted || closed) return;
    // Abort first: even a receipt-writing failure must not leave workers alive.
    controller.abort(new Error(reason));
    try { onCancel(reason); } catch (error) { console.error('Cannot retain cancellation receipt:', error); }
  };
  const handlers = new Map(['SIGINT', 'SIGTERM', 'SIGHUP'].map(name => [name, () => cancel(name)]));
  for (const [name, handler] of handlers) process.on(name, handler);
  const deadline = setTimeout(() => cancel('benchmark budget exhausted'), budgetMs);
  return {
    signal: controller.signal,
    cancel,
    async run(command, args, options = {}) {
      if (closed) throw new Error('benchmark process scope is closed');
      controller.signal.throwIfAborted();
      const result = timedProcess(command, args, {...options, signal: controller.signal, killGraceMs});
      pending.add(result);
      try { return await result; } finally { pending.delete(result); }
    },
    async close() {
      if (pending.size) cancel('benchmark owner closing with unfinished commands');
      closed = true;
      clearTimeout(deadline);
      await Promise.allSettled([...pending]);
      for (const [name, handler] of handlers) process.removeListener(name, handler);
    },
  };
}
