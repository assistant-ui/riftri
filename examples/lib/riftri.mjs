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

/**
 * Exit code for a signalled process, following the shell convention native
 * `riftri exec` itself uses: 128 plus the signal number. Kept inline so this
 * file stays copyable with no imports.
 */
const SIGNAL_NUMBERS = {
  SIGHUP: 1, SIGINT: 2, SIGQUIT: 3, SIGABRT: 6, SIGKILL: 9,
  SIGSEGV: 11, SIGPIPE: 13, SIGALRM: 14, SIGTERM: 15,
};
export function signalExitCode(signal) {
  const number = SIGNAL_NUMBERS[signal];
  return number === undefined ? null : 128 + number;
}

/** Thrown for any non-zero exit, carrying the parsed receipt when present. */
export class RiftriError extends Error {
  constructor(code, receipt, stderr, signal = null) {
    // `||`, not `??`: an empty stderr string is not nullish, so `??` would keep
    // it and hide the exit code behind an empty message.
    super(
      receipt?.message ||
        stderr.trim() ||
        (signal ? `riftri terminated by ${signal}` : `riftri exited ${code}`),
    );
    this.name = "RiftriError";
    this.code = code; // process exit code (128 + signal number when signalled)
    this.receipt = receipt ?? null; // parsed --json-errors receipt, or null
    this.signal = signal; // POSIX signal name when killed, else null
  }

  /** The process was killed by a signal rather than exiting on its own. */
  get wasSignalled() {
    return this.signal !== null;
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
    const child = spawn(bin, withRiftriFlags(args, flags), {
      cwd,
      stdio: ["ignore", "pipe", "pipe"],
    });

    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => (stdout += chunk));
    child.stderr.on("data", (chunk) => (stderr += chunk));

    child.on("error", reject); // riftri is not installed or not on PATH
    child.on("close", (code, signal) => {
      // A signalled child reports a null exit code. Translate it so callers
      // always see a number, matching native `riftri exec`.
      if (signal) {
        reject(new RiftriError(signalExitCode(signal) ?? EXIT.OPERATIONAL, null, stderr, signal));
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
          // Throwing from a close handler is uncatchable and takes the host
          // process down, so reject with a useful error instead.
          reject(
            new RiftriError(
              EXIT.OPERATIONAL,
              null,
              `riftri ${args.join(" ")} returned output that is not JSON: ${error.message}\n${stdout.slice(0, 2000)}`,
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
 * Place Riftri's own flags before any `--`. Appending them puts them inside
 * the payload `riftri exec` hands to the child, so they become the agent's
 * arguments instead of Riftri's.
 */
function withRiftriFlags(args, flags) {
  const boundary = args.indexOf("--");
  if (boundary === -1) return [...args, ...flags];
  return [...args.slice(0, boundary), ...flags, ...args.slice(boundary)];
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
    // A signalled child reports a null code; return 128 + signal number so the
    // contract "resolves with an exit code" always holds.
    child.on("close", (code, signal) =>
      resolve(signal ? (signalExitCode(signal) ?? 1) : code),
    );
  });
}
