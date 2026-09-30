---
status: open
kind: finding
opened: 2026-10-01
---

# `toolkit_winit_loop`'s output stopped reaching the log after stage 1, while the app ran on

At `09d74fcfb` the orchestrator's whole run `638r4-whole.log` went red on
`toolkit_winit_loop`. The host was at load average 43–52 on 14 cores. The
terminal logged `WINIT-LOOP stage 1` at 6.534 s and nothing else of the app's.

The app kept going. The compositor opened 23 of its windows by 9.448 s and
closed 22 of them. That is stages 3 to 5 and then stage 6's window, and each
of those windows is created after a `stage` line the app had already printed.
Stage 6 prints `WINIT-LOOP CLOSE-ME` on its first redraw. That line never
reached the log, so the harness never sent GUI+Q.

From then until the harness gave up at 42.8 s, every `sched:` line on every
CPU reads `ready=0`. The only speaker was the compositor's interval line,
every 2 s. The guest was idle with work outstanding, and it was not starved.
Candidates are a wake lost between the app's stdout, the terminal and logd, or
a terminal that stopped reading its pty.

In the 33 other whole-suite logs under the orchestrator's `logs/`, all six
stage lines arrive under `{… terminal}` within 0.373 to 2.446 s of the first.

Exit: the cause, found from a reproduction or from the next capture.
