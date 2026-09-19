# Changelog

All notable changes will be documented here. Riftri follows Semantic Versioning
for its Rust CLI and npm distribution packages as one synchronized release.

## Unreleased

### Fixed

- Recovery guidance no longer sends callers to the wrong Riftri state. Failure
  receipts and the human-readable messages beside them previously interpolated
  a state directory with `Path::display` and no shell quoting, so a repository
  under a path containing a space split into two shell arguments; `riftri
  repair` then inspected a directory that did not exist and reported "No
  journaled operation needs manual attention" while the real pending journal
  sat untouched. Suggested commands now quote every path for the platform
  shell, and a path that cannot be written as a shell argument — non-Unicode,
  or containing control characters — produces no command at all rather than a
  broken one.

- Failure receipts carry the repository and state directory the failing
  invocation actually selected, so `nextCommand` targets that state instead of
  whatever the caller's working directory would resolve to. Receipts also gain
  `repository`, `stateDirectory`, their `*NativeHex` twins, and
  `nativePathEncoding`, so automation can act on the exact native path without
  parsing a shell string. `schemaVersion` stays `1`; the fields are additive.

- `riftri repair`, `status`, `gc`, and `worktree list` now refuse an explicitly
  named `--state-dir` that does not exist, with a diagnostic and exit code 3,
  instead of treating the missing directory as empty and reporting an
  all-clear. A repository that has simply never created Riftri state is
  unaffected and still reports an all-clear with exit code 0.

- `riftri status` and `riftri gc` name the state directory they are reporting
  on in their `riftri repair` and `riftri gc --apply` hints, and `riftri
  doctor` and `riftri setup` share one quoting helper with these paths.
- `riftri exec` no longer strips a scoped command of the signal dispositions it
  should have inherited. The command now starts from the disposition Riftri
  itself inherited for every signal Riftri touches, not just the ones Riftri
  ignores, so `nohup riftri exec -- <command>` survives a hangup exactly like
  bare `nohup <command>`, and `riftri exec` started asynchronously by a shell
  without job control keeps the ignored SIGINT and SIGQUIT that POSIX requires
  for a background job. Commands launched from an ordinary foreground shell are
  unaffected and still see Ctrl-C.
- Terminating `riftri exec -- git …` no longer orphans the real Git. The scoped
  `git` is Riftri's own shim, which previously died instantly on a forwarded
  SIGTERM or SIGHUP and left a `clone` or `fetch` running and still writing.
  The shim now applies the same termination contract to its delegation, so the
  signal reaches the real Git process, and it still propagates Git's exit
  status under the `128 + signal` rule.
- Intercepted `git worktree prune` no longer refuses ordinary invocations in an
  enabled repository that holds managed Riftri state. `--no-optional-locks`,
  which VS Code and most IDE Git integrations pass unconditionally, now reaches
  the journaled prune, and the read-only `-n`/`--dry-run`, `-v`/`--verbose`, and
  `-h`/`--help` forms delegate to real Git because they change nothing. Options
  that could remove lifecycle metadata outside the journal are still refused,
  now quoting the arguments actually passed and naming `RIFTRI_BYPASS=1`.
- `git worktree add --help` and `git worktree add -h` print Git's usage instead
  of being refused as unsupported options, matching `worktree remove -h` and
  `worktree list -h`. An add carrying only checkout-neutral global options
  (`--no-optional-locks`, `--no-advice`, `--literal-pathspecs`) delegates to
  ordinary Git rather than failing.
- `git worktree add <path> <commit-ish>` now refuses a tag, a raw commit, a
  remote-tracking ref, or `HEAD` before running any Git process, explaining that
  the optimized path checks out an existing local branch and pointing at
  `--detach`, `-b <new-branch>`, and `RIFTRI_BYPASS=1`. It previously failed
  part-way through with a misleading "existing local branch does not exist".

## [0.3.1] - 2026-09-18

### Added

- `riftri setup` guides terminal users through destination checks and an
  explicitly confirmed, journaled worktree creation at `HEAD`, then offers an
  optional installed-agent choice. A separate confirmation enables the
  repository and starts the chosen executable through `riftri exec --worktree`.
  Cancellation preserves completed work, no agent or shell profile is installed
  or changed, and automation continues to use the existing explicit commands.
