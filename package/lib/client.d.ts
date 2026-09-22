/** Programmatic API for the Riftri CLI. */

export const EXIT_SUCCESS: 0;
export const EXIT_OPERATIONAL: 1;
export const EXIT_USAGE: 2;
export const EXIT_POLICY: 3;

/** One versioned failure receipt, as emitted by `--json-errors`. */
export interface FailureReceipt {
  schemaVersion: number;
  outcome: "failed";
  operation: string;
  code: string;
  category: "policy" | "operational";
  message: string;
  phase: string | null;
  cleanup: string;
  recovery: "required" | "not-required" | "retry" | "inspect" | "unknown";
  nextCommand: string | null;
}

export class RiftriError extends Error {
  readonly exitCode: number;
  readonly receipt: FailureReceipt | null;
  /** Riftri declined before touching anything; falling back is safe. */
  readonly isPolicyRefusal: boolean;
  /** The request was malformed; never retry it unchanged. */
  readonly isUsageError: boolean;
  /** A live process holds the lock; waiting and retrying is correct. */
  readonly isBusy: boolean;
  /** Run `receipt.nextCommand` before continuing. */
  readonly needsRepair: boolean;
}

export interface DoctorReport {
  project_stage: string;
  operating_system: string;
  architecture: string;
  /** Whether a usable copy-on-write backend exists here. */
  cow_backend_active: boolean;
  /** Whether this repository has opted in with `enable()`. */
  repository_enabled: boolean;
  git_shim_active: boolean;
  destination_readiness: {
    destination: string;
    status: string;
    backend: string | null;
    copy_on_write: boolean;
  };
  storage_capabilities: unknown[];
  [key: string]: unknown;
}

export interface AddReport {
  schema_version: number;
  backend: string;
  destination: string;
  destination_native_hex: string;
  commit: string;
  tree: string;
  base_path: string;
  /** False when this add materialized a new immutable base. */
  reused_base: boolean;
  journal_path: string;
  native_path_encoding: string;
}

export interface ManagedWorktree {
  path: string;
  path_native_hex: string;
  repository: string;
  head: string;
  branch: string | null;
  detached: boolean;
  backend: string;
  base_path: string;
  /** Filesystem-accounted bytes this worktree actually occupies. */
  allocated_bytes: number;
  /** Bytes the same content would occupy without sharing. */
  logical_bytes: number;
  locked_reason: string | null;
  prunable_reason: string | null;
}

export interface WorktreeInventory {
  schema_version: number;
  state_directory: string;
  worktrees: ManagedWorktree[];
  native_path_encoding: string;
}

export interface StatusReport {
  schema_version: number;
  state_directory: string;
  bases: unknown[];
  worktrees: unknown[];
  operations: unknown[];
  diagnostic_issues: unknown[];
  total_allocated_bytes: number;
  total_logical_bytes: number;
  [key: string]: unknown;
}

export interface RepairReport {
  schema_version: number;
  scanned: number;
  active: number;
  errors: unknown[];
  [key: string]: unknown;
}

export interface AddOptions {
  /** Create and check out a new branch. */
  branch?: string;
  /** Create a detached worktree instead of a branch. */
  detach?: boolean;
  /** Commit-ish to start from; defaults to HEAD. */
  revision?: string;
  /** Cone-mode sparse directories to include. */
  sparseDir?: string[];
}

export interface RiftriOptions {
  /** Working directory for every command. Defaults to `process.cwd()`. */
  repository?: string;
  /** Explicit executable; defaults to the installed platform binary. */
  binary?: string;
  /** Passed as `--state-dir` where the command supports it. */
  stateDir?: string;
}

export class Riftri {
  constructor(options?: RiftriOptions);
  repository: string;

  /** Inspect Git and storage. Creates nothing. */
  doctor(options?: { destination?: string }): Promise<DoctorReport>;
  backends(path?: string): Promise<unknown>;
  /** Convenience: true when this destination can be optimized. */
  isOptimizable(destination?: string): Promise<boolean>;

  enable(): Promise<null>;
  disable(): Promise<null>;

  status(): Promise<StatusReport>;
  repair(): Promise<RepairReport>;
  gc(options?: { apply?: boolean }): Promise<unknown>;

  /** Escape hatch: run any command and parse its report. */
  run(args: string[], options?: { json?: boolean; cwd?: string }): Promise<unknown>;

  readonly worktree: {
    add(destination: string, options?: AddOptions): Promise<AddReport>;
    list(options?: { allStates?: boolean }): Promise<WorktreeInventory>;
    /** Refuses a dirty worktree unless `force` is set. */
    remove(destination: string, options?: { force?: boolean }): Promise<unknown>;
    move(source: string, destination: string): Promise<unknown>;
    compact(destination: string): Promise<unknown>;
    prune(): Promise<unknown>;
  };
}
