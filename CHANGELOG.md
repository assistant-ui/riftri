# Changelog

All notable changes will be documented here. Riftri follows Semantic Versioning
for its Rust CLI and npm distribution packages as one synchronized release.

## Unreleased

## [0.6.1] - 2026-10-06

### Changed

- Reduce temporary allocations and repeated Git work during attribute checks,
  sparse selection, Git LFS reads, integrity hashing, and journal diagnostics.
  Storage accounting balances work across bounded workers, and single-worker
  native clones run inline without a worker thread.
- The Node SDK drains stdout without retaining it for non-reporting commands.
  JSON reports and stderr diagnostics retain their existing behavior (#659).

### Fixed

- The Node SDK preserves UTF-8 across output chunks, rejects missing JSON
  reports, and rejects malformed error messages without crashing the calling
  process. Type declarations now match status operation counters and all-state
  inventory reports (#650, #651, #652, #657, #658).
- Native directory traversal and storage accounting bound open directory
  handles, allowing deep trees to work under low descriptor limits. Parallel
  cloning preserves the caller's process umask (#645, #646, #647).
- File comparisons handle short reads correctly instead of treating equal
  content as different (#649).
- A file rewritten while `riftri worktree compact` hashes it is reported as
  the worktree changing during compaction, not as an I/O failure (#624).
- Committing or switching in a worktree before its `riftri worktree add`
  finished no longer leaves the add pending forever. If the view was complete,
  the add is rolled back but the worktree is kept as a plain Git worktree that
  Riftri does not manage. The add and `riftri repair` both say so; `repair
  --json` lists such worktrees under `released_adds`. A branch the add created
  that has since moved is never deleted; before, the only way out was deleting
  it (#622).
- Running out of disk space while Git builds a base, for example during
  `riftri worktree compact`, is reported as `storage-full`, not `git-failed`
  (#618).
- Removing a worktree that holds a FIFO or a Unix socket, such as a dev
  server's socket, works as `git worktree remove` does. A clean removal used to
  unregister the worktree and then fail to delete it, leaving a removal every
  `riftri repair` refused, and `--force` refused such a worktree outright
  (#616).
- `riftri worktree remove --force` no longer deletes a file written into the
  worktree just before its view is moved aside for deletion. The forced
  snapshot is now checked again at the quarantine, as a clean removal already
  is (#609).
- A removal refused before anything was removed is now `cancelled` and no
  longer left pending. Before, every `riftri repair` refused it, other
  lifecycle commands were blocked, and undoing the change let `repair` remove
  the worktree after all. `status` and `repair` report `cancelled_removals`
  (#609).
- A Git command run inside a worktree while `riftri worktree compact` swaps
  views (a commit, a switch) no longer leaves the worktree out of sync with
  HEAD. That command updates the old view, so the compaction is now undone and
  the worktree stays exactly as Git left it. A concurrent Git command holding
  the index lock, or deleting a file mid-walk, is reported as a change, not as
  a Git or I/O failure that left the compaction pending (#611).

## [0.6.0] - 2026-10-02

### Added

- `riftri gc --apply` retires finished journal history: the journals of
  worktrees that are gone for good, and finished prune and collection
  journals. A plan reports `retirable_journals`; an applied run reports
  `retired_journals`. Completed journals used to accumulate forever, and every
  command reads them all: 300 cycles left 1,103 state files, and one
  `gc --apply` reduced them to 2, taking `status` from 54 ms to 7 ms. Live
  worktrees and unfinished operations keep their journals, and an interrupted
  retirement is explained and finished by the next run (D043, #536).

### Changed

- Windows on ARM64 installs through npm like every other platform:
  `riftri-win32-arm64@0.5.1` is published now that npm lifted its name block.
  The launcher no longer sends that platform to the PowerShell installer, and
  the release's publish step treats any refused native package as fatal.

### Fixed

- A file written into the destination while `riftri worktree add` is still
  running no longer leaves the add pending with no way forward. Rollback still
  keeps the file, but the error from the add and from `riftri repair` now names
  the kept destination. It says to copy anything needed out of it, delete it,
  and run `riftri repair`, which then finishes the rollback (#599).
- A write into a worktree during `riftri worktree compact` no longer wedges it.
  If the write lands before the swap, the compaction is cancelled and only its
  replacement is removed. If it reaches the old view after the swap, that view
  is still kept, but the failure is reported as recovery to finish
  (`recovery-pending`, exit 1) rather than a policy refusal. The message names
  the kept view and says to copy what is needed back, delete it, and run
  `riftri repair` (#595).
- `--detach :/<text>` (Git's commit-message search) now resolves as it does in
  Git instead of failing as `git-failed`, and `<rev>:<path>` naming a file is
  refused as an invalid revision rather than reported as a Git failure. The
  appended `^{commit}` had become part of the search text or the path (#586).
- An add killed while it starts or builds a base no longer leaves a permanent
  `status` diagnostic. The repository base bucket is created only after the
  add's intent is journaled, so a kill between the two no longer strands an
  empty bucket with no owner. Rollback also removes the lock Git leaves beside
  the add's temporary index (#591).
- Adding a branch that another worktree already has checked out is refused
  before anything is written, as an `invalid-request` policy failure naming
  that worktree. It used to reach Git after the add was journaled and fail as
  an operational `git-failed` with unknown cleanup (#588).
- Node client: `worktree.list({ allStates: true })` no longer fails as a
  usage error on a client built with `stateDir`; a relative `binary` is
  resolved against the caller's directory, as `RIFTRI_BINARY` is, not against
  `repository`; and the `FailureReceipt` types admit the `usage` category and
  null `operation` that usage-error receipts carry (#584).
- Branch and revision lookups are exact again. A ref that merely ends in
  `refs/heads/<name>`, such as `refs/remotes/origin/refs/heads/<name>`, no
  longer makes `-b <name>` refuse as an existing branch or an existing-branch
  add fail. A range (`A..B`, `A...B`) or negation (`^A`) is refused as an
  invalid revision, as Git refuses it, instead of creating a worktree at a
  commit from the range (#586).
- The `post-checkout` hook now runs as Git runs it, with its stdout sent to
  stderr and no stdin. A hook that printed anything corrupted the `--json`
  document of `riftri worktree add`, so the Node client rejected an add that
  had succeeded. The Node client also reads a failure receipt from the last
  line of stderr, so output written there first no longer hides it (#579).
- The Node client passes paths after `--` and option values as `--name=value`,
  so a relative path such as `-scratch`, or a value starting with `-`, is no
  longer parsed as flags and rejected as a usage error (#581).
- Without a terminal, the npm launcher (`npx riftri`) now forwards SIGINT and
  SIGQUIT to the native binary, as native `riftri exec` does, and runs it in
  its own process group so a group-directed signal arrives once. It ignored
  both, so a harness that interrupted it by PID got no response while the
  command kept running (#557).
- `riftri repair` no longer resumes a removal or move another process is
  running (it now takes the worktree's add lock, as for compactions), no longer
  records a second removal for a worktree that was just removed, and skips an
  add journal `gc --apply` retired meanwhile. `gc --apply` re-reads a lineage
  under its lock before retiring it. Together these races could leave a
  removal journal with no add journal, after which every lifecycle command in
  the state directory failed (#550).
- Commands no longer fail because another Riftri command is running. `status`
  and `worktree list` exited 1 when a view vanished mid-scan (removed, moved, or
  swapped by a compaction), and lifecycle commands failed when a journal they
  had just listed was retired by `gc --apply` or replaced by its owner. A failed
  move was then left pending until `riftri repair`. Vanished paths now count as
  nothing, and journal reads retry an in-flight replace and skip a retired
  journal (#544).
- Listing Git's worktrees is retried briefly: `git worktree list` exits 128
  when a concurrent removal deletes a worktree's metadata mid-listing, which
  made unrelated Riftri commands fail at random (#546).
- `riftri worktree add` refuses a destination inside the repository's
  `.git/worktrees` directory before recording anything. A worktree there is
  its own Git metadata directory; the add used to fail as `rollback-failed`
  and stay pending with nothing `riftri repair` could roll back (#542).
- `riftri setup` without a terminal now exits 3 (policy refusal) instead of 1,
  matching its `--json-errors` refusal: nothing was attempted and retrying
  cannot help (#538).
- `riftri worktree remove`, `move`, and `compact` without `--state-dir` now
  name the registered state directory that manages the worktree, and the
  `--state-dir` to pass, instead of only saying it is not managed in the
  default location (#540).
- `riftri status`, `riftri worktree list`, and `riftri repair` list Git's
  worktrees once per repository instead of once per managed worktree. They
  slowed quadratically: at 120 worktrees, `status` took 5.4 s and now takes
  0.39 s, and `repair` went from 3.7 s to 0.12 s (#531).
- `riftri doctor --destination` now agrees with `riftri worktree add`. It
  blocks a non-empty directory, a symbolic link, or a destination whose nearest
  existing directory is not writable, all of which it used to report as ready,
  and no longer blocks an add from a cone-mode sparse worktree, which inherits
  that cone (#529).
- In an enabled repository without commits, an intercepted
  `git worktree add -b <branch>` now runs as ordinary Git, which creates an
  orphan worktree, instead of being refused. There is no tree to clone, so
  Riftri hands such adds to Git unchanged (#527).

## [0.5.1] - 2026-09-29

### Added

- A cached-creation baseline on a real repository:
  `docs/benchmarks/assistant-ui-cached-creation-2026-09-25.md` records serial and
  ten-way concurrent latencies against an exact-tree assistant-ui export,
  reports riftri's allocation accounting separately from whole-volume deltas, and
  retains its outliers. It also records that the unmodified repository is now
  accepted, which the 2026-09-12 experiment could not certify.
- A cached-creation baseline benchmark records serial and ten-way concurrent
  creation latencies against one warm base, for #211. CI uploads it as
  `cached-creation-baseline-macos-apfs`.
- A sparse monorepo benchmark measures allocation and creation costs for a cone
  against the full tree, closing Milestone 6's outstanding measurement for
  sparse-checkout profiles. CI records it on a disposable APFS volume and
  uploads it as `sparse-monorepo-benchmark-macos-apfs`.

### Fixed

- `riftri exec --worktree` naming anything but a registered worktree root is
  now an `invalid-worktree-binding` policy refusal (exit 3) instead of an
  operational `command-failed` receipt that invited a retry (#523).
- Without a terminal, `riftri exec` and the Git shim now forward SIGQUIT to
  their child like SIGINT. A PID-directed SIGQUIT used to kill the wrapper and
  leave the scoped command, or the real Git, running as an orphan (#520).
- An add refused because a base failed its integrity check now names the
  worktrees still using that base and says to remove them and run
  `riftri gc --apply`. `riftri status` marks such a base as damaged, in text
  and as `damaged` in JSON, until it is collected (#512).
- `riftri worktree compact` on a worktree that differs from its commit in a
  way Git does not report (a modified file marked `assume-unchanged` or
  `skip-worktree`, or an extended attribute) now cancels itself and says so.
  It used to leave the compaction pending, with its replacement directory, so
  every other lifecycle command refused until `riftri repair` (#515).
- A `--state-dir` that runs through a regular file now gets the same
  `invalid-state-directory` policy receipt as one that is a file, instead of an
  operational failure that invited a retry (#510).
- `riftri worktree remove` without `--force` refuses a worktree that contains
  submodules, as `git worktree remove` does. It used to remove it together
  with the submodule repositories in the worktree's Git directory, because Git
  skips that check once Riftri has moved the view aside (#507).
- A `riftri worktree move` that Git refuses, for example for a worktree that
  contains submodules, no longer leaves the worktree stuck behind a pending
  move that `riftri repair` could never finish. When both paths show nothing
  moved, the move is recorded as cancelled and the command reports Git's
  reason; `repair` cancels journals already stuck this way. Moving a worktree
  into itself is refused before anything is recorded. `status` and `repair`
  report `cancelled_moves` (#505).
- `riftri worktree add <path> <branch>` checks out the branch even when a tag
  has the same name, as Git does. It used to resolve the tag and refuse with a
  false "existing branch moved". A tree or blob ID given as the revision is now
  the same `invalid-request` refusal as an unknown name, instead of an
  operational Git failure (#503).
- Concurrent adds sharing a new state directory no longer fail at random with
  "worktree parent path contains a dangling symbolic link" when another add
  creates that directory between two probes (#498).
- A refused or rolled-back `riftri worktree add` no longer leaves an empty
  immutable-base bucket that `riftri status` reported as unexplained forever.
  The bucket is created only once the add records its intent, and rollback
  removes it when it is empty (#500).
- `riftri worktree add` no longer aborts with a stack overflow on a tree more
  than about 120 directories deep. The base integrity hash kept a 64 KiB read
  buffer in every recursion frame. The hashed bytes are unchanged, so existing
  bases still verify (#494).
- A tree path that fits in Git's checkout but not under Riftri's base or
  staging locations within the platform path limit is refused up front,
  naming the path and suggesting a shorter `--state-dir`. It used to fail deep
  inside the base build as an operational `git-failed` error (#496).
- On macOS, a worktree whose path uses decomposed Unicode (NFD), as Finder and
  many apps write names, stays manageable. Git registers such paths
  precomposed, and the byte comparison made `status` report the worktree as
  unregistered and made `move` and `remove` refuse it. Paths now compare by
  their composed form on macOS, where APFS treats both spellings as the same
  file (#489).
- `riftri worktree add --state-dir` accepts a path with `.` components or
  trailing separators, which failed as an I/O error. It refuses, before
  creating anything, a state path that climbs out of a directory that does not
  exist yet (previously left behind as litter), and a state directory inside
  the new worktree or the reverse (previously reported as "taken by another
  worktree" with the directories left behind) (#491).
- `riftri repair` re-homes add journals that an older Riftri wrote while the
  add ran inside a linked worktree, once that worktree has been removed. It
  points them at the repository's main worktree, so moving, compacting, and
  removing their worktrees works again. A journal is re-homed only while its
  destination is still a live worktree of the repository that owns the state
  directory (#476 follow-up).
- Worktree lifecycle commands, `status`, `gc`, `repair`, and `doctor` work
  when run from a bare repository (`git clone --bare`, then `git worktree add`
  from the bare directory). They refused there, although the same repository
  already worked from its linked worktrees. `riftri enable` still requires a
  working tree (D042, #485).
- `riftri worktree remove` works when run from inside the worktree being
  removed, or one of its subdirectories, as `git worktree remove` does.
  Previously it failed with `cannot run Git in <worktree>` and left a pending
  removal (#475).
- A worktree created from inside another linked worktree stays manageable after
  that worktree is removed. Journals now record the repository's main worktree
  instead of whichever worktree ran the command. Previously `move` left a
  pending move, `compact` and `remove` refused, and `status` called the journal
  unsafe (#476).
- `riftri worktree add` from an orphaned branch now says `HEAD is on branch
  <name>, which has no commits yet` instead of claiming that the whole
  repository has no commits (#479).
- A journal read that races its owner's atomic replacement is retried instead
  of being reported as an invalid journal. Concurrent `riftri worktree add`
  calls occasionally refused with "unsafe durable add journal: journal path
  changed" (#481).
- Concurrent `riftri worktree add` calls for the same path can no longer
  delete the one that won. Each loser's rollback used to take the winner's
  registration for its own and remove it, even after the winner had reported
  success. With distinct branches, every loser was instead left with a pending
  add and a leaked branch. The claim check through Git's registration now runs
  under the worktree-metadata lock: one add wins, and the others are refused
  cleanly before recording anything. An interrupted add also keeps claiming its
  path until `riftri repair` resolves it (#469).
- `riftri worktree remove` (with or without `--force`) and `riftri worktree
  move` now refuse a worktree locked with `git worktree lock` before recording
  anything, naming the lock reason and `git worktree unlock`. Previously Git
  refused only after Riftri had journaled its intent, and the pending journal
  blocked every later command on that worktree until `riftri repair` (#460).
- `riftri worktree add` now refuses, before recording anything, a path Git
  still registers as a worktree whose directory was deleted without
  `git worktree prune`, and says how to clear it. Previously Git refused only
  after the add was journaled, and rollback mistook the stale registration for
  its own, leaving a pending add until `riftri repair` (#461).
- Running Git in a directory that no longer exists, such as a repository moved
  after its worktrees were created, now reports `cannot run Git in <path>: the
  directory does not exist`. Previously it read `could not start Git command
  "git": No such file or directory`, which pointed at the Git installation
  (#465).
- `riftri status` no longer calls the journal of a managed worktree that was
  removed or moved with plain `git worktree remove`/`move` an "unsafe durable
  add journal". It now says the worktree was removed or moved outside Riftri and
  names what clears it: `riftri repair` for a removal, and `git worktree move`
  back (or `git worktree remove`, then `riftri repair`) for a move (#467).
- `riftri repair` also reaps the intent a removal, move, or compaction was
  writing when it was killed before publishing it. Such an operation never
  started, but its complete temporary record was previously preserved forever.
  Repair removes it once it holds the lock of the add the intent names; torn
  records stay preserved (#457).
- A compaction killed while it was deleting the old view no longer wedges
  `riftri repair` with a false "worktree content changed during compaction".
  The verified old view is renamed to a journal-derived
  `.riftri-compact-drop-<operation>` name before deletion, and repair finishes
  deleting it (#455).
- `riftri repair` now reaps the journal temporary left by a lifecycle command
  that was killed mid-write once that operation has finished, instead of
  preserving it forever. One killed `worktree move`, `remove`, or `compact`
  previously kept `riftri status` from ever reporting a clean state directory.
  Temporaries of unknown or still-pending operations stay preserved (#449).
- A Git index refresh blocked by `index.lock` now reports the lock path and how
  to clear it, instead of `Git command failed (update-index -q --refresh): exit
  code 128`. `-q` silences Git's own explanation, so a lock left by a killed
  compaction wedged `riftri repair` with no hint (#448).
- A worktree removal killed while its files were being deleted no longer
  wedges `riftri repair`. Native removals now rename the view to a journal-owned
  `.riftri-remove-<operation>` sibling, let Git unregister the missing path, and
  only then delete the quarantine. Repair finishes from any interruption point
  instead of refusing a half-deleted tree as "has changes" (#447).
- `riftri repair` recovers an add that was killed while its own internal
  `git worktree add` was running. Git registers a new worktree locked as
  "initializing" and unlocks it only when it finishes, so the interrupted
  registration stayed locked, `git worktree remove` refused it, and every later
  `repair` failed for that state directory. Rollback now removes that lock when
  the journal proves it is the operation's own in-progress marker — never a
  lock on a worktree Git finished registering — and deletes the branch the
  interrupted call created, which previously leaked.
- An add killed while its new immutable base was being finished no longer
  leaks that base. The base is moved into place before its integrity digest
  and completion marker are written, so a kill in between left a full,
  unmarked copy of the tree that neither `repair` nor `gc --apply` would
  remove, and `status` reported it on every run. Rollback now removes an
  unmarked base named by the rolled-back add's own journal, under the base's
  exclusive lock, and only when no other journal still claims it; a marked
  base, or one another process holds, is kept.
- `worktree add` accepts an existing empty directory as its destination, as
  `git worktree add` does, so `dir=$(mktemp -d); git worktree add "$dir" …`
  works under interception instead of failing with "destination already
  exists". A directory with content, or a symbolic link, is still refused. If
  such an add fails before Git has used the directory, rollback leaves it in
  place and completes rather than stopping on an unregistered destination,
  which would have left the add journal pending for every later `repair`.
- `git worktree prune` after a managed worktree was deleted outside Riftri —
  the standard `rm -rf <worktree> && git worktree prune` cleanup — now says
  what to do. Prune still never touches a managed worktree, but its refusal
  ("missing or not registered") named no remedy; it is now a `recovery-pending`
  refusal whose message and `nextCommand` name `riftri repair`, which retires
  the stale journal so prune can run. A managed worktree whose Git
  registration was deleted by hand is refused with its own message and no
  suggested command, since repair does not resolve it.
- A new branch created from a remote-tracking start point now tracks it, as
  with Git. `git worktree add -b task <path> origin/main` succeeded under
  Riftri with no upstream, because the add created the branch from the
  resolved commit rather than the name, so a later `git push` or `git pull`
  behaved differently without any warning. Git now makes the tracking decision
  from the name; if the start point moves during the add, the new branch is
  pinned back to the commit resolved beforehand, as it was before.
- Intercepted `git worktree add` accepts Git's standard option syntax for the
  options it supports: the short forms `-q` and `-d`, bundled short options
  such as `-qb <branch>` and `-qd`, a value stuck to its option as in
  `-b<branch>`, `--no-quiet`, and the explicit defaults `--no-lock`,
  `--no-force`, `--no-orphan`, and `--no-guess-remote`. Each was refused as
  unsupported, so `git worktree add -q …` failed under interception.
  `--no-track` is still refused, because it would change how Git sets up the
  new branch.
- Two caller mistakes get their own policy codes (exit 3, `cleanup:
  not-needed`, no `nextCommand`) instead of operational failures (#425).
  `bare-repository`: `status`, `gc`, `repair`, and `worktree list` in a bare
  repository were `command-failed`, and `gc` and `repair` suggested a
  `riftri status` that failed the same way. `invalid-state-directory`: a
  `--state-dir` that is a regular file or a symbolic link made `status`, `gc`,
  and `repair` report `journal-failed` with `recovery: required`, sending a
  harness back and forth between `repair` and `status` on the same file, while
  `worktree add` blamed the volume probe or reported "File exists".
- A repository path that does not exist or names a file is reported as
  `not-a-repository` (policy, exit 3) naming the path, instead of
  `could not start Git command "git"`. The operating system reports a bad
  working directory as the program being missing, so a mistyped
  `--repository` looked like a broken Git installation. Git genuinely missing
  from `PATH` is still reported as before.
- `worktree add` refuses three requests that cannot succeed as written — a
  `-b` branch name that already exists, a revision that names no commit, and
  `HEAD` in a repository with no commits — as `invalid-request` policy
  failures (exit 3) before writing anything. They were reported as Git command
  failures, operational with unknown cleanup, so an agent re-running a task
  with the same branch name was told to retry. A revision whose object is
  unreadable in a damaged repository stays an operational failure.
- A managed worktree deleted outside Riftri — commonly with `rm -rf` — now
  points at the fix. `worktree remove`, `move`, and `compact` on it reported
  an operational `filesystem-io-failed` and suggested `riftri status`, while
  its still-active add journal kept the immutable base in use so `gc` could
  never reclaim it. They now refuse with `recovery-pending` and a
  `nextCommand` of `riftri repair`, which retires the journal and frees the
  base. A path that was never managed is a plain policy refusal.
- `worktree add` creates missing leading directories of its destination, as
  `git worktree add` does, so enabling Riftri no longer makes an ordinary
  `git worktree add .worktrees/task-1 -b task-1` fail when `.worktrees` does
  not exist yet (#423). Directories are created only after the request passes
  validation, so a refused add creates nothing; a later failure rolls back the
  worktree and leaves the directories, as Git does. `riftri doctor` no longer
  reports a missing parent as a blocker, and a path Git could not create
  either — an existing ancestor that is a file, or a dangling symbolic link —
  is an `invalid-request` policy refusal instead of `filesystem-io-failed`.
  `worktree move` still requires the parent to exist, like `git worktree move`.
- Running a command outside a Git repository is reported as a policy refusal —
  `not-a-repository`, exit code `3` — instead of an operational failure with an
  unknown cleanup. The cause reached the receipt by two routes that classified
  it differently: the worktree commands wrapped it in a `WorktreeError` and
  reported `git-failed`, while `status`, `gc`, and `repair` wrapped it in an
  `ActivationError` that never reached the mapping at all and reported
  `command-failed` with `recovery: unknown`. Both now resolve through one
  predicate, and the exit code agrees with the receipt. A Git failure inside a
  repository Git accepts, such as a damaged object store, stays operational.

## [0.5.0] - 2026-09-25

### Added

- Riftri runs the repository's `post-checkout` hook after creating a worktree,
  on Git's own contract: the null object id, the new HEAD, and `1` for a branch
  checkout, executed from inside the new worktree. `core.hooksPath` is honored
  and resolved where Git resolves it. A hook is no longer a reason to refuse a
  repository, which previously excluded every repository using a hook manager:
  husky sets `core.hooksPath` and generates a `post-checkout` entry whether or
  not the project defines one. A failing hook is reported through the exit code
  and leaves the worktree in place, exactly as `git worktree add` does, so a
  hook cannot roll back a worktree Git would have kept. The outcome appears on
  the add result, in human output, and in `--json`. (#402)

### Changed

- The checkout profile that keys immutable bases no longer includes
  `core.sparseCheckout` and `core.sparseCheckoutCone`. Both are worktree-scoped,
  so the same cone read from the repository root and from inside a sparse
  worktree produced different base keys, splitting one profile across several
  base buckets. The canonical cone list already describes the materialization
  exactly. The profile version moves to `v4`. Existing worktrees keep working
  and keep referencing the bases they were built from; new adds build a `v4`
  base once per tree and profile. Superseded bases become unreferenced only
  when the worktrees using them are removed, and `riftri gc --apply` reclaims
  them then, so nothing accumulates silently and nothing needs removing by
  hand.

### Added

- `riftri worktree compact` supports sparse worktrees. A pristine view rebuilds
  at its creation profile and resolves to the base it already had, instead of
  being refused outright with no way to reclaim its storage. A view whose
  selection changed since the add is refused with both profiles named, since
  Riftri never rewrites a worktree's immutable creation base.
### Fixed

- The npm launcher now installs its Unix signal handlers before starting the
  native process. A fast child can no longer announce readiness while the
  launcher still has the default fatal `SIGINT` disposition, and the Linux
  foreground-group regression is enabled again.
- Unix launcher-signal tests now own and clean up an isolated process group, so
  a native `riftri` process that outlives a timed-out launcher cannot keep the
  test worker's output pipe open and stall Ubuntu CI until the job-level limit.
- A missing repository-registered state directory now produces an actionable
  policy refusal instead of an operational filesystem error. The failure
  receipt identifies the stale directory, reports that no cleanup is needed,
  and provides the exact non-destructive `riftri state unregister` command;
  running that command clears the blocker for the next add.
- Disk-full failures now report the stable `storage-full` receipt code on
  macOS, Linux, and Windows, including when both an operation and its rollback
  fail. Combined failures retain their typed causes instead of stringifying
  them, so automation can ask an operator to free space before following the
  unchanged lifecycle recovery command without matching localized error text.

- `riftri worktree remove`, `worktree move`, and `worktree compact` report an
  unmanaged worktree as a policy error whether or not the repository has a
  state directory yet. They previously surfaced the absent directory as
  `filesystem-io-failed` with a recovery of `inspect` and an unknown `cleanup`,
  so the same request was classified two different ways depending on whether an
  unrelated managed add had ever run. None of them creates a state directory in
  order to fail.

- Optimized adds now resolve `core.hooksPath` with Git's pathname rules before
  creating lifecycle state. Values such as `~/hooks` and
  `%(prefix)/share/git-core/hooks` therefore run the same `post-checkout` hook
  as `git worktree add`; previously Riftri treated those forms literally and
  silently skipped the hook.

- `riftri worktree prune` no longer fails on an enabled repository that has no
  state directory yet, which is every repository until its first managed add.
  It reported `filesystem-io-failed` with a recovery of `inspect`, telling a
  harness to investigate a repository that `status`, `doctor`, `gc`, `repair`,
  and `worktree list` all reported as healthy at the same moment. Prune now
  creates its state layout the way an add does, so the Git prune it exists to
  perform still runs: a stale registration left by an ordinary Git worktree is
  removed even when Riftri has never managed one.

## [0.4.1] - 2026-09-24

### Fixed

- An optimized add from inside a cone-mode sparse worktree now inherits that
  worktree's cone instead of being refused. `git worktree add` copies the
  current worktree's sparse selection into the new worktree, so Riftri
  reproduces that and keys the immutable base by the inherited profile.
  Previously a Riftri sparse worktree was a dead end: Riftri set
  `core.sparseCheckout` in the worktree it created, and every later add from
  inside it was rejected for configuration Riftri itself had written. Sparse
  checkout that is enabled outside cone mode is still refused.

- Doctor's suggested add and activation commands keep the inspected repository
  when doctor runs from a different directory. Repository and destination paths
  use shell quotes to preserve spaces and shell metacharacters.
  Non-UTF-8 repository paths retain the `riftri enable` suggestion for use
  inside that repository.

- Linux GNU builds no longer require a glibc newer than the distributions they
  claim to support. `v0.4.0` needed `GLIBC_2.39`, so it installed cleanly on
  RHEL 9, Debian 12, and Ubuntu 22.04 and then failed to start. GNU artifacts
  now target glibc 2.34, release CI verifies each artifact's ELF requirements
  and runs it on a glibc 2.34 image, and `install.sh` falls back to the static
  musl archive on an older host.

- A Git alias that resolves to a worktree command is matched with Git's own
  grammar, including backslash escapes and chained aliases. Previously
  `alias.wtrm = 'worktree remo\ve --force'` removed a managed worktree outside
  Riftri's journal, and its immutable base stayed referenced forever, so
  `riftri gc` could never reclaim it. An alias Riftri cannot resolve, such as
  one supplied with `git -c`, now fails closed rather than bypassing the
  journal.

- `riftri exec` no longer loses the scoped command's exit status when an
  interrupt arrives as the command finishes. The launcher released its SIGINT
  handler before exiting, which restored the signal's default disposition, so
  an in-flight Ctrl-C killed the launcher instead of propagating the command's
  code.

- The npm package's programmatic API no longer sends Riftri's own flags to the
  child of `exec --`, crashes the host process on malformed output, reports a
  signalled run as exit code `null`, or hides a broken installation behind
  `isOptimizable() === false`.

- `isOptimizable()` answers on the destination rather than the volume. It read
  `cow_backend_active`, which stays true outside a Git repository, so the
  documented check-then-add pattern took the optimized path into a failure.
  A destination Riftri reports as `blocked` is no longer optimizable;
  `needs-activation` still is, because the explicit interface works without
  `enable`.

- Conditional checkout configuration (`includeIf`) is rejected rather than
  producing a worktree whose contents depend on where Git was run from.

- A repository Riftri cannot inspect fails closed instead of proceeding.

### Changed

- `doctor` resolves HEAD's tree only when it needs it, so inspection runs
  fewer Git subprocesses.

## [0.4.0] - 2026-09-22

### Added

- The npm package ships a programmatic API alongside the CLI launcher, so
  Node.js tooling can drive Riftri without shelling out to the binary by hand.
  (#344)
- New guides: what survives when you stop using Riftri (#334), building a
  custom harness on Riftri with runnable examples (#327, #335), and expanded
  page substance across the documentation (#338).

### Changed

- `riftri enable` now pins `gc.worktreePruneExpire=never` in the repository's
  local Git configuration, and `riftri disable` removes it again when the value
  is still the one Riftri wrote. `git gc` runs `git worktree prune` internally
  and `gc.auto` fires it from ordinary commands, resolving `git` from Git's own
  exec-path — those inner calls never reach the proxy, so Git could drop a
  managed worktree's registration outside the journal, stranding its add
  journal and pinning its retained base. With the expiry pinned, Git itself
  declines. Plain `git worktree prune` becomes a no-op in enabled repositories;
  the journaled `riftri worktree prune` remains the supported path. An expiry
  the user already set is never overwritten. (#324)
- A failed `riftri repair` no longer answers with `nextCommand: riftri repair`
  — the command that just failed — which sent any harness following the receipt
  into a retry loop. It now points at `riftri status`, which isolates and names
  the state needing attention. Every other operation still routes to `repair`.
  (#332)
- Creating a worktree runs fewer Git subprocesses (17 → 15): the repository
  cleanliness probe now runs only for `riftri doctor`, the one command that
  reports it, instead of on every lifecycle operation (#340), and the
  repository-identity questions share one `git rev-parse` invocation (#348).

### Fixed

- `worktree remove`, `force-remove`, and `move` now claim the same
  per-worktree operation lock that `compact` already took, and every claimant
  re-validates under the lock. Previously cross-operation exclusion rested on a
  journal scan performed before the competing operation published its own
  journal — a wide race window in which, for example, a `remove` could delete
  the worktree a `compact` was still hashing. The loser then failed into a
  journal shape no recovery branch could classify: `repair` errored on every
  run, every later `prune` was refused, and both the old and new base stayed
  pinned against `gc`, permanently. The loser of the race now receives a clean,
  retryable "busy" refusal instead. No user data was at risk in either
  ordering. (#323)
- A `git` config alias that resolves to a worktree command (for example
  `alias.wtp = worktree prune -v`) is now expanded and planned as the command
  Git will actually run, so it takes the journaled path instead of bypassing
  classification entirely. Shell aliases (`!command`) are deliberately left
  alone: they re-enter the proxy on their own. (#326)
- An unrecognized Git global option no longer bypasses the managed-worktree
  guard. `git --attr-source=HEAD worktree remove <managed>` used to reach real
  Git unclassified because the option parser failed open; unknown global
  options are now treated as significant, matching how a recognized
  `-c key=value` already behaved, and both the attached and separated option
  spellings are covered. Ordinary commands, read-only subcommands, unmanaged
  worktrees, and disabled repositories still delegate to Git unchanged. (#325)
- An add journal whose worktree directory vanished (an external `rm -rf`, a
  cleared `/tmp`) is retired again. Git keeps such a registration and marks it
  `prunable` rather than dropping it; Riftri treated any registration as live,
  so `repair` reported nothing to do while `status` flagged the state forever
  and `gc` could never reclaim the base — permanent once #324 stopped Git's
  own expiry from clearing the entry. A `prunable` registration whose
  destination is gone now counts as vanished at both sites that assumed
  otherwise. (#337)
- `riftri shell deactivate` no longer strips an unrelated `PATH` entry that
  merely embeds the shim prefix in a parent segment (for example
  `/opt/riftri-git-shim-tools/bin`). The process-shim sweep now matches on
  each entry's final path component, which still removes stacked shims left by
  nested `riftri exec` sessions. (#322)
- The `PATH` walk that records the real Git for `riftri exec` and the shell
  hooks can no longer select a shim or the running Riftri executable itself.
  A half-deactivated shell — durable shim still on `PATH`, marker variable
  cleared — used to bake Riftri in as the real Git, after which every Git
  command re-entered the shim forever. The walk now skips shim directories
  (reading their baked real-Git marker instead) and never selects the current
  executable, matching the hardened stripped-environment resolver. (#328)
- `riftri status` no longer describes a base it cannot verify as an unused
  cache. Reference counts are derived from the journals that parsed; when some
  cannot be read, the count is a lower bound, and the previous output
  (`refs=0`, `in_use: false`, "retained cache; no active views") invited
  deleting storage that live worktrees still depended on. The JSON now carries
  `counts_complete` and a per-base `reference_count_complete`, `in_use` stays
  true on an incomplete inventory, and the human line says the count is
  unknown. Healthy output is unchanged. (#333)
- `status` no longer promises that `riftri repair` removes an interrupted
  journal-write temporary in directories repair's reaper deliberately does not
  cover; those are reported as preserved, and the advice and the reaper now
  share one definition so they cannot drift apart. (#331)
- The repository cleanliness probe passes `--untracked-files=all`, so
  `status.showUntrackedFiles=no` in repository or global configuration can no
  longer make `riftri doctor` report a working tree with untracked content as
  clean. (#330)
- `remove_worktree_force` passes `--` before the worktree path, matching its
  non-force sibling, so a selector beginning with `-` cannot be parsed as
  options on the one path that bypasses Git's dirty-worktree checks. (#329)
- Unsupported-platform messages for garbage collection, removal, move, and
  prune name macOS, Linux, and Windows instead of claiming the features
  require macOS. (#321)
- npm installation works again end to end, and one registry-blocked package
  name no longer breaks the whole release: the publisher continues past a
  repeat refusal so the remaining packages still reach npm. (#345)
- Website: the hero install box stacks cleanly on mobile (#318), the install
  reference links meet the 44px touch-target minimum with a guard test (#320),
  and the copy controls drop a non-conformant `aria-label` and derive their
  failure guidance from the document they actually copy (#319).

## [0.3.5] - 2026-09-21

### Fixed

- The shipped rich (`tui`) build now escapes control characters in human output
  per output stream, matching the default build's #310 fix. Sanitization was
  gated on `policy.rich`, which is false whenever stdout/stderr is redirected,
  `CI` is set, or `TERM=dumb` — so when stdout stayed an interactive terminal
  (for example `riftri status 2>/dev/null`), an untrusted branch name, worktree
  path, or git stderr containing terminal escape sequences (for example
  `\x1b[2J`) could clear the screen or spoof output. `print_line`, `print_error`,
  and the `progress` fallback now sanitize when their own target stream is an
  interactive terminal, independent of `policy.rich` (which still governs the
  animated/styled rendering). This closes the same escape-injection class as
  #310 for the rich build. Piped/redirected output and machine (`--json`) output
  stay byte-for-byte faithful.

## [0.3.4] - 2026-09-20

### Fixed

- `riftri completions <shell>` and `riftri shell hook|deactivate <shell>` no
  longer abort with a panic backtrace (exit 101) when the reader closes the pipe
  early (`| head`, `| less` then `q`). These stdout writers bypassed the
  broken-pipe-safe path added for `--json` output: `print!` panicked on `EPIPE`
  and `clap_complete::generate` `.expect()`ed its writer. Completions now render
  into an in-memory buffer and every one of these writers flows through a shared
  broken-pipe-safe path that exits cleanly (code 0). Hook output stays
  byte-exact, so sourced hooks are unaffected.
- Default (non-`tui`) builds again escape control characters in human output
  written to an interactive terminal, so an untrusted branch name, path, or git
  stderr containing terminal escape sequences (for example `\x1b[2J`) can no
  longer clear the screen or spoof output. The `tui`-feature split moved
  control-character sanitization into the rich renderer only, leaving the
  default binary emitting raw bytes on a terminal — a regression against
  pre-split behavior. Piped/redirected output and machine (`--json`) output stay
  byte-for-byte faithful, matching the rich build's policy.
- CI now test-executes the default/plain build again. Gating the setup TUI
  behind the `tui` feature made the Quality job run tests only with
  `--features riftri-cli/tui`, which deselects `crates/riftri-cli/src/ui/plain.rs`
  and left the default build shipped by `cargo install riftri-cli` and
  `npm run build:native` unexecuted (MSRV only `cargo check`s it). The Quality
  job now also runs `cargo test --workspace --locked` with default features on
  Linux. New workflow guard tests assert the release build keeps
  `--features tui` (alongside `-p riftri-cli`, `--release`, `--locked`) and that
  the workspace `[profile.release]` keeps `strip = true`, so #290's shipped
  feature set and stripping cannot silently regress.
- Copy-on-write worktree directories now regain the umask-appropriate group and
  other write bits, matching a plain `git worktree add`. Restoring write access
  after cloning a read-only base only re-added the owner bits to directories, so
  under `umask 002` or `core.sharedRepository=group` a `0o775` directory came
  back as `0o755`: a second group member could edit files but could not create,
  rename, or delete entries inside the directory. Directories now mirror the
  file fix and OR in `0o700` plus the umask-appropriate write bits.
- `status --json` now counts each OverlayFS worktree's on-disk write cost in
  `total_allocated_bytes` instead of silently dropping it. The CoW-aware total
  subtracts each view's shared base blocks so they are counted once, but an
  OverlayFS view's `allocated_bytes` already measures only its private
  upper/work layers (base-exclusive), so subtracting the base again saturated
  every OverlayFS view to zero and omitted its real footprint. The subtraction
  now applies only to backends whose per-tree measurement re-counts the base
  (APFS clone, reflink, ReFS block clone); OverlayFS views are added whole.

### Changed

- The interactive `riftri setup` terminal UI is now behind an off-by-default
  `tui` Cargo feature. Default builds omit ratatui and crossterm, dropping the
  release binary from 3.03 MB to 2.70 MB; `setup` still works through plain
  text prompts. Published release archives build with `--features tui` to keep
  the styled experience. Release binaries are now stripped.

## [0.3.3] - 2026-09-20

### Fixed

- Decoding `/proc/self/mountinfo` path escapes no longer overflows when a mount
  path contains an octal escape whose leading digit is 4 or greater. Such
  escapes cannot name a single byte (the maximum is `\377`), so they previously
  panicked in debug builds and silently wrapped to the wrong byte in release
  builds; they are now rejected as a malformed layout, and the escape validator
  only accepts representable `\000`–`\377` sequences.
- `--json-errors` now reports command-line usage errors (a missing argument or
  unknown flag rejected by the parser) as one JSON receipt on stderr, with
  `"code": "usage-error"`, `"category": "usage"`, and clap's usage exit code
  `2`, instead of printing clap's plain human-readable text. A caller that
  always parses `--json-errors` stderr as JSON no longer receives plain text for
  usage errors. `--help` and `--version` still print normally and exit `0`, and
  without `--json-errors` usage errors keep clap's exact text and exit code.
- `status --json` now reports `total_allocated_bytes` as the actual physical
  footprint on disk, counting each copy-on-write base's shared blocks once,
  instead of a naive per-tree sum. Every view is a CoW clone that shares
  physical blocks with its base, so summing each tree's `allocated_bytes`
  double-counted the shared blocks: a freshly created view made the total jump
  by roughly its base's full allocation even though almost nothing new was
  written, badly overstating usage for a tool whose headline value is space
  savings. The total now adds each base once plus, per view, only the blocks
  that diverge from its base (`max(0, view − base)`), falling back to a view's
  full allocation when its base cannot be resolved. Per-tree `allocated_bytes`
  fields are unchanged. The total is an approximation: it assumes a view's
  extra allocation is entirely unshared and does not detect blocks shared
  between sibling views.
- Removal journal decoding now validates `overlayfs_clean_snapshot` with the
  same digest-shape rule already applied to `force_snapshot` — a stored value
  must be exactly 64 lowercase hexadecimal characters — so a corrupted durable
  record is rejected as an invalid journal instead of surviving decode.
- Piping a riftri command into a reader that closes early (for example
  `riftri status | head` or `riftri doctor --json | head`) no longer panics
  with a `BrokenPipe` error and exit code 101. Human-readable and machine
  (`--json`) stdout writes now treat a closed pipe as a clean stop and exit
  quietly. Handling the write error kind (rather than resetting the SIGPIPE
  disposition) keeps this portable to Windows and still lets terminal cleanup
  run for any other write failure.
- COW worktrees no longer silently drop the group/other write bits. Because the
  base tree is made read-only (`0o444`) before cloning, files re-gained only the
  owner write bit afterwards, leaving `0o644` where a plain `git worktree add`
  under `umask 002` or `core.sharedRepository=group` would leave `0o664`. The
  APFS and reflink backends now restore write bits according to the process
  umask, matching a normal checkout.
- The macOS/Linux `install.sh` now runs its `--version` sanity check against
  the staged binary on the install directory's filesystem instead of the
  freshly extracted copy in the temp directory. Hosts that mount `/tmp` (or
  `$TMPDIR`) `noexec` — a common CIS-hardened default — could previously abort
  a valid install with a misleading "Downloaded binary cannot run" error even
  though the download, platform detection, and checksum were all correct. The
  checksum gate, single-member archive validation, and atomic stage-then-rename
  are unchanged.
- Cloning a Windows symlink into a worktree now chooses the file-vs-directory
  reparse type from the source link's own attributes via `symlink_metadata`
  instead of `metadata`, which followed the link to its target. A dangling
  link (target missing at clone time) is no longer forced to a file symlink,
  and a link whose target's kind differs from the link's is no longer
  misclassified.

## [0.3.2] - 2026-09-19

### Added

- Guided setup and interactive confirmations now render through a Ratatui
  terminal interface: editable inputs, arrow-key choices with safe defaults
  (confirmations default to **No**, agent selection to **Not now**), and
  PgUp/PgDn review for long plans and paths, with a restrained orange accent
  across human-readable reports and help and an indeterminate spinner driven
  only by real lifecycle events. `--plain` restores line-oriented prompts and
  undecorated reports; `--no-animation` (or a nonempty `RIFTRI_NO_ANIMATION`)
  keeps the interface but replaces the spinner with bounded phase lines;
  `--no-progress` suppresses progress; a nonempty `NO_COLOR` disables colors
  while keeping keyboard navigation. Redirected stdout or stderr, `CI`, and
  `TERM=dumb` fall back to plain output automatically. JSON, `--json-errors`,
  generated shell hook/deactivation code, completions, the Git shim, and child
  process streams are never decorated, and terminal modes, cursor visibility,
  and inherited Unix signal dispositions are restored before any transaction or
  agent launch.

### Fixed

- Suggested recovery commands in messages and JSON receipts now use POSIX
  shell quoting on every platform instead of choosing the dialect at build
  time. A Windows build previously emitted PowerShell quoting that a POSIX
  shell such as Git Bash or WSL — where Git work commonly happens on Windows —
  parses as a different path. The exact path stays in each receipt's
  native-path field for any other shell.
- Four machine-contract defects found by a contract audit of the CLI's JSON
  reports and failure receipts. `worktree list --all-states --json` now emits
  a `state_directory_native_hex` sibling beside each diagnostic entry's
  `state_directory` display string — previously the only display path in the
  CLI without a hex partner — and the registration-level branch that reports
  `state_directory: null` emits the hex key as explicit `null` too, instead
  of omitting it. `riftri setup --json-errors` now reports the refusal of
  that flag combination as the policy shape (`category: policy`, exit code 3,
  `cleanup: not-needed`, `recovery: not-required`) instead of an operational
  failure inviting harnesses to retry and inspect state that was never
  touched. The symlinked-base safety stop's receipt now agrees with its own
  message: it reports `recovery: inspect` with the exact
  `riftri status --state-dir …` command the message names — built by the
  same helper, so the two cannot diverge — and its `stateDirectory` names
  the affected state directory (a new narrow `SymlinkedBaseParent` error
  variant carries it; the code stays `invalid-request` and the exit code
  stays 3). `doctor --json` now always emits
  `destination_readiness.backend` and `destination_readiness.next_command`,
  explicitly `null` when absent, instead of dropping the keys on a blocked
  destination. None of these additive, null-consistent changes bump any
  `schema_version`.

- Lifecycle commands now refuse the remaining inherited Git environment
  overrides that could redirect their internal Git operations:
  `GIT_OBJECT_DIRECTORY` and `GIT_ALTERNATE_OBJECT_DIRECTORIES`, plus
  environment-based configuration injection via `GIT_CONFIG_COUNT` (the
  `GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n` family) and
  `GIT_CONFIG_PARAMETERS`, exactly as `GIT_DIR`, `GIT_WORK_TREE`,
  `GIT_COMMON_DIR`, and `GIT_INDEX_FILE` were already refused before any
  mutation. Ordinary Git passthrough through the shim and `riftri exec` is
  unchanged and still delegates these variables to the user's own Git
  commands. Separately, the isolated base materialization no longer re-injects
  a caller-set `GIT_ALTERNATE_OBJECT_DIRECTORIES` into its private checkout
  environment; its comment always said every inherited override is removed,
  and now the behavior matches, so a foreign object store can no longer
  satisfy a materialization with objects absent from the source repository.
- `riftri setup` now validates the destination before printing the plan and
  asking for confirmation. The plan step runs the same destination pre-checks
  the explicit `riftri worktree add` performs — an existing destination
  (including a symlink to an existing target) and checkout paths that cannot
  coexist on the destination filesystem (case or Unicode-normalization
  collisions) — by calling the add path's own validation, so the diagnostics
  and the policy exit code are identical to the explicit command's.
  Previously setup confidently printed the full plan and asked "Create this
  worktree?" for a destination the creation step was always going to refuse;
  the refusal itself was already safe, but the guided flow confirmed a plan
  it had enough information to reject.
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
- Garbage collection that cancels after removing a base's `.complete` marker —
  because a new reference raced in between the marker removal and the
  protected re-check — now restores the completion marker in the same
  journaled step as the cancellation. The marker is recomputed from the base
  on disk with the current versioned digest — the same content-and-metadata
  hash reuse verification checks, never replayed from remembered bytes, so it
  cannot vouch for a base modified behind Riftri's back — and staged next to
  the base before an atomic rename, so no interruption window can leave a
  truncated marker.
  Previously the cancellation dropped the marker on the floor; if the racing
  add then rolled back before rebuilding the base, the fully materialized tree
  became invisible to marker-driven enumeration forever — `gc` could never
  propose it again and `status` never accounted for it. Recovery of an
  interruption anywhere around the restore is idempotent: it settles on either
  the completed collection or the restored marker, and a base leaked by the
  old behavior is at least surfaced by `status` as an unexplained
  immutable-base artifact diagnostic.

- The Windows ReFS block cloner now verifies the length of the sub-cluster
  tail copy that follows aligned extent cloning. The tail was written with
  `std::io::copy` over a `take` adaptor and the returned byte count discarded,
  so a source that yielded fewer bytes than the recorded file size — a base
  file truncated concurrently, or a stale size — left the clone's tail
  zero-filled (the destination had already been extended with `set_len`) while
  the clone reported success. A short read now fails the clone with an
  explicit error naming the source and destination files and the expected
  versus copied byte counts, mirroring the read-length check the base
  integrity hash already performs.

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

- APFS worktree creation and compaction restore writable clone permissions
  during the existing clone traversal, avoiding a second directory scan without
  skipping base-integrity, index, clean-state, or recovery checks. Linux and
  ReFS behavior is unchanged.

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
