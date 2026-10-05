const assert = require("node:assert/strict");
const { EventEmitter } = require("node:events");
const fs = require("node:fs");
const { createRequire } = require("node:module");
const path = require("node:path");
const { PassThrough } = require("node:stream");
const { test } = require("node:test");
const vm = require("node:vm");

// Real stream decoders with deterministic byte-sized chunks. OS pipes may
// coalesce writes, so a timer-based child fixture cannot guarantee the split.
function clientWithOutput(stdout, stderr, exitCode) {
  const filename = path.resolve(__dirname, "../lib/client.js");
  const localRequire = createRequire(filename);
  const module = { exports: {} };
  vm.runInNewContext(fs.readFileSync(filename, "utf8"), {
    module,
    process,
    require(name) {
      if (name !== "node:child_process") return localRequire(name);
      return {
        spawn() {
          const child = new EventEmitter();
          child.stdout = new PassThrough();
          child.stderr = new PassThrough();
          setImmediate(() => {
            for (const [stream, bytes] of [
              [child.stdout, Buffer.from(stdout)],
              [child.stderr, Buffer.from(stderr)],
            ]) {
              for (const byte of bytes) stream.write(Buffer.from([byte]));
              stream.end();
            }
            setImmediate(() => child.emit("close", exitCode, null));
          });
          return child;
        },
      };
    },
  }, { filename });
  return new module.exports.Riftri({ binary: "fixture" });
}

test("client preserves split UTF-8 in JSON reports", async () => {
  const expected = { path: "café/日本/🚀", plain: "ASCII" };
  const report = await clientWithOutput(JSON.stringify(expected), "", 0).status();
  assert.equal(JSON.stringify(report), JSON.stringify(expected));
});

test("client preserves split UTF-8 in receipts after hook output", async () => {
  const receipt = {
    code: "filesystem-io-failed",
    message: "Cannot read café/日本/🚀",
    nextCommand: "riftri status café/日本/🚀",
  };
  const stderr = `hook: café/日本/🚀\n${JSON.stringify(receipt)}\n`;
  await assert.rejects(clientWithOutput("", stderr, 1).status(), (error) => {
    assert.equal(error.message, receipt.message);
    assert.equal(JSON.stringify(error.receipt), JSON.stringify(receipt));
    assert.equal(error.exitCode, 1);
    return true;
  });
});

test("client preserves plain-text UTF-8 errors and flushes incomplete bytes", async () => {
  const bytes = Buffer.concat([Buffer.from("café/日本/🚀 "), Buffer.from([0xe2, 0x82])]);
  await assert.rejects(clientWithOutput("", bytes, 1).status(), (error) => {
    assert.equal(error.message, bytes.toString("utf8"));
    assert.equal(error.receipt, null);
    return true;
  });
});
