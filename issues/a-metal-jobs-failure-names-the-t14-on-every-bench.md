---
status: open
kind: finding
opened: 2026-10-20
---

# A metal job's failure names the T14 on every bench

`Readback::job_passed` (`tests/common/metal.rs`) words a job that exited
nonzero as "exited {code} on the T14", whichever bench the boot ran on. The
AMD bench's judge printed it for `test_rs_end_returns_claims` and
`test_rs_usbd_spare_vector` (PR #947's bench rounds 1 and 6), sending a reader
to the wrong machine's log. The bench a run is on is `--metal-host`'s, which
the harness already holds.

Owner: the orchestrator; it belongs with
`issues/a-metal-row-names-no-hardware-and-a-bench-without-it-reds.md`.

Exit: the message names the bench the readback came from, or no bench.