- An optional `riftri-worktrees` agent skill documents explicit creation,
  process-scoped Git integration, and permission-respecting cleanup. It is
  installable through the skills CLI and is not required for COW correctness.

### Documentation

- The Markdown guide at `riftri.dev/index.md` explains guided setup, opening an
  agent in a ready COW worktree, and wrapping an agent that creates additional
  worktrees. Installation examples and package metadata now target v0.3.1.

## [0.3.0] - 2026-09-18

### Added

- `enable`, `disable`, `doctor`, `status`, `repair`, `gc`, and `shell status`
  now accept `--repository <PATH>`, matching the worktree and state commands.
  Existing positional repository arguments still work; passing both forms is
  rejected as ambiguous before running the command.

- `riftri worktree add --sparse-dir <DIR>` (repeatable) creates cone-mode
  sparse worktrees through Git's real sparse-checkout and skip-worktree
  semantics. The canonical directory list becomes part of the versioned
  checkout profile and immutable-base key, so different selections at the same
  commit never share a base and full worktrees keep their existing bases.
  Sparse patterns, nonexistent directories, repository-configured sparse
  checkout, intercepted sparse adds, sparse plus Git LFS, and sparse
  compaction are refused with precise diagnostics before any state is created.
  See [docs/sparse-checkout.md](docs/sparse-checkout.md).
- `riftri worktree add`, `riftri repair`, and `riftri gc` report lifecycle
  progress on stderr: one plain line per durable journal phase, plus explicit
  lines when an operation waits on a contended coordination lock and when it
  resumes. Lines reflect only states an operation genuinely reached — no
  percentages, timers, or terminal control sequences. The new global
  `--no-progress` flag suppresses them, `--json-errors` implies that
  suppression so its stderr stays exactly one JSON receipt, and `--json`
  stdout remains a single valid report. `riftri-core` gains an optional
  `progress::set_progress_observer` hook that emits these phase events
  without changing lifecycle behaviour.
- `riftri worktree list --all-states` inventories managed worktrees across the
  default state location and every state directory the repository registers,
  so worktrees created with a custom `--state-dir` are discoverable without
  repeating the path. The default single-state scope is unchanged. The new
  scope's JSON report uses `schema_version` 2, names each worktree's owning
  state directory, deduplicates equivalent registrations, filters shared state
  directories to the queried repository, and reports missing, non-absolute, or
  symlinked registrations as diagnostic entries without traversing them.
  Discovery stays strictly read-only.
- A staged Homebrew tap directory at `package/homebrew/` mirrors the planned
  `assistant-ui/homebrew-riftri` tap repository, with
  `package/scripts/sync-homebrew-tap.mjs` regenerating every checked-in
  formula copy directly from a release's published `SHA256SUMS`. Tests and the
  scheduled freshness workflow fail when the copies diverge, drift from the
  generator, or fall behind the latest release. The tap install command is
  documented as pending maintainer setup; installing from a checkout remains
  the supported Homebrew path.

### Changed

- The standalone `Website deployment check` workflow is removed. It duplicated
  the post-deploy verification that the `Website deploy` workflow already runs
  against https://riftri.dev, and its `deployment_status` trigger produced a
  redundant failing check on unrelated pull requests.
- `riftri worktree add` spawns three fewer Git processes per creation (21 to
  18 cached, 27 to 24 cold on macOS): the two attribute-compatibility passes
  share one `read-tree` temporary index, the compatibility analysis reuses the
  already-resolved common Git directory instead of re-running `rev-parse`, and
  the separate `update-index --refresh` is gone because the fail-closed clean
  check performs the same full refresh. Corruption detection, index contents,
  and the clean-creation guarantee are unchanged, and a new integration test
  guards the per-add Git invocation budget.
- `riftri state unregister <PATH>` replaces the displayed `state forget-missing`
  command. The old name remains a hidden compatibility alias, and existing paths
  still cannot be unregistered. Help, completions, and generated man pages use
  the new name.

### Fixed

