"use strict";

const assert = require("node:assert/strict");
const os = require("node:os");
const path = require("node:path");
const { spawn } = require("node:child_process");
const { once } = require("node:events");
const { mkdtemp, readFile, rm, writeFile } = require("node:fs/promises");
const { setTimeout: delay } = require("node:timers/promises");
const { test } = require("node:test");

const { signalExitCode } = require("../lib/signals.js");

const repositoryRoot = path.resolve(__dirname, "..", "..");
const launcher = path.join(repositoryRoot, "package", "bin", "riftri.js");
const nativeBinary = path.join(
  repositoryRoot,
  "target",
  "debug",
  process.platform === "win32" ? "riftri.exe" : "riftri",
);
const onWindows = process.platform === "win32";

async function makeScratchDirectory(t) {
  const directory = await mkdtemp(path.join(os.tmpdir(), "riftri-launcher-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  return directory;
}

async function writeFakeBinary(directory, script) {
  const binary = path.join(directory, "riftri");
  await writeFile(binary, `#!/bin/sh\n${script}\n`, { mode: 0o755 });
  return binary;
}

function waitForOutput(stream, text) {
  return new Promise((resolve, reject) => {
    let buffer = "";
    stream.setEncoding("utf8");
    const onData = (chunk) => {
      buffer += chunk;
      if (buffer.includes(text)) {
        stream.off("data", onData);
        resolve(buffer);
      }
    };
    stream.on("data", onData);
    stream.once("end", () => {
      reject(new Error(`stream ended before ${JSON.stringify(text)}: ${buffer}`));
    });
  });
}

test("maps every platform signal name to 128 plus its number", () => {
  for (const [name, number] of Object.entries(os.constants.signals)) {
    assert.equal(signalExitCode(name), 128 + number);
  }
  assert.equal(signalExitCode("SIGNOTASIGNAL"), null);
  assert.equal(signalExitCode("toString"), null);
  assert.equal(signalExitCode(null), null);
});

// The launcher must report a signal death numerically as 128 + the signal's
// number. Re-raising through process.kill only worked for signals Node lets a
// process send itself; for ignored or reserved ones (SIGUSR1, SIGPIPE, ...)
// it was a silent no-op and the launcher fell through to exit 0.
for (const signal of ["SIGTERM", "SIGINT", "SIGHUP", "SIGUSR1", "SIGPIPE"]) {
  test(
    `launcher reports 128+n when the native executable dies from ${signal}`,
    { skip: onWindows, timeout: 15_000 },
    async (t) => {
      const directory = await makeScratchDirectory(t);
      const binary = await writeFakeBinary(directory, `kill -${signal.slice(3)} $$`);
      const child = spawn(process.execPath, [launcher, "--version"], {
        env: { ...process.env, RIFTRI_BINARY: binary },
        stdio: ["ignore", "ignore", "pipe"],
      });
      const [code, exitSignal] = await once(child, "exit");
      assert.equal(exitSignal, null);
      assert.equal(code, 128 + os.constants.signals[signal]);
    },
  );
}

// Ctrl-C semantics: the terminal delivers SIGINT to the whole foreground
// process group. The launcher must survive it while the child runs — the
// child alone decides whether the interrupt is fatal — and the child's later
// exit code must still propagate through the launcher.
test(
  "launcher stays attached through foreground-group SIGINT that the child survives",
  { skip: onWindows, timeout: 15_000 },
  async (t) => {
    const directory = await makeScratchDirectory(t);
    const release = path.join(directory, "release");
    const binary = await writeFakeBinary(
      directory,
      [
        "trap : INT",
        "echo ready",
        `until [ -e "${release}" ]; do sleep 0.05; done`,
        "exit 37",
      ].join("\n"),
    );
    const child = spawn(process.execPath, [launcher], {
      detached: true,
      env: { ...process.env, RIFTRI_BINARY: binary },
      stdio: ["ignore", "pipe", "pipe"],
    });
    t.after(() => {
      try {
        process.kill(-child.pid, "SIGKILL");
      } catch {
        // The group is already gone on the happy path.
      }
    });
    const exited = once(child, "exit");

    await waitForOutput(child.stdout, "ready");
    process.kill(-child.pid, "SIGINT");
    await delay(300);
    assert.equal(child.exitCode, null, "launcher died from a group SIGINT the child survived");

    await writeFile(release, "");
    const [code, exitSignal] = await exited;
    assert.equal(exitSignal, null);
    assert.equal(code, 37);
  },
);

// PID-directed SIGTERM and SIGHUP must reach the child, and a child that
// catches the signal and exits normally must have that exit code propagated.
for (const signal of ["SIGTERM", "SIGHUP"]) {
  test(
    `launcher forwards PID-directed ${signal} to the child and propagates its exit code`,
    { skip: onWindows, timeout: 15_000 },
    async (t) => {
      const directory = await makeScratchDirectory(t);
      const binary = await writeFakeBinary(
        directory,
        [
          `trap 'exit 41' ${signal.slice(3)}`,
          "echo ready",
          "while :; do sleep 0.05; done",
        ].join("\n"),
      );
      const child = spawn(process.execPath, [launcher], {
        env: { ...process.env, RIFTRI_BINARY: binary },
        stdio: ["ignore", "pipe", "pipe"],
      });
      t.after(() => {
        try {
          child.kill("SIGKILL");
        } catch {
          // Already exited on the happy path.
        }
      });
      const exited = once(child, "exit");

      await waitForOutput(child.stdout, "ready");
      child.kill(signal);
      const [code, exitSignal] = await exited;
      assert.equal(exitSignal, null);
      assert.equal(code, 41);
    },
  );
}

// End-to-end against the built native binary: `riftri exec` reports a
// signal-terminated scoped command as a normal exit with 128 + the signal's
// number, and the launcher must propagate that code unchanged.
async function spawnNativeExec(t) {
  const directory = await makeScratchDirectory(t);
  const child = spawn(
    process.execPath,
    [
      launcher,
      "exec",
      "--",
      process.execPath,
      "-e",
      "console.log(`${process.ppid} ${process.pid}`); setTimeout(() => {}, 8000);",
    ],
    {
      cwd: directory,
      env: {
        ...process.env,
        HOME: directory,
        XDG_STATE_HOME: directory,
        RIFTRI_BINARY: nativeBinary,
      },
      stdio: ["ignore", "pipe", "pipe"],
    },
  );
  t.after(() => {
    try {
      child.kill("SIGKILL");
    } catch {
      // Already exited on the happy path.
    }
  });
  const ready = await waitForOutput(child.stdout, "\n");
  const [nativePid, scopedPid] = ready.trim().split(/\s+/u).map(Number);
  assert.ok(Number.isInteger(nativePid) && nativePid > 0, ready);
  assert.ok(Number.isInteger(scopedPid) && scopedPid > 0, ready);
  t.after(() => {
    try {
      process.kill(scopedPid, "SIGKILL");
    } catch {
      // The scoped command usually dies with native riftri.
    }
  });
  return { child, nativePid, scopedPid };
}

for (const signal of ["SIGTERM", "SIGHUP"]) {
  test(
    `launcher forwards PID-directed ${signal} through native riftri exec`,
    { skip: onWindows, timeout: 15_000 },
    async (t) => {
      const { child, nativePid } = await spawnNativeExec(t);
      const exited = once(child, "exit");

      child.kill(signal);
      const [code, exitSignal] = await exited;
      assert.equal(exitSignal, null);
      assert.equal(code, 128 + os.constants.signals[signal]);
      assert.throws(() => process.kill(nativePid, 0), { code: "ESRCH" });
    },
  );
}

// The previously-exit-0 defect end to end: a signal Node ignores or reserves
// kills the native process, and the launcher must still report 128 + its
// number rather than success. SIGUSR1 is the representative case here because
// the Rust runtime ignores SIGPIPE at startup, so the native binary cannot
// die from it; the fake-binary tests above pin the SIGPIPE mapping.
test(
  "launcher reports native riftri dying from SIGUSR1 instead of exit 0",
  { skip: onWindows, timeout: 15_000 },
  async (t) => {
    const { child, nativePid } = await spawnNativeExec(t);
    const exited = once(child, "exit");

    process.kill(nativePid, "SIGUSR1");
    const [code, exitSignal] = await exited;
    assert.equal(exitSignal, null);
    assert.equal(code, 128 + os.constants.signals.SIGUSR1);
  },
);

// Removing the last listener for a signal restores its default disposition.
// The launcher used to do that in its exit handler, before `process.exit`,
// which left a window where a SIGINT still in flight — the usual case, since
// a terminal sends it to the whole foreground group — killed the launcher and
// the command's real exit code was never reported.
//
// Measured against the previous launcher by releasing a command that exits 37
// and bursting SIGINT across that moment: 10 of 10 runs returned 1 instead of
// 37. With the handlers left installed, 10 of 10 returned 37. That race is not
// reproducible inside this runner on every platform, so the contract is
// pinned structurally instead of by timing.
test("the launcher keeps its signal handlers until it exits", async () => {
  const source = await readFile(launcher, "utf8");
  assert.doesNotMatch(
    source,
    /removeListener\s*\(/,
    "removing a signal listener restores its default disposition and reopens the window",
  );
  assert.match(source, /process\.on\(signal, ignoreSignal\)/);
  assert.match(source, /process\.on\(signal, forwardSignal\)/);
  // The handlers must outlive the exit handler that reports the child.
  const exitHandler = source.slice(source.indexOf('child.on("exit"'));
  assert.doesNotMatch(exitHandler, /release|removeListener/);
});
