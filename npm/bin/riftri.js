#!/usr/bin/env node

"use strict";

const { spawnSync } = require("node:child_process");
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

const result = spawnSync(binary, process.argv.slice(2), {
  stdio: "inherit",
  windowsHide: false,
});

if (result.error) {
  fail(`could not start the native executable at ${binary}: ${result.error.message}`);
}

if (result.signal) {
  try {
    process.kill(process.pid, result.signal);
  } catch {
    fail(`native executable stopped after signal ${result.signal}`);
  }
} else {
  process.exit(result.status ?? 1);
}
