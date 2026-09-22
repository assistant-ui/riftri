#!/bin/sh
# A stand-in for an agent you do not control. It knows nothing about Riftri —
# it just runs plain Git, the way Claude Code, Codex, and most harnesses do.
set -eu

echo "  [agent] git resolves to: $(command -v git)"
echo "  [agent] running: git worktree add $1 -b $2"
git worktree add "$1" -b "$2" >/dev/null
echo "  [agent] done; the worktree is mine to use"