- Evaluating `riftri shell deactivate <shell>` inside a `riftri exec` session
  no longer breaks normal Git commands. The emitted code now removes the
  process-scoped exec shim entries from `PATH` (including nested scopes) and
  unsets `RIFTRI_PROCESS_SHIM_DIR` alongside the other shim variables, so
  `git --version` reports the real Git and non-worktree commands such as
  `git log` keep working for the rest of the session. Independently, every
  Git shim now records the captured real Git path at creation and fails safe:
  a shim whose environment was stripped delegates invocations to the real Git
  unchanged instead of answering as the Riftri CLI.
- npm publication now waits for every native platform package to be visible
  before publishing the launcher, preventing a propagation delay or incomplete
  platform release from producing an unusable first-time install.

- Lifecycle operations now reject inherited Git repository/index overrides
  before mutation, preventing worktree creation from resetting the source
  repository's staged index through `GIT_DIR`. Ordinary Git passthrough is unchanged.
- Interrupted-add recovery preserves staged modifications, staged deletions,
  intent-to-add entries, and conflicts even when working files match the base.
  Recovery initializes only a missing index and never overwrites an existing one.

- Interactive `riftri exec` no longer dies from Ctrl-C while its scoped
  command survives the interrupt. With a foreground controlling terminal,
  `riftri exec` now ignores SIGINT and SIGQUIT while waiting — the terminal
  still delivers both to the whole foreground process group, so the command
  alone decides whether the interrupt is fatal — and keeps waiting so shim
  cleanup and exit-status propagation still happen (including 130 when the
  command does die from SIGINT). The command starts with its inherited
  dispositions restored, prior dispositions are reinstated after the command
  is reaped, and supervised process-group forwarding is unchanged.
- Interrupted-add recovery preserves ignored files and unexpected empty
  directories, even when Git reports a clean worktree. Rollback verifies the
  complete view before deleting it; explicit removal semantics are unchanged.
- Repair of an interrupted forced managed removal no longer deletes a worktree
  whose metadata changed after force intent was recorded. The forced-removal
  snapshot now covers each entry's full native permission bits (setuid,
  setgid, and sticky included) and, on Unix, its extended attribute names and
  values, so a metadata-only change preserves the worktree and reports why.
  Pending snapshots recorded by older versions can never match the upgraded
  digest and therefore also fail closed instead of authorizing deletion.
  Immutable-base content hashing is unchanged, so existing cached bases stay
  valid.
- macOS compaction now refuses extended ACLs instead of silently dropping
  access-control rules. Native, symlink-aware inspection protects initial
  compaction and recovery cleanup without changing ordinary snapshot formats.
