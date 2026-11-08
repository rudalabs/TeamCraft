#!/bin/bash
# Only a bare "git push"/"git pull" and "git add --all" are allowed, and never a push from main.
# Also blocks all "git worktree" commands (Claude must not create worktrees).
set -uo pipefail
command="$(cat | jq -r '.tool_input.command // ""')"
deny() {
    jq -nc --arg reason "$1" '{hookSpecificOutput: {
        hookEventName: "PreToolUse",
        permissionDecision: "deny",
        permissionDecisionReason: $reason}}'
    exit 0
}

# Block any worktree-related command immediately
case "$command" in
    *"git worktree"* | *"git-worktree"*)
        deny "Refused: git worktree commands are not allowed (Claude must not create or manage worktrees)."
        ;;
esac

case "$command" in
    *"git push"* | *"git pull"* | *"git add"*) ;;
    *) exit 0 ;;
esac

trimmed="$(printf '%s' "$command" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
repo="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"

case "$trimmed" in
"git push")
    branch="$(git -C "$repo" rev-parse --abbrev-ref HEAD || true)"
    [[ -z $branch || $branch == "HEAD" ]] && deny "Refused: current branch could not be determined."
    [[ $branch == "main" ]] && deny "Refused: nothing may be pushed from main."
    exit 0
    ;;
"git pull") exit 0 ;;
"git add --all") exit 0 ;;
esac

deny "Refused: only a bare \"git push\", \"git pull\" or \"git add --all\" is allowed. Got: ${trimmed}"
