#!/usr/bin/env bash
# A scripted stand-in for an ACP agent (Agent Client Protocol, JSON-RPC over
# stdio) – NOT an AI. scripts/screenshots.sh registers it as "Demo agent" in
# TeamCraft Desktop so that the README can show the Agent view without an
# AI provider, a key or Node.js: it answers the handshake, "uses" two tools and
# streams one fixed answer about the demo data. Nothing is read from the
# server; the numbers come from the environment of the screenshot run.
#
#   DEMO_ECO   change order waiting for the review   (ECO-000002)
#   DEMO_ECR   the change request about the roughness (ECR-000001)
#
# Requires bash and jq. Reads one JSON object per line, writes the same.
set -u

ECO=${DEMO_ECO:-ECO-000002}
ECR=${DEMO_ECR:-ECR-000001}

send() { printf '%s\n' "$1"; }

# update JSON – a session/update notification of the one session.
update() {
    send "$(jq -nc --argjson u "$1" '{jsonrpc: "2.0", method: "session/update", params: {sessionId: "demo-session", update: $u}}')"
}

chunk() {
    update "$(jq -nc --arg t "$1" '{sessionUpdate: "agent_message_chunk", content: {type: "text", text: $t}}')"
    sleep 0.15
}

tool() { # id title status
    update "$(jq -nc --arg i "$1" --arg t "$2" --arg s "$3" \
        '{sessionUpdate: (if $s == "pending" then "tool_call" else "tool_call_update" end), toolCallId: $i, title: $t, kind: "read", status: $s}')"
}

answer() {
    tool t1 "teamcraft · my worklist" pending
    sleep 0.4
    tool t1 "teamcraft · my worklist" completed
    tool t2 "teamcraft · open change requests" pending
    sleep 0.4
    tool t2 "teamcraft · open change requests" completed
    chunk $'Three things need your attention:\n\n'
    chunk "1. $ECO – Thicker enclosure cover for the IP67 seal – waits for your review (one of two approvals is in)."$'\n\n'
    chunk $'2. The mounting bracket is checked out by you; the anodising note is not checked in yet.\n\n'
    chunk "3. $ECR – Sealing face of the enclosure box is too rough – was raised by Jonas Lopez: the inspection report measures Ra 3.2 µm where the drawing asks for 1.6 µm at most, and the IP67 leak test failed on the same lot. Mia Chen is triaging it, and it belongs to the cover change $ECO."$'\n\n'
    chunk 'I have not changed anything.'
}

while IFS= read -r line; do
    method=$(jq -r '.method // empty' <<<"$line" 2>/dev/null) || continue
    id=$(jq -c '.id // null' <<<"$line" 2>/dev/null) || continue
    case $method in
    initialize)
        send "$(jq -nc --argjson id "$id" '{jsonrpc: "2.0", id: $id, result: {
            protocolVersion: 1,
            agentCapabilities: {mcpCapabilities: {http: true}},
            agentInfo: {name: "demo-agent", title: "Demo agent", version: "scripted"},
            authMethods: []}}')"
        ;;
    session/new)
        send "$(jq -nc --argjson id "$id" '{jsonrpc: "2.0", id: $id, result: {sessionId: "demo-session"}}')"
        ;;
    session/prompt)
        answer
        send "$(jq -nc --argjson id "$id" '{jsonrpc: "2.0", id: $id, result: {stopReason: "end_turn"}}')"
        ;;
    "") ;; # a response to a request of ours, or noise
    session/cancel) ;;
    *)
        if [[ $id != null ]]; then
            send "$(jq -nc --argjson id "$id" '{jsonrpc: "2.0", id: $id, error: {code: -32601, message: "method not found"}}')"
        fi
        ;;
    esac
done
