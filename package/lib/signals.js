"use strict";

const os = require("node:os");

/**
 * Exit code the launcher reports for a native process that died from `signal`,
 * following the shell convention native `riftri exec` itself uses: 128 plus
 * the signal's number on this platform. The code is computed numerically —
 * never by re-raising the signal through `process.kill`, which is a silent
 * no-op for signals Node ignores or reserves (SIGPIPE, SIGUSR1, SIGWINCH,
 * SIGCHLD, SIGURG) and would turn a killed run into exit 0.
 *
 * Returns `null` when the name is not a signal this platform knows, so the
 * caller can report the failure instead of guessing a number.
 */
function signalExitCode(signal) {
  if (typeof signal !== "string") {
    return null;
  }
  const number = os.constants.signals[signal];
  if (!Number.isInteger(number) || number <= 0) {
    return null;
  }
  return 128 + number;
}

module.exports = { signalExitCode };
