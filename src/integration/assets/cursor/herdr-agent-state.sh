#!/bin/sh
# managed by herdr; reinstalling the integration replaces this file.
# TWODR_INTEGRATION_ID=cursor
# TWODR_INTEGRATION_VERSION=2

[ "${1:-}" = "session" ] || exit 0
# twodr panes export TWODR_* and, until it is dropped, a HERDR_* alias.
# Prefer the twodr name so a pane from either side of that change reports.
HERDR_ENV="${TWODR_ENV:-${HERDR_ENV:-}}"
HERDR_PANE_ID="${TWODR_PANE_ID:-${HERDR_PANE_ID:-}}"
HERDR_SOCKET_PATH="${TWODR_SOCKET_PATH:-${HERDR_SOCKET_PATH:-}}"
export HERDR_ENV HERDR_PANE_ID HERDR_SOCKET_PATH
[ "${HERDR_ENV:-}" = "1" ] || exit 0
[ -n "${HERDR_SOCKET_PATH:-}" ] || exit 0
[ -n "${HERDR_PANE_ID:-}" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

python3 -c '
import json
import os
import socket
import sys
import time

try:
    payload = json.load(sys.stdin)
except Exception:
    raise SystemExit(0)

def first_text(*names):
    for name in names:
        value = payload.get(name)
        if isinstance(value, str) and value:
            return value
    return None

event = first_text("hook_event_name", "hookEventName")
if event not in (None, "sessionStart"):
    raise SystemExit(0)

session_id = first_text("session_id", "sessionId", "conversation_id", "conversationId")
if session_id is None:
    raise SystemExit(0)

seq = time.time_ns()
request = json.dumps({
    "id": f"herdr:cursor:{seq}",
    "method": "pane.report_agent_session",
    "params": {
        "pane_id": os.environ["HERDR_PANE_ID"],
        "source": "twodr:cursor",
        "agent": "cursor",
        "seq": seq,
        "agent_session_id": session_id,
    },
})
try:
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
        client.settimeout(0.5)
        client.connect(os.environ["HERDR_SOCKET_PATH"])
        client.sendall((request + "\n").encode())
        client.recv(4096)
except Exception:
    pass
' 2>/dev/null || true
