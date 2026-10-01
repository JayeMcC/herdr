#!/bin/sh
# managed by herdr; reinstalling the integration replaces this file.
# TWODR_INTEGRATION_ID=qodercli
# TWODR_INTEGRATION_VERSION=4

[ "${1:-}" = "session" ] || exit 0
# twodr panes export TWODR_* and, until it is dropped, a HERDR_* alias.
# Prefer the twodr name so a pane from either side of that change reports.
HERDR_ENV="${TWODR_ENV:-${HERDR_ENV:-}}"
HERDR_PANE_ID="${TWODR_PANE_ID:-${HERDR_PANE_ID:-}}"
HERDR_SOCKET_PATH="${TWODR_SOCKET_PATH:-${HERDR_SOCKET_PATH:-}}"
HERDR_BIN_PATH="${TWODR_BIN_PATH:-${HERDR_BIN_PATH:-}}"
export HERDR_ENV HERDR_PANE_ID HERDR_SOCKET_PATH HERDR_BIN_PATH
[ "${HERDR_ENV:-}" = "1" ] || exit 0
[ -n "${HERDR_SOCKET_PATH:-}" ] || exit 0
[ -n "${HERDR_PANE_ID:-}" ] || exit 0
command -v python3 >/dev/null 2>&1 || exit 0

python3 -c '
import json
import os
import subprocess
import sys
import time

try:
    payload = json.load(sys.stdin)
    session_id = payload.get("session_id")
    if not isinstance(session_id, str) or not session_id:
        raise ValueError
    subprocess.run(
        [
            os.environ.get("HERDR_BIN_PATH") or "herdr",
            "pane", "report-agent-session", os.environ["HERDR_PANE_ID"],
            "--source", "twodr:qodercli", "--agent", "qodercli",
            "--agent-session-id", session_id, "--seq", str(time.time_ns()),
        ],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        timeout=1,
        check=False,
    )
except Exception:
    pass
' 2>/dev/null || true
