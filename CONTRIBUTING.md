# Contributing to Riftri

Thank you for helping build Riftri. Start with `PROJECT.md`, `ROADMAP.md`,
`AGENTS.md`, and `docs/architecture.md`; safety and ordinary Git compatibility
take priority over reaching a filesystem demo quickly.

## Development setup

Install stable Rust with Rustfmt and Clippy, Git, and Node.js 18.18 or newer.
Then run:

```console
$ npm ci --ignore-scripts --omit=optional
$ cargo fmt --all --check
$ cargo clippy --workspace --all-targets --all-features -- -D warnings
$ cargo test --workspace
$ npm test
```

The npm package is only a distribution launcher. Product behavior belongs in
the Rust crates, following their documented responsibilities.

## Changes

- Begin behavioral changes with a failing test or reproducible fixture.
- Keep worktree mutation and cleanup explicitly opt-in.
- Preserve platform-native paths in low-level APIs.
- Use real Git plumbing and stable machine-readable output.
- Add platform integration tests before claiming backend support.
- Update `docs/decisions.md` for foundational decisions and `ROADMAP.md` only
  when all relevant acceptance criteria pass.

Pull requests should explain their safety impact, compatibility assumptions,
and exact verification commands. Do not include credentials, private repository
contents, generated native binaries, or benchmark data that exposes private
paths.
