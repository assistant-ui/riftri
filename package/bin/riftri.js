#!/usr/bin/env node

"use strict";

const { spawn } = require("node:child_process");
const { resolveBinary } = require("../lib/platform.js");
const { signalExitCode } = require("../lib/signals.js");

function fail(message) {
  process.stderr.write(`riftri: ${message}\n`);
  process.exit(1);
}

let binary;
try {
  binary = resolveBinary();
} catch (error) {
  fail(error instanceof Error ? error.message : String(error));
}

const child = spawn(binary, process.argv.slice(2), {
  stdio: "inherit",
  windowsHide: false,
});

// Mirror the termination contract of native `riftri exec` (docs/cli.md) while
// the native process runs. The launcher survives SIGINT (Ctrl-C) and SIGQUIT
// (Ctrl-\): a terminal delivers both to the whole foreground process group,
// so the native process receives its own copy and — like a shell waiting on a
// foreground job — the command alone decides whether the interrupt is fatal.
// Dying here instead would return the shell prompt while riftri and a command
// that caught the interrupt keep running on the terminal. PID-directed
// SIGTERM and SIGHUP are forwarded to the native process, which applies the
// same contract downstream to the scoped command. Windows has no equivalent
// to preserve: the console already delivers Ctrl-C events to every attached
// process, and a hard TerminateProcess cannot be intercepted.
const ignoredSignals = ["SIGINT", "SIGQUIT"];
const forwardedSignals = ["SIGTERM", "SIGHUP"];
const ignoreSignal = () => {};
const forwardSignal = (signal) => {
  child.kill(signal);
};

if (process.platform !== "win32") {
  for (const signal of ignoredSignals) {
    process.on(signal, ignoreSignal);
  }
  for (const signal of forwardedSignals) {
    process.on(signal, forwardSignal);
  }
}

// The handlers deliberately stay installed until the process exits. Removing
// the last listener for a signal restores its default disposition, so a SIGINT
// still in flight — the common case, since the terminal sent it to the whole
// foreground group — would kill the launcher in the window before
// `process.exit`, losing the command's real status. `process.exit` does not
// need them removed.

child.on("error", (error) => {
  fail(`could not start the native executable at ${binary}: ${error.message}`);
});

child.on("exit", (code, signal) => {
  if (signal !== null) {
    const exitCode = signalExitCode(signal);
    if (exitCode === null) {
      fail(`native executable stopped after unknown signal ${signal}`);
    }
    process.exit(exitCode);
  }
  process.exit(code ?? 1);
});
