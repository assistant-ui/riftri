"use strict";

// Programmatic API for the Riftri CLI.
//
// The binary is the implementation; this is a typed, promise-returning wrapper
// around it. Every method runs one command, returns its parsed `--json` report,
// and throws a RiftriError carrying the parsed `--json-errors` receipt on
// failure. Nothing here reimplements Riftri behaviour.

const { spawn } = require("node:child_process");
const path = require("node:path");
const { resolveBinary } = require("./platform.js");
const { signalExitCode } = require("./signals.js");

/** Process exit codes. Documented in docs/cli.md. */
const EXIT_SUCCESS = 0;
const EXIT_OPERATIONAL = 1;
const EXIT_USAGE = 2;
const EXIT_POLICY = 3;

/** Commands that accept `--json`. The rest print human text on success. */
const REPORTING = new Set([
  "doctor",
  "backends",
  "status",
  "repair",
  "gc",
  "worktree",
]);

class RiftriError extends Error {
  constructor(exitCode, receipt, stderr, signal = null) {
    super(
      // JSON is not a guarantee of a valid receipt. Coercing an object here
      // can throw inside the child close handler instead of rejecting run().
      (typeof receipt?.message === "string" ? receipt.message : "") ||
        stderr.trim() ||
        (signal ? `riftri terminated by ${signal}` : `riftri exited ${exitCode}`),
    );
    this.name = "RiftriError";
    // Always a number. A signalled process reports 128 + the signal number,
    // the convention native `riftri exec` already uses for its own children.
    this.exitCode = exitCode;
    this.signal = signal;
    this.receipt = receipt ?? null;
  }

  /** The process was killed rather than exiting on its own. */
  get wasSignalled() {
    return this.signal !== null;
  }

  /** Riftri declined before touching anything. Falling back is safe. */
  get isPolicyRefusal() {
    return this.exitCode === EXIT_POLICY;
  }

  /** The request itself was malformed. Never retry unchanged. */
  get isUsageError() {
    return this.exitCode === EXIT_USAGE;
  }

  /** A live process holds the lock; unlike a refusal, waiting helps. */
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
 * Place Riftri's own flags before any `--`. Appending them instead puts them
 * inside the payload `riftri exec` hands to the child, which then fails on a
 * flag meant for Riftri.
 */
function withRiftriFlags(args, flags) {
  if (flags.length === 0) return [...args];
  const boundary = args.indexOf("--");
  if (boundary === -1) return [...args, ...flags];
  return [...args.slice(0, boundary), ...flags, ...args.slice(boundary)];
}

function flagsFor(options) {
  const flags = [];
  for (const [key, value] of Object.entries(options)) {
    if (value === undefined || value === null || value === false) continue;
    const flag = `--${key.replace(/[A-Z]/g, (c) => `-${c.toLowerCase()}`)}`;
    // `--flag=value`, so a value that starts with `-` (a branch, a path) is
    // never parsed as another flag.
    if (value === true) flags.push(flag);
    else if (Array.isArray(value)) for (const item of value) flags.push(`${flag}=${item}`);
    else flags.push(`${flag}=${value}`);
  }
  return flags;
}

class Riftri {
  /**
   * @param {object} [options]
   * @param {string} [options.repository] working directory for every command
   * @param {string} [options.binary] explicit riftri executable
   * @param {string} [options.stateDir] passed as --state-dir where supported
   */
  constructor(options = {}) {
    this.repository = options.repository ?? process.cwd();
    // A relative path is relative to this process, as RIFTRI_BINARY is.
    // Otherwise spawn resolves it against `repository`, the child's cwd. A
    // bare name is still looked up on PATH.
    const binary = options.binary ?? null;
    this.binary = binary && /[\\/]/.test(binary) ? path.resolve(binary) : binary;
    this.stateDir = options.stateDir ?? null;
    this.worktree = {
      add: this.#worktreeAdd.bind(this),
      list: this.#worktreeList.bind(this),
      remove: this.#worktreeRemove.bind(this),
      move: this.#worktreeMove.bind(this),
      compact: this.#worktreeCompact.bind(this),
      prune: this.#worktreePrune.bind(this),
    };
  }

  /** Resolve the executable: explicit, else the installed platform package. */
  #executable() {
    if (this.binary) return this.binary;
    this.binary = resolveBinary();
    return this.binary;
  }

