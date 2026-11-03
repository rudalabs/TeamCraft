#!/bin/bash
# Only a bare "git push"/"git pull" and "git add --all" are allowed, and never a push from main.
set -uo pipefail

command="$(cat | jq -r '.tool_input.command // ""')"

deny() {
    jq -nc --arg reason "$1" '{hookSpecificOutput: {
        hookEventName: "PreToolUse",
        permissionDecision: "deny",
        permissionDecisionReason: $reason}}'
    exit 0
}

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
    exit 0
    ;;
"git pull") exit 0 ;;
"git add --all") exit 0 ;;
esac

deny "Refused: only a bare \"git push\", \"git pull\" or \"git add --all\" is allowed. Got: ${trimmed}"