- Terminating the native `riftri exec` with SIGTERM, SIGINT, or SIGHUP now
  forwards the signal to the scoped command instead of orphaning it.
  Supervised invocations run the command in its own process group and signal
  that whole group, so the command's descendants stop with it without touching
  unrelated processes; interactive foreground invocations keep terminal job
  control unchanged and forward SIGTERM and SIGHUP to the command itself.
  Riftri then removes its temporary Git shim and exits with the command's
  status. The npm launcher's own signal forwarding is tracked separately
  (#180).
- `--json-errors` receipts for a lifecycle command blocked by a pending
  operation now agree with the human-readable guidance: they report
  `"code": "recovery-pending"`, `"category": "operational"` (exit code 1),
  `"recovery": "required"`, and a `nextCommand` of
  `riftri repair --state-dir <state-dir>` naming the state directory that
  holds the pending journal. Previously these receipts claimed
  `"recovery": "not-required"` with no next command while the message said to
  run repair. Genuine policy refusals still report
  `"recovery": "not-required"`. A worktree whose operation lock is held by a
  live process is reported separately as `"code": "worktree-busy"` with
  `"recovery": "retry"`, since waiting and retrying — not repair — is the
  correct response there.
- Managed removal, forced removal, and compaction reject worktrees with an
  incomplete move journal until repair completes the move.
- State unregistration accepts relative parent components such as `../old-state`
  and resolves existing parent aliases when matching a missing registered path,
  without deleting files or unregistering existing paths.
- The website storage diagram shows the full backend status when text wraps
  near the mobile layout breakpoint.
- Release preparation uses the selected repository for the root launcher
  package as well as the native packages.
- The static website preview serves `robots.txt` as plain text and `sitemap.xml`
  as XML instead of `application/octet-stream`.
- `riftri shell status` reports optimized interception as inactive when
  `RIFTRI_BYPASS` is active, without reporting that the shell hook is inactive.
- `riftri status` reports a file or symlink at `overlays/v1` as an unsafe
  layout root without traversal or removal of the path.
- Status keeps completed and cancelled compaction counts after later compactions
  or managed moves, without false unsafe-journal diagnostics.
- Linux mount identity checks keep complete paths that contain carriage-return
  or form-feed bytes.
- Batched Git configuration reads keep case-sensitive subsection names and
  lowercase only section and variable names.
- Intercepted `git worktree add --quiet` commands no longer print a success
  message. Error messages remain visible.
- Shell status and deactivation retain the activated shim path after a directory
  change when `RIFTRI_CACHE_DIR` is relative.
- `riftri doctor --json` preserves non-UTF-8 paths through display strings and
  exact native hexadecimal fields instead of rejecting the report.
- Doctor's suggested worktree command quotes the destination as one shell argument,
  including paths with spaces or apostrophes.
- `riftri backends --json` emits a versioned report that preserves non-UTF-8
  requested and probe paths through display strings and exact native
  hexadecimal fields instead of rejecting serialization. The report is now an
  object with `schema_version` 1 wrapping the previous capability array as
  `storage_capabilities`.
- Git worktree remove and move commands that use a unique path suffix no longer
  bypass Riftri's lifecycle journals for managed worktrees.

## 0.2.3 - 2026-09-16

This release changes documentation, tests, and tooling only. The CLI itself is
unchanged since 0.2.2: no behaviour, output, or on-disk format differs, and
upgrading is optional.

### Added

- `docs/build-caches.md` explains which dependency stores are safe to share
  across parallel worktrees and which working directories never are, since
  untracked dependency and build directories are outside the Git tree and are
  therefore never shared by copy-on-write.
- A Homebrew formula at `Formula/riftri.rb`, generated from a release's
  published checksums by `package/scripts/update-homebrew-formula.mjs`, with a
  scheduled check that fails when it falls behind the latest release or pins a
  checksum the release does not publish.
- Test coverage for the destination path preflight across non-ASCII case
  folding, both Unicode normalization forms, and colliding directory
  components, verified against the behaviour of the destination volume itself.

### Changed

- The README is roughly half its previous length; reference material it
  duplicated now lives in the `docs/` pages that own it.
- `docs/filesystem-compatibility.md` documents the destination path preflight
  and its scope instead of deferring case folding and normalization to a
  future compatibility slice.

### Fixed

- The Markdown link checker no longer deadlocks pull requests that add a
  document, which previously could not link to themselves at `main` until after
  merging.

## 0.2.2 - 2026-09-16

### Added

- Checkout-neutral GitHub linguist metadata attributes (`linguist-generated`,
  `linguist-vendored`, `linguist-documentation`, `linguist-detectable`, and
  `linguist-language`) no longer block optimized worktree creation, so stock
  repositories using lockfile display hints work without edits.
- Optimized explicit and intercepted worktree adds can attach an existing local
  branch while preserving Git's branch-in-use checks and Riftri's exact-commit
  transaction boundary.
- `riftri worktree list` reports active managed worktrees in concise human or
  versioned JSON form with Git identity, backend, base, and allocation details.
- A checksum-verifying PowerShell installer provides profile-free per-user
  installation for Windows x64 and ARM64.
- Canonical Git LFS checkouts accept strict v1 pointers backed by verified
  objects already present in the default local LFS store.
- Snapshot-guarded forced managed removal safely supports intentional discard
  while preserving any changes made after force intent is recorded.
- Explicit compaction recreates pristine native-COW views to release retained
  private blocks without changing their Git worktree registration or HEAD.
- Explicitly evaluated PowerShell activation and deactivation provide the same
  repository-gated normal Git interception as sh, bash, and zsh without editing
  PowerShell profiles or persistent `PATH`.
- Shell completion generation, generated man pages, and a Homebrew formula
  generator backed by checksummed native release archives.
- Stable JSON success reports for lifecycle commands, binary-unit byte counts,
  and a `--branch` alias for explicit adds.
- CLI confirmation prompts, distinct error exit codes, and practical help
  examples; documentation now includes a CLI reference, troubleshooting,
  comparisons, and agent integration guidance.

### Fixed

- Website animation controls now keep their accessible Pause/Resume labels in
  sync, clipboard feedback restarts on every copy, and keyboard navigation
  moves focus to the requested section.
- The local static preview now follows branded 404 routes and file overrides,
  while preserving response headers and rejecting paths outside its output.
- Improved website text contrast, Windows installation instructions, sharing
  metadata, and inline access to the Markdown guide at `riftri.dev/index.md`.

### Compatibility

- The redundant `riftri recover` command was removed; use `riftri repair` for
  repository-aware recovery. Scripts should also account for the documented
  distinct error exit codes. Destructive confirmations apply only in terminals;
  `--yes` explicitly skips those prompts.

### Release

- Native GitHub release assets include signed build-provenance attestations.
  npm publication remains paused pending registry review; standalone downloads
  and the Bash and PowerShell installers remain available.
- CI now checks the minimum Rust version, dependency policy, code coverage,
  Markdown links, browser interactions, and public deployment contents.
- Production website verification checks the deployed revision, installers,
  Markdown response, sharing assets, and branded error page.

## 0.2.1 - 2026-09-13

### Fixed

- OverlayFS unmounts retry transient busy results with bounded backoff while
  revalidating the exact journaled mount identity before every retry.
- Rollback race coverage now synchronizes directly at Git's removal boundary
  and asserts structured failures while proving concurrent writes are preserved.

### Release

- Partial npm publications safely skip completed exact versions, keep the
  launcher last, and verify all nine packages after bounded registry retries.

### Documentation

- Project, roadmap, architecture, and agent guidance now consistently record
  the completed Linux milestone and the journal-backed lifecycle registry.

## 0.2.0 - 2026-09-13

### Added

- A checksum-verified Bash installer for macOS and Linux, with pinned versions,
  atomic per-user upgrades, and a copyable command on the website.
- Standalone direct downloads and npm packages for eight native targets,
  including ARM64 and x64 Linux builds for both glibc and musl.
- Destination-readiness diagnostics that explain the selected backend and give
  a concrete next command or remedy.
- Stable JSON lifecycle failure receipts for automation, including the failure
  category, transaction phase, cleanup result, recovery state, and next command.
- A documented cross-backend guarantee contract and versioned checkout metadata
  profile for APFS, Linux reflink and OverlayFS, and Windows ReFS.

### Changed

- Native reflink and ReFS tree cloning now processes independent regular files
  concurrently with a bounded worker pool while preserving ordered metadata and
  cleanup behavior.
- Exact immutable-base verification can run concurrently for readers, reducing
  contention between parallel worktree creations.
- Checkout configuration is read in batches instead of starting Git once per
  setting.

### Fixed

- GitHub native-archive releases publish independently of npm after shared
  staging checks. Exact asset and checksum validation rejects incomplete or
  changed downloads; release retries never overwrite published assets.
- Linux filesystem detection now uses a libc-independent representation, so
  both GNU and musl builds compile and select backends consistently.

### Testing

- Installed-package lifecycle tests now exercise real APFS, Btrfs,
  reflink-enabled XFS, helper-backed OverlayFS, and ReFS environments.
- Deterministic race hooks verify that cleanup preserves concurrent files and
  rejects path substitution without modifying data outside managed state.
- Native allocation benchmarks cover normal checkout comparisons, concurrent
  shared-base readers, and an evaluation-only APFS bulk-clone candidate.

## 0.1.1 - 2026-09-12

The first public distribution includes the 0.1.0 foundation and the following
platform, lifecycle, and safety improvements.

### Added

- Native Linux reflink worktrees on Btrfs and reflink-enabled XFS, plus Windows
  ReFS block-clone worktrees with real-filesystem CI and allocation checks.
- Linux OverlayFS worktrees with caller-namespace capability checks, an
  explicitly installed least-privilege helper, crash-gap mount adoption, and
  explicit reboot recovery that preserves private changes.
- Repository compatibility preflight, custom state-directory discovery, and
  conservative repair of stale state registrations.
- Deterministic in-tree text, line-ending, and binary attribute support;
  unsupported filters, encodings, and external attributes remain fail-closed.

### Fixed

- Interrupted-add recovery preserves detached commits and other changed HEADs
  instead of removing a worktree whose files happen to be clean (#90).
- Repair skips live adds under per-operation locks and reloads journals after
  acquiring ownership, preventing concurrent rollback of an active creator (#93).
- Unicode normalization and case collisions are checked on the destination
  filesystem before durable state or Git metadata is created (#91).
- Fork-only OverlayFS probes release unrelated inherited file descriptors,
  preventing leaked mount and lock references (#92).
- Immutable bases are verified before reuse, checkout inputs stay pinned to
  the resolved tree, and cleanup preserves writes racing with Git removal.
- Journal snapshots, temporary files, state paths, Git pointers, and storage
  accounting reject unsafe or inconsistent inputs without deleting user data.

### Compatibility

- Concurrent add and repair processes must all use the lock-aware version.
  An older running binary cannot participate in the new ownership protocol.
- New verified base-cache buckets do not reuse bases from the older checkout
  profile. Important work should still be committed or backed up: Riftri remains
  experimental, and unsupported filesystems never silently fall back to copies.

## 0.1.0 - 2026-09-09

### Added

- Rust workspace with CLI, core, Git, and storage crate boundaries.
- Destination-specific, read-only storage capability diagnostics.
- Git repository identity, object resolution, and NUL-delimited worktree parsing.
- Versioned base-key, checkout-profile, and operation-journal models.
- npm launcher with platform-specific native binary packages.
- GitHub CI and tag-driven GitHub/npm release automation.
- Explicit `riftri worktree add` support on writable APFS volumes.
- Exact-tree immutable-base creation and reuse with serialized construction.
- Strict APFS COW cloning for regular files with symlink and executable-mode support.
- Atomic native-path add journals and `riftri recover` for interrupted operations.
- Repository-local activation through `riftri enable` and `riftri disable`.
- Process-scoped `riftri exec` Git interception for supported adds in enabled repositories.
- Optional validated worktree binding for any process-scoped command through
  `riftri exec --worktree <path> -- <command>`.
- Explicit `riftri shell hook` activation so normal Git adds are intercepted in
  enabled repositories without wrapping each command.
- A concurrent APFS integration fixture that proves parallel adds serialize
  immutable-base construction and preserve independent Git commits and files.
- Journaled clean-worktree removal through the explicit CLI and enabled Git
  shim, with conservative interrupted-removal recovery.
- `riftri status` reporting for retained bases, active views, reference counts,
  logical bytes, and filesystem-allocated bytes.
- Repository-aware `riftri repair` for conservative add/removal journal recovery,
  plus actionable status explanations for pending removals and retained bases.
- Explicit `riftri gc` planning and `--apply` collection for zero-reference
  immutable bases, with per-base locking, reference revalidation, and durable
  collection journals.
- Read-only status diagnostics for unjournaled files, empty base buckets,
  structurally unsafe state entries, and missing active journal paths.
- Offline release-staging coverage for all six native npm packages, GitHub
  archives, executable names, and SHA-256 checksums.

### Changed

- npm distribution sources now live under `package/` without changing
  published package names or tarball layout.
- Public contribution, support, security, and release guidance now follows the
  Assistant UI organization ownership model.
- Public documentation and npm metadata identify Riftri as experimental,
  pre-release software.
- CI runs once for pull-request branches and again on `main` after merge instead
  of duplicating every pull-request run with an unrestricted push run.
- Milestone 2 now permits only explicit APFS worktree creation. Unsupported
  checkout configurations fail before mutation, and full-copy fallback remains
  disabled.
- Ordinary Git commands and disabled repositories pass directly to real Git
  inside `riftri exec`; unsupported enabled add forms fail without fallback.

### Fixed

- Concurrent worktree operations in one Riftri process now receive distinct
  journal and scratch identifiers even when the system clock returns the same
  timestamp to multiple threads.
- Enabled Git interception now fails closed instead of allowing forced removal,
  move, or prune to mutate managed Riftri lifecycle state outside its journals.

### Safety

- Failed adds roll back real Git metadata and their newly created branch when it
  has not moved.
- Recovery preserves incomplete views that are dirty or no longer match their
  immutable base.
