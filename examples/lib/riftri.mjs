// A minimal Riftri client for a harness runner. No dependencies.
//
// The whole integration surface is here: spawn the CLI, read one JSON report
// from stdout, read one JSON receipt from stderr on failure, and branch on the
// exit code. Copy this file next to your runner and adapt it.

import { spawn } from "node:child_process";

/** Exit codes documented in docs/cli.md. */
export const EXIT = {
  SUCCESS: 0,
  OPERATIONAL: 1, // Git, storage, journal, or filesystem I/O. Retry may help.
  USAGE: 2, // Bad flag or argument. Never retry.
  POLICY: 3, // Riftri refused. Nothing changed. Fall back or stop.
};

/** Thrown for any non-zero exit, carrying the parsed receipt when present. */
export class RiftriError extends Error {
  constructor(code, receipt, stderr) {
    super(receipt?.message ?? stderr.trim() ?? `riftri exited ${code}`);
    this.name = "RiftriError";
    this.code = code; // process exit code
    this.receipt = receipt; // parsed --json-errors receipt, or null
  }

  /** Riftri declined before touching anything: safe to fall back. */
  get isPolicyRefusal() {
    return this.code === EXIT.POLICY;
  }

  /** A live process holds the lock. Unlike a refusal, waiting helps. */
  get isBusy() {
    return this.receipt?.code === "worktree-busy";
  }

  /** Run `nextCommand` (usually `riftri repair`) before continuing. */
  get needsRepair() {
    return this.receipt?.recovery === "required";
  }
}

/**
 * Run one Riftri command.
 *
 * `--json-errors` is accepted by every command and puts exactly one failure
 * receipt on stderr. `--json` is only accepted by commands that report or
 * change state — doctor, backends, status, repair, gc, and every worktree
 * subcommand — so pass `json: false` for the rest (enable, disable, exec).
 */
export function riftri(args, { cwd, bin = "riftri", json = true } = {}) {
  return new Promise((resolve, reject) => {
    const flags = json ? ["--json", "--json-errors"] : ["--json-errors"];
    const child = spawn(bin, [...args, ...flags], {
      cwd,
      stdio: ["ignore", "pipe", "pipe"],
    });

    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => (stdout += chunk));
    child.stderr.on("data", (chunk) => (stderr += chunk));

    child.on("error", reject); // riftri is not installed or not on PATH
    child.on("close", (code) => {
      if (code === EXIT.SUCCESS) {
        // Without --json the command prints human text, so do not parse it.
        resolve(json && stdout.trim() ? JSON.parse(stdout) : null);
        return;
      }
      let receipt = null;
      try {
        receipt = JSON.parse(stderr);
      } catch {
        // A usage error (exit 2) is reported by the argument parser and never
        // produces a receipt, so leaving this null is expected.
      }
      reject(new RiftriError(code, receipt, stderr));
    });
  });
}

/** True when this destination can actually get an optimized worktree. */
export async function isOptimizable(repository, destination, options = {}) {
  try {
    const report = await riftri(["doctor", "--destination", destination], {
      cwd: repository,
      ...options,
    });
    return Boolean(report?.cow_backend_active);
  } catch (error) {
    if (error.code === "ENOENT") return false; // riftri not installed
    throw error;
  }
}

/** Run any command and resolve with its exit code. Used to call plain git. */
export function run(command, args, { cwd } = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { cwd, stdio: "inherit" });
    child.on("error", reject);
    child.on("close", (code) => resolve(code));
  });
}
