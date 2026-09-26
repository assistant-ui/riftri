// A minimal Riftri client for a harness runner. No dependencies.
//
// The whole integration surface is here: spawn the CLI, read one JSON report
// from stdout, read one JSON receipt from stderr on failure, and branch on the
// exit code. Copy this file next to your runner and adapt it.

import { spawn } from "node:child_process";
import os from "node:os";

/** Exit codes documented in docs/cli.md. */
export const EXIT = {
  SUCCESS: 0,
  OPERATIONAL: 1, // Git, storage, journal, or filesystem I/O. Retry may help.
  USAGE: 2, // Bad flag or argument. Never retry.
  POLICY: 3, // Riftri refused. Nothing changed. Fall back or stop.
};

/** Thrown for any non-zero exit, carrying the parsed receipt when present. */
export class RiftriError extends Error {
  constructor(exitCode, receipt, stderr, signal = null) {
    // `??` would keep an empty string, so a silent failure had no message.
    super(
      receipt?.message ||
        stderr.trim() ||
        (signal ? `riftri terminated by ${signal}` : `riftri exited ${exitCode}`),
    );
    this.name = "RiftriError";
    // Named exitCode, not code: Node puts strings like "ENOENT" on error.code,
    // and one field meaning both is how a spawn failure gets mistaken for an
    // exit status. Always a number; a signalled process reports 128 + signal.
    this.exitCode = exitCode;
    this.signal = signal;
    this.receipt = receipt; // parsed --json-errors receipt, or null
  }

  /** The process was killed rather than exiting on its own. */
  get wasSignalled() {
    return this.signal !== null;
  }

  /** Riftri declined before touching anything: safe to fall back. */
  get isPolicyRefusal() {
    return this.exitCode === EXIT.POLICY;
  }

  /** The request itself was malformed. Never retry unchanged. */
  get isUsageError() {
    return this.exitCode === EXIT.USAGE;
  }

  /** A live process holds the lock. Unlike a refusal, waiting helps. */
  get isBusy() {
    return this.receipt?.code === "worktree-busy";
  }

  /** The affected volume needs free space before recovery can proceed. */
  get isStorageFull() {
    return this.receipt?.code === "storage-full";
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
    // Riftri's flags must precede any `--`. Appended, they end up in the
    // payload `riftri exec` hands to the child, which then fails on a flag
    // that was never meant for it.
    const boundary = args.indexOf("--");
    const argv =
      boundary === -1
        ? [...args, ...flags]
        : [...args.slice(0, boundary), ...flags, ...args.slice(boundary)];
    const child = spawn(bin, argv, {
      cwd,
      stdio: ["ignore", "pipe", "pipe"],
    });

    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => (stdout += chunk));
    child.stderr.on("data", (chunk) => (stderr += chunk));

    child.on("error", reject); // riftri is not installed or not on PATH
    child.on("close", (code, signal) => {
      // A signalled child reports a null exit code; report the signal rather
      // than letting null pass for a status.
      if (signal) {
        const number = os.constants.signals[signal];
        reject(
          new RiftriError(
            Number.isInteger(number) ? 128 + number : EXIT.OPERATIONAL,
            null,
            stderr,
            signal,
          ),
        );
        return;
      }
      if (code === EXIT.SUCCESS) {
        // Without --json the command prints human text, so do not parse it.
        if (!json || !stdout.trim()) {
          resolve(null);
          return;
        }
        try {
          resolve(JSON.parse(stdout));
        } catch (error) {
          // Throwing here is uncatchable by the caller and kills the host
          // process, so this has to reject.
          reject(
            new RiftriError(
              EXIT.OPERATIONAL,
              null,
              `riftri ${args.join(" ")} returned output that is not JSON: ` +
                `${error.message}\n${stdout.slice(0, 2000)}`,
            ),
          );
        }
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

/**
 * True when this destination can actually get an optimized worktree.
 *
 * `false` means Riftri answered: no backend here, or it refused this
 * destination. A missing binary or a broken setup throws, so a harness does
 * not read "riftri is not installed" as "this repository is unsupported".
 * See fallback-detection for handling that case deliberately.
 */
export async function isOptimizable(repository, destination, options = {}) {
  try {
    const report = await riftri(["doctor", "--destination", destination], {
      cwd: repository,
      ...options,
    });
    if (!report?.cow_backend_active) return false;
    // A COW-capable volume is not the same as a usable destination: outside a
    // Git repository this stays true while `worktree add` cannot succeed.
    return report.destination_readiness?.status !== "blocked";
  } catch (error) {
    if (error instanceof RiftriError && error.isPolicyRefusal) return false;
    throw error;
  }
}

/**
 * Run any command and resolve with its exit code. Used to call plain git.
 *
 * A signalled child resolves 128 + the signal number, the shell convention,
 * so the result is always a number a caller can compare against zero.
 */
export function run(command, args, { cwd } = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { cwd, stdio: "inherit" });
    child.on("error", reject);
    child.on("close", (code, signal) => {
      if (signal) {
        const number = os.constants.signals[signal];
        resolve(Number.isInteger(number) ? 128 + number : EXIT.OPERATIONAL);
        return;
      }
      resolve(code);
    });
  });
}
