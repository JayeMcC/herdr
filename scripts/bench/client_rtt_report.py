#!/usr/bin/env python3
"""Summarise client_rtt_bench.py JSON results into Markdown tables.

Usage: client_rtt_report.py <dir-with-json>   (files named <label>-<busy>-<run>.json)
"""
import glob
import json
import os
import statistics
import sys

ROOT = sys.argv[1]
KINDS = [("key", "key", "Single key"), ("enter", "enter", "Enter"),
         ("dictation_200", "dictation", "200-char dictation"), ("scroll", "scroll", "Scroll notch")]
results = {}
for path in sorted(glob.glob(os.path.join(ROOT, "*-*-*.json"))):
    data = json.load(open(path))
    results.setdefault((data["label"], data["busy_panes"]), []).append(data)


def median(values):
    values = [v for v in values if v is not None]
    return round(statistics.median(values), 2) if values else None


def fmt(value):
    return "–" if value is None else f"{value:g}"


for busy in sorted({key[1] for key in results}):
    print(f"\n### {busy + 1} pane(s){' (' + str(busy) + ' busy)' if busy else ''}\n")
    print("| Input | Build | first output p50 / p95 ms | settle p50 / p95 ms | "
          "client CPU / input ms | server CPU / input ms | client_rtt p50 / p95 / max ms (n) |")
    print("|---|---|---|---|---|---|---|")
    for field, rtt_kind, title in KINDS:
        for label in ("master", "branch"):
            runs = results.get((label, busy), [])
            if not runs:
                continue
            rows = [run[field] for run in runs]
            first = f"{fmt(median([r['first_p50_ms'] for r in rows]))} / {fmt(median([r['first_p95_ms'] for r in rows]))}"
            if field == "dictation_200":
                # Time from the last character written until the screen settles.
                settle = (f"tail {fmt(median([r['tail_p50_ms'] for r in rows]))} / "
                          f"max {fmt(max(r['tail_max_ms'] for r in rows))}")
            else:
                settle = (f"{fmt(median([r['settle_p50_ms'] for r in rows]))} / "
                          f"{fmt(median([r['settle_p95_ms'] for r in rows]))}")
            cpu_c = fmt(median([r["client_cpu_per_input_ms"] for r in rows]))
            cpu_s = fmt(median([r["server_cpu_per_input_ms"] for r in rows]))
            rtt_rows = [run["client_rtt"].get(rtt_kind) for run in runs if run["client_rtt"].get(rtt_kind)]
            if rtt_rows:
                rtt = (f"{fmt(median([r['p50_ms'] for r in rtt_rows]))} / {fmt(median([r['p95_ms'] for r in rtt_rows]))}"
                       f" / {fmt(max(r['max_ms'] for r in rtt_rows))} ({sum(r['n'] for r in rtt_rows)})")
            else:
                rtt = "unavailable (no capability)"
            print(f"| {title} | {label} ({len(runs)} runs) | {first} | {settle} | {cpu_c} | {cpu_s} | {rtt} |")
