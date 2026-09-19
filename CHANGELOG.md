# Changelog

All notable changes will be documented here. Riftri follows Semantic Versioning
for its Rust CLI and npm distribution packages as one synchronized release.

## Unreleased

### Fixed

- Termination forwarding now enforces the single-waiter invariant its design
  relies on, and no longer loses a termination signal delivered during its
  own teardown. The forwarding state behind `riftri exec` and the Git shim is
  process-global, so two overlapping waits would each capture the other's
  handler as "previous" and restore it, leaving a handler forwarding to a
  dead PID while the single target slot signalled the wrong child; a second
  overlapping call is now refused with a clear error instead (Riftri performs
  one such wait per process lifetime, so nothing supported changes).
  Separately, a SIGTERM or SIGHUP that landed after the child was reaped but
  before the original dispositions were restored used to be recorded and then
  silently discarded; it was aimed at Riftri itself, so it is now re-raised
  once restoration completes and takes effect under the restored disposition
  — the waiter dies with the conventional `128 + signal` status exactly as a
  shell does after its foreground child, while a `nohup`-style inherited
  ignore still discards it.
- Linux OverlayFS recovery no longer resets the private work directory of a
  mount that may still be live in another mount namespace. When a crash left a
  mount without a journaled identity, the no-identity recovery branch ran the
  destructive remount loader — which cannot see mounts in other namespaces and
  `remove_dir_all`s the work directory — before the boot/namespace/liveness
  determination, so "recovery preserved it" could report a worktree whose
  overlay was already damaged (copy-up failing with ESTALE/EIO in its original
  namespace). Both recovery branches now load non-destructively, decide
  boot/namespace/liveness first, and reset disposable work state only after
  that determination proves the mount absent; the same guard now protects the
  elevated helper's work-directory reset, which receives the journaled mount
  context and refuses a reset the journaled namespace cannot rule out.

- Linux OverlayFS hygiene around live mounts: the recovery marker is no longer
  unlinked directly from the upper layer while the overlay is mounted —
  modifying an underlying layer of a live overlay is undefined per kernel
  OverlayFS rules and could leave a stale marker entry in the merged root that
  failed the add's clean check. The marker is now cleared through the merged
  view (and only when that view provably exposes this journal's marker; a
  mount that does not is reported and the marker preserved), with the direct
  upper unlink reserved for unmounted layouts. Abandoned probe mounts — the
  `.riftri-overlay-probe-*` directories deliberately leaked next to worktrees
  when a probe unmount fails — are also no longer invisible and unbounded:
  `riftri status` names each one in its diagnostics, and `riftri repair`
  removes one only when the kernel mount inventory proves nothing is mounted
  at or below it, preserving and reporting any probe root a mount still
  covers.
- Reusing a cached immutable base no longer trusts metadata its completion
  marker never covered. The v1 marker hashed contents, tree shape, symlink
  targets, and `mode & 0o777`, while the native cloners faithfully propagate
  more than that: the Linux reflink backend restores the full `st_mode`
  (setuid, setgid, and sticky bits land) and APFS `clonefile` copies mode,
  extended attributes, and ACLs verbatim. Anything that modified a cached
  base under `bases/v1` could therefore inject special permission bits or
  xattrs into every later worktree cloned from it, with Git reporting the
  new worktree clean. Completion markers now use a versioned v2 digest that
  also covers the full native Unix mode, every extended attribute name and
  value, and macOS ACL presence — hashed in the same traversal that already
  reads file contents — and a mismatch refuses reuse and preserves the base,
  exactly like content corruption. Windows continues to cover only the
  read-only attribute, matching what the ReFS cloner propagates. An existing
  base with an intact v1 marker migrates predictably: its content digest is
  still verified, then the base is rebuilt once and re-marked with v2
  instead of being trusted or silently mass-invalidated; cache keys, journal
  formats, and the persisted compaction and forced-removal snapshot digests
  are unchanged.
- Git failures now report how the process ended. A Git killed by a signal —
  an OOM kill during `checkout-index` on a big tree, a SIGSEGV — usually
  wrote nothing to stderr, so the error rendered as
  `Git command failed (checkout-index …): ` with nothing after the colon.
  The message now appends the exit disposition: the exit code when the
  process exited (`fatal: … (exit code 128)`), or on Unix the terminating
  signal (`killed by signal 9 (SIGKILL)`). Optional-result probes were also
  audited so a signal death is never misread as "absent": a killed
  `rev-parse` now surfaces as a real error instead of an unborn HEAD.
- `doctor` and repository inspection no longer report a corrupt object store
  as an unborn HEAD. `git rev-parse --verify --quiet HEAD^{commit}` exits 1
  both for a genuinely unborn repository and for a HEAD whose commit object
  is missing or unreadable; a cheap follow-up probe of the unpeeled `HEAD`
  now distinguishes them. A broken repository reports "could not peel
  HEAD^{commit}: HEAD resolves to `<object>`, but that object is unreadable;
  the repository object store may be corrupt (try `git fsck`)" in both human
  and JSON reports, while a real unborn repository keeps its friendly
  "repository HEAD is unborn; commit a tree first" guidance.

