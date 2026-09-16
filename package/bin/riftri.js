#!/usr/bin/env node

"use strict";

const { spawn } = require("node:child_process");
const { resolveBinary } = require("../lib/platform.js");

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

const signals = ["SIGTERM", "SIGINT", "SIGHUP"];
const forwardSignal = (signal) => child.kill(signal);
for (const signal of signals) {
  process.on(signal, forwardSignal);
}

child.on("error", (error) => {
  fail(`could not start the native executable at ${binary}: ${error.message}`);
});

child.on("exit", (code, signal) => {
  for (const forwardedSignal of signals) {
    process.removeListener(forwardedSignal, forwardSignal);
  }
  if (signal) {
    try {
      process.kill(process.pid, signal);
    } catch {
      fail(`native executable stopped after signal ${signal}`);
    }
  } else {
    process.exit(code ?? 1);
  }
});
