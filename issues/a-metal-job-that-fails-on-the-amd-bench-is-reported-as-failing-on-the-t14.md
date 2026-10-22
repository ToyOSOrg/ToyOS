---
status: open
kind: defect
opened: 2026-10-11
---

# A metal job that fails on the AMD bench is reported as failing on the T14

`Boot::job_passed` (`tests/common/metal.rs`) words every non-zero
exit as `"{binary} exited {code} on the T14"`, whichever bench ran the boot.
A `--metal-host` run on the AMD bench printed
`FAIL test_rs_vector_state: test_rs_vector_state exited 101 on the T14`
(https://github.com/ToyOSOrg/ToyOS/pull/859#issuecomment-6113859804, the
`metal-amd-mut-ymm` log), so a reader of a red is told the wrong machine.

Owner: the metal suite (`tests/common/metal.rs`).

**Exit**: a red job's line names the bench its boot ran on, read from the
same source the run's own `the timings` line names it by, and
`tests/checks.rs` holds a non-T14 bench's failing job to its name.
