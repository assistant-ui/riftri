## Summary

Describe the behavior or documentation changed by this pull request.

## Safety and compatibility

- [ ] The change preserves real Git linked-worktree semantics.
- [ ] Destructive behavior is opt-in and validates exact paths.
- [ ] Native paths are not assumed to be UTF-8 in low-level code.
- [ ] Backend fallback remains visible and explicit.

## Verification

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `npm test`
- [ ] `npm run pack:check`
