# Global shell activation

Riftri can be present in every new sh, bash, or zsh session without enabling
every repository. These are separate controls:

```text
shell profile evaluates hook     repository has riftri.enabled=true
             |                                  |
             +---------------+------------------+
                             |
                 supported worktree commands
                    use Riftri transactions
```

Adding this line to a user-owned shell profile globally enables the lightweight
Git interception layer for that user:

```sh
eval "$(riftri shell hook zsh)"
```

Riftri never adds or removes that line. In repositories without `riftri enable`,
and outside Git repositories, the shim immediately starts the captured real Git
executable. Non-worktree Git commands are delegated in enabled repositories too.

## Inspect and reverse it

`riftri shell status [repository]` reports whether the current process inherited
a complete hook, whether the selected repository opted in, and whether those two
conditions make optimized interception effective.

Remove Riftri from the current shell with:

```sh
eval "$(riftri shell deactivate zsh)"
```

This removes every Riftri shim-directory entry from `PATH` and unsets the two
shim variables. It does not disable a repository. If the hook is in a profile,
remove that profile line yourself before opening another shell. Remove the line
before uninstalling a globally installed `riftri` package so new shells do not
print a command-not-found error while evaluating the stale profile command.

## Compatibility matrix

The automated suite covers the behaviors that make broad shell activation
non-disruptive:

| Area | Verified behavior |
| --- | --- |
| sh, bash, zsh | Every installed supported shell can evaluate the same hook. CI always requires sh and exercises the other shells when installed. |
| Disabled repository | Normal Git creates an ordinary linked worktree and no Riftri state. |
| Enabled repository on APFS | Supported adds create clean, real Git worktrees through strict native clones. |
| Child tools | Plain-shell, `claude`, and `codex`-named child harnesses inherit interception without Riftri-specific prompts. |
| Shared storage | Three child-created views reuse one immutable base while keeping separate writable views. |
| Delegation | Standard input, standard output, standard error, and exit status match direct Git for passthrough commands; a signalled child produces the conventional `128 + signal` shell status. |
| Repeated activation | The shim is placed first on `PATH` without accumulating leading entries. |
| Deactivation | All shim entries and environment markers are removed from the current shell. |
| Unsupported platforms | The npm launcher works, but mutation remains an explicit unsupported-backend error until a native backend exists. |

Run the matrix with the workspace tests. A non-gating release-mode latency probe
is also available:

```console
$ cargo test --release -p riftri-cli --test global_activation \
    global_shim_passthrough_latency_benchmark -- --ignored --nocapture
```

The latency probe reports direct and intercepted process startup per operation.
It has no fixed timing assertion because host load dominates small process
benchmarks; correctness gates must not become flaky.

One development run on macOS on 2026-09-09, using 50 release-mode invocations
of `git --version`, measured 10.48 ms per direct invocation and 12.93 ms per
intercepted invocation: about 2.45 ms of shim overhead per Git process. This is
an observation, not a performance guarantee. Riftri is absent from ordinary
file reads and writes, so this startup cost does not apply to editor, build, or
test filesystem traffic.

## Boundaries and edge cases

- Shell aliases and functions named `git` take precedence over `PATH`; Riftri
  cannot intercept them. Use `command git` or remove the alias/function when
  optimized lifecycle handling is required.
- Tools that use libgit2/JGit, call Git by an absolute path, replace `PATH`, or
  clear the environment bypass the shim. They continue to behave as they did
  before Riftri, but their worktree lifecycle operations are not optimized.
- `sudo`, containers, IDEs, GUI applications, and already-running terminals may
  use a different environment from a newly opened shell. Check the exact
  environment with `riftri shell status`.
- A pre-existing Git wrapper is captured as the real Git command. A wrapper that
  recursively resolves `git` through the modified `PATH` can loop; configure
  that wrapper to call its underlying Git by absolute path or use process-scoped
  `riftri exec` instead.
- Exporting `RIFTRI_BYPASS=1` disables optimization for that environment. This is
  an intentional escape hatch, not a safe way to mutate a managed lifecycle
  behind Riftri's journals.
- Repository consent is local Git configuration shared by that repository's
  linked worktrees. It is not copied to unrelated clones and is removed with
  `riftri disable`.
- Global shell activation adds one Rust shim process before delegated Git
  commands. It does not put Riftri in the filesystem read/write hot path; normal
  file access, builds, tests, and editor operations have no Riftri process or
  daemon overhead.

Riftri remains experimental. Keep valuable changes committed or backed up, and
use `riftri status` plus `riftri repair` when a lifecycle operation is
interrupted.
