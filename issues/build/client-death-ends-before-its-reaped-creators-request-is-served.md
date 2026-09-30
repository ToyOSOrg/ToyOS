---
status: open
kind: tooling
opened: 2026-10-01
---

# `metal_sim_client_death` can end before its reaped creator's request is served

`test_rs_compositor_client_death`'s root closes the pipe that releases the
grandchild holding the reaped creator's connection, probes the compositor and
goes on to its next case. Nothing it does waits for the grandchild's
`MSG_CREATE_WINDOW` to have been served, although its module header says every
step waits on the one before it. The grandchild's `a reaped creator's
connection still got a window` is the host's non-vacuity witness, and the host
reads it only out of `run_test`'s window, which closes at the root's
`===TEST_END`.

At `09d74fcfb`, in the orchestrator's whole run `638r4-whole.log` on a host at
load average 43–52 on 14 cores, the root reported all six cases and exited 0.
The compositor's `window opened client=37003 content=928,506 64x64` came after
`6 deaths survived`, and the witness never reached the window, so the run went
red on `the compositor never served a request from a reaped creator`. The
test's ceiling did not fire. The orchestrator counts it as the first red in 34
runs.

Exit: the witness is ordered before the verdict. Either the root waits for its
grandchild's line, or the host reads past `===TEST_END` for it under the run's
ceiling.