- Compacting a worktree after a checkout-profile input changed — a Git
  upgrade, a checked config flip such as `core.autocrlf` or `core.eol`, or a
  different Git LFS object set — no longer wedges the worktree's add journal.
  Compaction used to rewrite the journal's `base_path` into the newly keyed
  immutable-base bucket while `base_staging` stayed in the old one, so
  recovery validation rejected the journal forever afterwards: remove, move,
  compact, and repair all refused with "journal … contains paths outside its
  operation scope", storage accounting dropped the view, and an interruption
  between the base update and completion stranded the original tree in
  `.riftri-compact-old-<id>`. The base update now retargets `base_path` and
  `base_staging` in the same durable journal write, recovery validation
  accepts the cross-bucket staging record an older Riftri left behind in an
  Active journal (staging is confined to the immutable-base layout either
  way), repair resumes previously stuck compactions, and the next compaction
  heals the stale staging record in place. A compaction cancelled before its
  base build also removes the empty bucket it created for the new profile
  instead of leaving it as permanently unexplained state.

- One worktree whose HEAD file cannot be resolved (empty, garbage, or an empty
  symref target — classic crash and power-loss shapes) no longer makes every
  Riftri command in the repository fail with "invalid Git output". Git lists
  such a worktree with a null `HEAD` and none of `branch`, `detached`, or
  `bare`; the porcelain parser now represents that state instead of rejecting
  it, while still rejecting records that claim more than one of the three.
  Unrelated operations — add, remove, move, prune, gc, status, list, and the
  intercepted shim path — keep working; `status`, `worktree list`, and
  `repair` name the corrupt worktree in a diagnostic that points at
  `git worktree repair`; and removing, moving, or shell-binding the corrupt
  worktree itself fails closed with the same guidance instead of risking work
  in a worktree whose cleanliness cannot be verified.
- The npm launcher now mirrors the termination contract of native `riftri
  exec`. A native process killed by a signal Node ignores or reserves
  (SIGUSR1, SIGPIPE, ...) previously made the launcher exit 0 — a killed run
  reported success — because the death was re-raised through `process.kill`,
  which is a silent no-op for those signals; the launcher now computes
  `128 + signal` numerically for every signal death. While the native process
  runs, the launcher also stays alive through Ctrl-C (SIGINT) and Ctrl-\
  (SIGQUIT), which the terminal delivers to the whole foreground process
  group, so a command that catches the interrupt keeps its wrapper instead of
  outliving a dead launcher on the terminal; PID-directed SIGTERM and SIGHUP
  are forwarded to the native process, and the child's exit code propagates
  unchanged.
- Intercepted `git worktree prune -v` no longer bypasses the journaled prune.
  Git's `-v` is a verbose prune, not a report, so delegating it let ordinary
  Git remove managed lifecycle metadata outside the Riftri journal; verbose
  prunes now take the same journaled path as a bare prune. Dry runs remain
  delegated, including with `--expire`, and a dry run beside an unrecognized
  option stays refused.
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
- `riftri repair` now reconciles active add journals against Git's own worktree
  registry. A journal whose worktree Git no longer registers and whose
  directory is gone is retired by a journaled completion, so a worktree deleted
  by hand no longer leaves a path claimed forever. `riftri worktree add`
  reclaims such a journal instead of creating a second active claim on one
  path, which previously left `riftri worktree remove` and `riftri repair`
  failing permanently with "multiple active Riftri journals reference". Nothing
  is retired while its directory still exists or still has content.
- `riftri repair` reports a managed worktree that Git registers under a
  different path — the result of `mv` plus `git worktree repair` — instead of
  silently dropping it from `riftri status` and `riftri worktree list`. The
  journal is preserved and the live worktree is never touched.
- `riftri repair` can now retire the journal of an add that lost a race for a
  destination another active operation owns. Those journals were stuck in
  `rollback-pending` and made every later repair exit non-zero while
  `riftri status` reported no issue. The winner's worktree, branch and
  immutable base are untouched.
- `riftri gc` now accounts for retained bases it refuses to collect, naming the
  base, the journal that claims it and why, in both the human and JSON reports.
  `riftri status` reports the same explanation for a base no active worktree
  references, so the two commands no longer disagree about unreclaimable
  storage.
- `riftri repair` removes the temporary files an interrupted journal write
  leaves in Riftri's own state directory, and `riftri status` no longer reports
  Riftri's own temporaries and coordination locks as unrecognized foreign
  files. One crash no longer breaks an automation gate on
  `diagnostic_issues == []` permanently.
- The PowerShell installer's HTTPS-downgrade check now actually runs for the
  `SHA256SUMS` and archive downloads. `Invoke-WebRequest -OutFile` returns
  nothing to the pipeline, so the assertion always received `$null` and
  returned without inspecting anything; downloads now pass `-PassThru`
  (supported alongside `-OutFile` on Windows PowerShell 5.1 and PowerShell 7+),
  and the check fails closed when no final URI is observable instead of
  silently skipping. The installer test mock now matches the real cmdlet's
  contract — no pipeline output with `-OutFile` unless `-PassThru` — and a
  regression test proves the installer refuses a download whose final URI is
  not HTTPS.

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
