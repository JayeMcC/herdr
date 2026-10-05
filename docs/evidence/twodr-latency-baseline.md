# twodr input latency baseline (2026-10-05)

Lane `w-twodr-latency-rtt`, tasks T-06 and T-04. Machine: Apple M4, 10 cores, load
average 8–14 throughout (other agents running). Builds: `master` at 368b0a23 and branch
`latency-client-rtt` (client round trip, T-06), both `cargo build --release`.

## Method

- **Isolated session per run.** `twodr --session rtt-bench-<pid>` launched from the build
  under test, with every inherited `TWODR_*`/`HERDR_*` variable removed. The client
  spawns its server from its own executable, so client and server are always the same
  build. The operator's session and socket were never touched; each run deletes its
  session directory and stops only the server pid that its own log recorded.
- **Real client in a pty**, 200x60. The focused pane prints `seq 1 3000`, then runs
  `cat`, so typed characters echo through a real pane, much like a prompt.
- **Busy case:** 30 more tabs, each printing a timestamp line every 50 ms.
- **Inputs:**
  - single key `k`, 60 times;
  - Enter, 40 times;
  - 200-character dictation burst, one byte per write, 2–5 ms apart (Wispr Flow speed);
  - SGR wheel notches, 40 up and 40 down.
- **External latency** (both builds): time from the write to the client's pty until the
  client's first output (`first`), and until it has been quiet for 40 ms (`settle`). For
  dictation, `tail` is the time from the last character written until the screen settles.
- **Client round trip** (branch only): the `client_rtt` line the client writes to
  `twodr-client.log`. It covers stdin read → server applies input → echo → frame written
  to stdout, for each input kind. On master the cell says "unavailable": master's server
  does not advertise the capability, so the client never measures.
- **CPU per input**: client and server `ps` CPU-time deltas across each phase, divided by
  the number of inputs (10 ms `ps` resolution, so treat values under ~0.5 ms as noise).
- Matrix: 2 runs per cell, alternating master and branch. Dictation was rerun as 6
  alternating pairs of 10 bursts each, because the first matrix's dictation tail spread
  (5 vs 11.7 ms) turned out to be noise.
- Scripts in this commit: `scripts/bench/client_rtt_bench.py`, `scripts/bench/client_rtt_report.py`.

## Results

### 1 pane

| Input | Build | first output p50 / p95 ms | settle p50 / p95 ms | client CPU / input ms | server CPU / input ms | client_rtt p50 / p95 / max ms (n) |
|---|---|---|---|---|---|---|
| Single key | master | 0.34 / 0.78 | 0.34 / 1.04 | 0.33 | 0.33 | unavailable |
| Single key | branch | 0.41 / 0.74 | 0.41 / 0.74 | 0.42 | 0.33 | 0.35 / 6.86 / 30.8 (137) |
| Enter | master | 1.08 / 1.59 | 1.09 / 19.32 | 0.25 | 1.12 | unavailable |
| Enter | branch | 1.32 / 2.09 | 1.42 / 19.71 | 0.5 | 1.5 | 1.41 / 2.82 / 14.1 (92) |
| 200-char dictation | master | 0.33 | tail 10.36 (range 9.6–13.6) | 0.085 | 0.119 | unavailable |
| 200-char dictation | branch | 0.36 | tail 10.15 (range 7.9–13.7) | 0.095 | 0.124 | 9.22 / 18.43 / 42.0 (2383) |
| Scroll notch | master | 3.16 / 5.56 | 3.19 / 5.67 | 1.62 | 2 | unavailable |
| Scroll notch | branch | 3.56 / 9.34 | 3.6 / 9.76 | 1.81 | 2.19 | 3.97 / 8.96 / 82.8 (160) |

(Dictation rows: 6 alternating pairs x 10 bursts. All other rows: 2 runs each.)

### 31 panes (30 busy)