  /**
   * Run one command and resolve with its parsed report, or null when the
   * command does not support `--json`.
   */
  run(args, { json = REPORTING.has(args[0]), cwd = this.repository } = {}) {
    const flags = json ? ["--json", "--json-errors"] : ["--json-errors"];
    return new Promise((resolve, reject) => {
      let child;
      try {
        child = spawn(this.#executable(), withRiftriFlags(args, flags), {
          cwd,
          stdio: ["ignore", "pipe", "pipe"],
        });
      } catch (error) {
        reject(error); // no installed binary for this platform
        return;
      }
      let stdout = "";
      let stderr = "";
      // Pipe chunks need not end on a UTF-8 character boundary. Let each
      // stream retain incomplete bytes between chunks (and flush at EOF).
      if (json) {
        child.stdout.setEncoding("utf8");
        child.stdout.on("data", (chunk) => (stdout += chunk));
      } else {
        // The API returns null for non-reporting commands. Drain their output
        // without decoding or retaining it, so verbose children cannot grow
        // the host heap in proportion to output that will be discarded.
        child.stdout.resume();
      }
      child.stderr.setEncoding("utf8");
      child.stderr.on("data", (chunk) => (stderr += chunk));
      child.on("error", reject);
      child.on("close", (exitCode, signal) => {
        // A signalled child reports a null exit code. Translate it rather than
        // letting `null` reach callers through a field typed as a number.
        if (signal) {
          const code = signalExitCode(signal) ?? EXIT_OPERATIONAL;
          reject(new RiftriError(code, null, stderr, signal));
          return;
        }
        if (exitCode === EXIT_SUCCESS) {
          if (!json) {
            resolve(null);
            return;
          }
          try {
            resolve(JSON.parse(stdout));
          } catch (error) {
            // Thrown from a close handler this would be uncatchable and would
            // take the host process down, so it has to reject instead.
            reject(
              new RiftriError(
                EXIT_OPERATIONAL,
                null,
                `riftri ${args.join(" ")} returned output that is not JSON: ` +
                  `${error.message}\n${stdout.slice(0, 2000)}`,
              ),
            );
          }
          return;
        }
        // The receipt is one JSON line at the end of stderr. Anything else a
        // child wrote there first (a hook's output, a Git warning) must not
        // cost the caller the receipt's code and category.
        let receipt = null;
        const lines = stderr.trimEnd().split("\n");
        for (const candidate of [lines.at(-1), stderr]) {
          try {
            receipt = JSON.parse(candidate);
            break;
          } catch {
            // Plain text, as from a binary that predates usage receipts.
          }
        }
        reject(new RiftriError(exitCode, receipt, stderr));
      });
    });
  }

  #stateFlag() {
    return this.stateDir ? [`--state-dir=${this.stateDir}`] : [];
  }

  /** Inspect Git and storage without creating anything. */
  doctor(options = {}) {
    return this.run(["doctor", ...flagsFor(options)]);
  }

  /** Probe storage backends for a destination volume. */
  backends(pathOrOptions = {}) {
    const target = typeof pathOrOptions === "string" ? ["--", pathOrOptions] : [];
    return this.run(["backends", ...target]);
  }

  /**
   * True when this destination can get an optimized worktree.
   *
   * `false` is an answer from Riftri: either the report says no backend is
   * active, or Riftri refused this destination outright. A broken
   * installation, an unreadable repository, or any other unexpected failure
   * rejects instead, so a caller can tell "not supported here" apart from
   * "this client cannot run at all".
   */
  async isOptimizable(destination) {
    try {
      const report = await this.doctor(destination ? { destination } : {});
      if (!report?.cow_backend_active) return false;
      // cow_backend_active is a fact about the volume, not about this
      // destination. Outside a Git repository it is still true while
      // `worktree add` cannot possibly succeed, so the readiness verdict
      // decides. `needs-activation` stays optimizable: the explicit interface
      // works without `enable`, which only gates Git interception.
      const readiness = report.destination_readiness?.status;
      return readiness === undefined || readiness !== "blocked";
    } catch (error) {
      if (error instanceof RiftriError && error.isPolicyRefusal) return false;
      throw error;
    }
  }

  /** Opt one repository in. Required before interception applies. */
  enable() {
    return this.run(["enable"], { json: false });
  }

  disable() {
    return this.run(["disable"], { json: false });
  }

  /** Bases, reference counts, storage use, and pending operations. */
  status() {
    return this.run(["status", ...this.#stateFlag()]);
  }

  /** Resume or roll back interrupted operations. Safe to run repeatedly. */
  repair() {
    return this.run(["repair", ...this.#stateFlag()]);
  }

  /** Plan base collection; pass `{ apply: true }` to delete. */
  gc({ apply = false } = {}) {
    const args = ["gc", ...this.#stateFlag()];
    if (apply) args.push("--apply", "--yes");
    return this.run(args);
  }

  // Paths and revisions follow `--`: a relative path such as `-scratch` was
  // otherwise parsed as flags and the call failed as a usage error.
  #worktreeAdd(destination, options = {}) {
    const { revision, ...rest } = options;
    const positionals = ["--", destination];
    if (revision) positionals.push(revision);
    return this.run(["worktree", "add", ...flagsFor(rest), ...this.#stateFlag(), ...positionals]);
  }

  #worktreeList(options = {}) {
    // `--all-states` already covers the configured state directory, and Riftri
    // rejects it alongside `--state-dir` as a usage error.
    const state = options.allStates ? [] : this.#stateFlag();
    return this.run(["worktree", "list", ...flagsFor(options), ...state]);
  }

  #worktreeRemove(destination, { force = false } = {}) {
    const args = ["worktree", "remove"];
    if (force) args.push("--force", "--yes");
    return this.run([...args, ...this.#stateFlag(), "--", destination]);
  }

  #worktreeMove(source, destination) {
    return this.run(["worktree", "move", ...this.#stateFlag(), "--", source, destination]);
  }

  #worktreeCompact(destination) {
    return this.run(["worktree", "compact", ...this.#stateFlag(), "--", destination]);
  }

  #worktreePrune() {
    return this.run(["worktree", "prune", ...this.#stateFlag()]);
  }
}

module.exports = {
  Riftri,
  RiftriError,
  EXIT_SUCCESS,
  EXIT_OPERATIONAL,
  EXIT_USAGE,
  EXIT_POLICY,
};
