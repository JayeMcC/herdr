#!/usr/bin/env python3
"""End-to-end input latency bench for twodr, driven through a real client in a pty.

Runs against an ISOLATED session (never the operator's), with inherited TWODR_*/HERDR_*
variables stripped, using the binary given on the command line for both client and server
(the client spawns its server from its own executable).

Per scenario it records the external latency: time from writing the input to the client's
pty until the client's output first changes (first byte) and until it goes quiet (settle).
A build with the client_rtt capability also logs its own per-kind round trip to
twodr-client.log; that line is collected when the client exits.

Usage: client_rtt_bench.py <twodr-binary> <label> <busy-panes> <out.json>
"""

import faulthandler
import fcntl
import json
import os
import pty
import re
import select
import shutil
import signal
import socket
import struct
import subprocess
import sys
import termios
import time

BIN, LABEL, BUSY, OUT = sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4]
SESSION = f"rtt-bench-{os.getpid()}"
BASE = os.path.expanduser(f"~/.config/twodr/sessions/{SESSION}")
SOCK = os.path.join(BASE, "twodr.sock")
COLS, ROWS = 200, 60


def env():
    clean = {k: v for k, v in os.environ.items() if not k.startswith(("TWODR", "HERDR"))}
    clean["TERM"] = "xterm-256color"
    return clean


def cli(*args):
    try:
        return subprocess.run([BIN, "--session", SESSION, *args], env=env(),
                              capture_output=True, timeout=15)
    except subprocess.TimeoutExpired:
        return None


def stop_server():
    """Stop only this bench's server, identified by the pid its own log recorded."""
    # lsof on the socket can block for minutes on this host, so use the pid the server logged.
    pids = []
    server_log = os.path.join(BASE, "twodr-server.log")
    if os.path.exists(server_log):
        found = re.findall(r'subsystem="server" outcome="started" pid=(\d+)', open(server_log).read())
        pids = found[-1:]
    for server in pids:
        try:
            os.kill(int(server), signal.SIGTERM)
        except ProcessLookupError:
            pass
    time.sleep(0.5)
    for server in pids:
        try:
            os.kill(int(server), signal.SIGKILL)
        except ProcessLookupError:
            pass


def api(method, params):
    sock = socket.socket(socket.AF_UNIX)
    sock.settimeout(30)
    sock.connect(SOCK)
    sock.sendall((json.dumps({"id": "b", "method": method, "params": params}) + "\n").encode())
    buf = b""
    while not buf.endswith(b"\n"):
        chunk = sock.recv(1 << 20)
        if not chunk:
            break
        buf += chunk
    sock.close()
    return json.loads(buf)


def drain(fd, quiet_s, cap_s):
    start = time.perf_counter()
    last = start
    while True:
        ready, _, _ = select.select([fd], [], [], quiet_s)
        now = time.perf_counter()
        if ready:
            try:
                if not os.read(fd, 1 << 16):
                    return
            except OSError:
                return
            last = now
        elif now - last >= quiet_s or now - start > cap_s:
            return


def measure(fd, payload, gaps_s=None, quiet_s=0.04, cap_s=5.0):
    """Write payload (optionally one byte-group at a time with gaps), return first/settle ms
    measured from the first write."""
    drain(fd, 0.03, 1.0)
    start = time.perf_counter()
    if gaps_s is None:
        os.write(fd, payload)
    first = None
    if gaps_s is not None:
        for index, part in enumerate(payload):
            os.write(fd, part)
            # Watch for output while pacing so first-byte time is not lost.
            deadline = time.perf_counter() + gaps_s[index]
            while True:
                remaining = deadline - time.perf_counter()
                if remaining <= 0:
                    break
                ready, _, _ = select.select([fd], [], [], remaining)
                if ready:
                    try:
                        os.read(fd, 1 << 16)
                    except OSError:
                        break
                    if first is None:
                        first = time.perf_counter()
        last = time.perf_counter()
        wrote_all = last
    else:
        last = start
        wrote_all = start
    while True:
        ready, _, _ = select.select([fd], [], [], quiet_s)
        now = time.perf_counter()
        if ready:
            try:
                os.read(fd, 1 << 16)
            except OSError:
                break
            if first is None:
                first = now
            last = now
        elif now - last >= quiet_s or now - start > cap_s:
            break
    first_ms = (first - start) * 1000 if first else None
    return first_ms, (last - start) * 1000, (last - wrote_all) * 1000


def pct(values, fraction):
    values = sorted(v for v in values if v is not None)
    if not values:
        return None
    return round(values[min(len(values) - 1, int(len(values) * fraction))], 2)


def cpu_ms(pid):
    try:
        text = subprocess.check_output(["ps", "-o", "time=", "-p", str(pid)]).decode().strip()
    except subprocess.CalledProcessError:
        return 0.0
    minutes, seconds = text.split(":")
    return (int(minutes) * 60 + float(seconds)) * 1000


def server_pid():
    server_log = os.path.join(BASE, "twodr-server.log")
    found = re.findall(r'subsystem="server" outcome="started" pid=(\d+)', open(server_log).read())
    return int(found[-1])


