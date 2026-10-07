/** Programmatic API for the Riftri CLI. */

export const EXIT_SUCCESS: 0;
export const EXIT_OPERATIONAL: 1;
export const EXIT_USAGE: 2;
export const EXIT_POLICY: 3;

/** Resolve and validate the installed native executable, honoring RIFTRI_BINARY. Does not spawn. */
export function resolveBinary(): string;

/** One versioned failure receipt, as emitted by `--json-errors`. */
export interface FailureReceipt {
  schemaVersion: number;
  outcome: "failed";
  /** Null for a usage error: parsing failed before a command was chosen. */
  operation: string | null;
  code: string;
  /** Mirrors the exit code: 1 operational, 2 usage, 3 policy. */
  category: "policy" | "operational" | "usage";
  message: string;
  phase: string | null;
  cleanup: string;
  recovery: "required" | "not-required" | "retry" | "inspect" | "unknown";
  nextCommand: string | null;
  repository: string | null;
  repositoryNativeHex: string | null;
  stateDirectory: string | null;
  stateDirectoryNativeHex: string | null;
  nativePathEncoding: string;
}

export class RiftriError extends Error {
  /**
   * Always a number. A process killed by a signal reports 128 plus the signal
   * number, the convention native `riftri exec` uses for its own children.
   */
  readonly exitCode: number;
  /** The signal that killed the process, or null if it exited on its own. */
  readonly signal: NodeJS.Signals | null;
  /** The process was killed rather than exiting on its own. */
  readonly wasSignalled: boolean;
  readonly receipt: FailureReceipt | null;
  /** Parsed stdout on a nonzero exit (for example a failed post-checkout hook), or null. Validate before use. */
  readonly report: unknown;
  /** A native policy receipt confirms no mutation; an exit code alone is insufficient. */
  readonly isPolicyRefusal: boolean;
  /** The request was malformed; never retry it unchanged. */
  readonly isUsageError: boolean;
  /** A live process holds the lock; waiting and retrying is correct. */
  readonly isBusy: boolean;
  /** Free space on the affected volume before following recovery guidance. */
  readonly isStorageFull: boolean;
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
  /** The `post-checkout` hook Git would run, or null when there is none. */
  post_checkout: { hook: string; started: boolean; exit_code: number | null } | null;
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
  schema_version: 1;
  state_directory: string;
  state_directory_native_hex: string;
  worktrees: ManagedWorktree[];
  diagnostic_issues: StateDiagnosticIssue[];
  native_path_encoding: string;
}

export interface WorktreeOwner {
  schema_version: 1;
  /** Null only when no registered state claims the path. Errors reject. */
  state_directory: string | null;
  state_directory_native_hex: string | null;
  native_path_encoding: string;
}

export interface StateDiagnosticIssue {
  path: string;
  path_native_hex: string;
  reason: string;
  may_hide_base_reference: boolean;
}

export interface AllStatesManagedWorktree extends ManagedWorktree {
  state_directory: string;
  state_directory_native_hex: string;
}

export interface AllStatesDiagnosticIssue extends StateDiagnosticIssue {
  /** Null for a registration issue that belongs to no usable state directory. */
  state_directory: string | null;
  state_directory_native_hex: string | null;
}

export interface AllStatesWorktreeInventory {
  schema_version: 2;
  scope: "all-registered-states";
  state_directories: {
    path: string;
    path_native_hex: string;
    source: "default" | "registered";
  }[];
  worktrees: AllStatesManagedWorktree[];
  diagnostic_issues: AllStatesDiagnosticIssue[];
  native_path_encoding: string;
}

/** Lifecycle counters; lower bounds when StatusReport.counts_complete is false. */
export interface StatusOperations {
  active_views: number;
  pending_adds: number;
  completed_removals: number;
  cancelled_removals: number;
  pending_removals: number;
  completed_moves: number;
  cancelled_moves: number;
  pending_moves: number;
  completed_compactions: number;
  cancelled_compactions: number;
  pending_compactions: number;
  completed_prunes: number;
  pending_prunes: number;
  completed_collections: number;
  cancelled_collections: number;
  pending_collections: number;
  coordination_locks: number;
}

export interface StatusReport {
  schema_version: number;
  state_directory: string;
  bases: unknown[];
  worktrees: unknown[];
  /** False when unreadable journals make operation counts lower bounds. */
  counts_complete: boolean;
  operations: StatusOperations;
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
  /**
   * Explicit executable; defaults to the installed platform binary. A
   * relative path is relative to `process.cwd()`, not `repository`.
   */
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
  /**
   * True when this destination can be optimized.
   *
   * Resolves `false` only when Riftri answers the question: no active
   * copy-on-write backend, a destination Riftri reports as `blocked`, or an
   * outright refusal. A missing or non-executable binary, an unreadable
   * repository, or output that is not JSON rejects instead.
   *
   * A `needs-activation` destination is optimizable: `worktree.add` works
   * without `enable()`, which only gates Git interception.
   */
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
    /** Advisory ownership lookup without disk accounting; mutations revalidate. */
    owner(destination: string): Promise<WorktreeOwner>;
    list(options: { allStates: true }): Promise<AllStatesWorktreeInventory>;
    list(options?: { allStates?: false }): Promise<WorktreeInventory>;
    /** Narrow schema_version when allStates is a runtime boolean. */
    list(options?: { allStates?: boolean }): Promise<WorktreeInventory | AllStatesWorktreeInventory>;
    /** Refuses a dirty worktree unless `force` is set. */
    remove(destination: string, options?: { force?: boolean }): Promise<unknown>;
    move(source: string, destination: string): Promise<unknown>;
    compact(destination: string): Promise<unknown>;
    prune(): Promise<unknown>;
  };
}