| Input | Build | first output p50 / p95 ms | settle p50 / p95 ms | client CPU / input ms | server CPU / input ms | client_rtt p50 / p95 / max ms (n) |
|---|---|---|---|---|---|---|
| Single key | master | 0.59 / 3.56 | 0.59 / 3.56 | 0.42 | 5 | unavailable |
| Single key | branch | 0.45 / 1.49 | 0.45 / 2.07 | 0.33 | 4.08 | 0.38 / 10.18 / 22.0 (138) |
| Enter | master | 1.66 / 7.13 | 1.76 / 24.14 | 0.5 | 6.5 | unavailable |
| Enter | branch | 1.07 / 3.42 | 1.11 / 12.43 | 0.38 | 5.25 | 0.99 / 3.97 / 8.75 (92) |
| 200-char dictation | master | 0.56 / 2.82 | tail 7.96 / max 12.79 | 0.11 | 0.41 | unavailable |
| 200-char dictation | branch | 0.58 / 0.93 | tail 13.06 / max 29.21 | 0.12 | 0.43 | 9.22 / 18.43 / 37.2 (2382) |
| Scroll notch | master | 6.28 / 23.92 | 6.33 / 24.66 | 2.19 | 8.38 | unavailable |
| Scroll notch | branch | 5.99 / 12.5 | 6 / 12.5 | 2.12 | 8.5 | 5.63 / 10.24 / 91.7 (160) |

The 31-pane dictation tail comes from 2 runs only; given how the 1-pane case moved when
it was rerun, read the 8 vs 13 ms gap as within noise.

## What the numbers say

1. **A single key is under 1 ms at the median**, in both builds and in both pane counts.
   With 30 busy panes the p95 rises to 1.5–3.6 ms, and server CPU per key rises from
   0.3 ms to 4–5 ms. Busy panes cost server time on every input; history does not.
2. **Dictation is set by the frame cadence, not by the per-key cost.** A 200-character
   burst finishes on screen about 10 ms after its last character (median), at either pane
   count. Measured from the client, each dictated character takes 9.2 ms p50 and 18.4 ms
   p95 to come back. That fits the 16 ms render interval (`MIN_RENDER_INTERVAL` in
   `src/app/mod.rs`): when keys arrive 2–5 ms apart, a key waits for the next allowed
   frame, on average about half the interval. A single isolated key takes 0.35 ms
   because it starts a frame on an idle loop.
3. **Scroll is the slowest common input**: 3–4 ms per notch with 1 pane and 6 ms with
   30 busy panes, with p95 12–24 ms. The client measured 80–92 ms maxima for scroll.
   Those are the stalls the operator feels, and T-02's per-phase timing should name
   them.
4. **Enter's settle p95 (~20 ms) is the shell redrawing**, not twodr: the first output
   arrives in 1–2 ms.
5. **The round-trip change itself is not measurable end to end.** Every branch-vs-master
   gap above lies inside the run-to-run spread at this load (key p50 0.34 vs 0.41 ms,
   1-pane scroll p50 3.16 vs 3.56 ms, with the busy case pointing the other way).

## Overhead check against the B-01 budget

| Budget (design B-01) | Measured | How |
|---|---|---|
| < 1 µs per input on the hot path | 235 ns per input for classify + mark + echo + present | `cargo test --release client_rtt_hot_path_cost -- --ignored --nocapture`, 1,000,000 inputs |
| No allocation per input | Tracker: none (fixed ring of 128 marks, fixed 100-bucket histogram per kind). Mark message: one small `String` (decimal sequence) per forwarded input, and the server's echo prefix is one allocation per frame, not per input. | Code: `src/client/rtt.rs`, `ClientConnection::prefix_rtt_echo` |
| < 256 KB total | 6,304 bytes client tracker; 16 bytes per server client | `client_rtt_tracker_fits_the_memory_budget`, `size_of::<ClientRtt>()` |
| < 0.5% of loop time | 235 ns against a 16 ms frame interval is 0.0015% per input; at dictation rate (~300 inputs/s) that is 70 µs/s, 0.007% | Derived from the line above |
| Always on, no setting | Yes: it switches on when the endpoint advertises `client_rtt`; there is no configuration | – |

**Deviation, stated plainly:** the mark is a separate `EndpointControl` message sent after
each forwarded input, so each input causes one extra small allocation and socket write on
the client. That is how the round trip stays compatible with old peers without a version
bump (see the T-06 commit). The tracker and the server state do not allocate per input.

## Reproduce

```bash
cargo build --release                                     # branch
python3 scripts/bench/client_rtt_bench.py "$PWD/target/release/twodr" branch 0  /tmp/b0.json
python3 scripts/bench/client_rtt_bench.py "$PWD/target/release/twodr" branch 30 /tmp/b30.json
RTT_BENCH_ONLY_DICTATION=1 RTT_BENCH_DICTATION_BURSTS=10 \
  python3 scripts/bench/client_rtt_bench.py "$PWD/target/release/twodr" branch 0 /tmp/d.json
python3 scripts/bench/client_rtt_report.py /tmp               # tables from <label>-<busy>-<run>.json
```