def phase(client, server, inputs, run):
    c0, s0 = cpu_ms(client), cpu_ms(server)
    samples = run()
    c1, s1 = cpu_ms(client), cpu_ms(server)
    firsts = [s[0] for s in samples]
    settles = [s[1] for s in samples]
    tails = [s[2] for s in samples]
    return {"n": len(samples), "first_p50_ms": pct(firsts, 0.5), "first_p95_ms": pct(firsts, 0.95),
            "settle_p50_ms": pct(settles, 0.5), "settle_p95_ms": pct(settles, 0.95),
            "settle_max_ms": round(max(settles), 2),
            "tail_p50_ms": pct(tails, 0.5), "tail_max_ms": round(max(tails), 2),
            "client_cpu_per_input_ms": round((c1 - c0) / inputs, 3),
            "server_cpu_per_input_ms": round((s1 - s0) / inputs, 3)}


def log(msg):
    print(f"[{time.strftime('%H:%M:%S')}] {msg}", file=sys.stderr, flush=True)


def main():
    shutil.rmtree(BASE, ignore_errors=True)
    pid, fd = pty.fork()
    if pid == 0:
        os.execvpe(BIN, [BIN, "--session", SESSION], env())
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
    drain(fd, 1.5, 20)
    panes = api("pane.list", {})["result"]["panes"]
    pane, ws = panes[0]["pane_id"], panes[0]["workspace_id"]
    # The focused pane runs `cat`, so every key echoes back through the pane like a prompt.
    api("pane.send_text", {"pane_id": pane, "text": "clear; seq 1 3000; stty -echoctl; cat\r"})
    for _ in range(BUSY):
        created = api("tab.create", {"workspace_id": ws, "cwd": "/tmp", "focus": False})
        busy = created["result"]["root_pane"]["pane_id"]
        api("pane.send_text", {"pane_id": busy, "text":
            "while :; do printf '\\r%s busy %s' $(date +%s%N) $RANDOM; sleep 0.05; done\r"})
    drain(fd, 2.0, 15)

    log("setup done")
    client, server = pid, server_pid()
    results = {"label": LABEL, "busy_panes": BUSY, "panes": BUSY + 1}
    only_dictation = os.environ.get("RTT_BENCH_ONLY_DICTATION") == "1"
    bursts = int(os.environ.get("RTT_BENCH_DICTATION_BURSTS", "6"))
    if not only_dictation:
        results["key"] = phase(client, server, 60, lambda: [measure(fd, b"k") for _ in range(60)])
        results["enter"] = phase(client, server, 40,
                                 lambda: [measure(fd, b"\r") for _ in range(40)])

    def dictation():
        samples = []
        for _ in range(bursts):
            text = ("the quick brown fox jumps over the lazy dog " * 5)[:200].encode()
            parts = [text[i:i + 1] for i in range(len(text))]
            # Wispr Flow-like pacing: 2-5 ms between characters.
            gaps = [0.002 + 0.003 * ((i * 7) % 10) / 10 for i in range(len(parts))]
            samples.append(measure(fd, parts, gaps_s=gaps, quiet_s=0.08, cap_s=10))
            measure(fd, b"\r")
        return samples

    results["dictation_200"] = phase(client, server, bursts * 201, dictation)
    # Scroll notches land on the pane holding the seq output.
    if not only_dictation:
        results["scroll"] = phase(client, server, 80, lambda: (
        [measure(fd, b"\x1b[<64;100;30M") for _ in range(40)]
        + [measure(fd, b"\x1b[<65;100;30M") for _ in range(40)]))

    log("measurements done, stopping client")
    os.kill(pid, signal.SIGTERM)
    # The client restores the host terminal on exit; keep reading so it never blocks on a
    # full pty, and stop waiting after a bound.
    deadline = time.time() + 5
    exited = False
    while time.time() < deadline:
        done, _ = os.waitpid(pid, os.WNOHANG)
        if done:
            exited = True
            break
        if select.select([fd], [], [], 0.05)[0]:
            try:
                os.read(fd, 1 << 16)
            except OSError:
                pass
    if not exited:
        log("client ignored SIGTERM for 5 s, killing")
        os.kill(pid, signal.SIGKILL)
        os.waitpid(pid, 0)
    log("client exited")
    time.sleep(0.3)
    client_log = os.path.join(BASE, "twodr-client.log")
    lines = []
    if os.path.exists(client_log):
        lines = [line.strip() for line in open(client_log, errors="replace") if "client_rtt" in line]
    results["client_rtt_log"] = lines[-3:]
    rtt = {}
    for line in lines:
        for kind, n, p50, p95, mx, mean in re.findall(
                r" (\w+)=n:(\d+),p50_us:(\d+),p95_us:(\d+),max_us:(\d+),mean_us:(\d+)", line):
            if int(n):
                rtt[kind] = {"n": int(n), "p50_ms": int(p50) / 1000, "p95_ms": int(p95) / 1000,
                             "max_ms": int(mx) / 1000, "mean_ms": int(mean) / 1000}
    results["client_rtt"] = rtt
    log("client stopped, stopping server")
    stop_server()
    shutil.rmtree(BASE, ignore_errors=True)
    with open(OUT, "w") as handle:
        json.dump(results, handle, indent=2)
    print(json.dumps(results, indent=2))


if __name__ == "__main__":
    faulthandler.dump_traceback_later(60, exit=True)
    main()
