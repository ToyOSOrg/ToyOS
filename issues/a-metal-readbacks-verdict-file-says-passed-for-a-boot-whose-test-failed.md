---
status: open
kind: tooling
opened: 2026-10-21
---

# A metal readback's `verdict.txt` says `passed` for a boot whose test failed

`toyos-metal` writes `verdict.txt` (`READBACK_VERDICT`, `tests/metal/src/lib.rs`) into every
readback directory: `passed` when its loop flashed, booted and read the stick back without a
`Refusal`. It is the loop's verdict on the boot and says nothing about any test, which only the
judging run (`--metal --metal-readback`) decides from `kernel.log`. A file of that name reading
`passed` beside a log whose test exited 101 is read, and published, as the test's verdict.

**Evidence.** #967's `probe-control` boot: the readback's `verdict.txt` read `passed`, its log
ends `===TEST_END test_rs_tlb_shootdown_waits exit=101===`, and the judge said `FAIL
tlb_shootdown_waits`; the evidence first published it as the boot's verdict (pr-967 at
6f6bbae1a, corrected at 2188ec622). The same `passed` stands in every readback.

Owner: the orchestrator.

**Exit**: nothing in a readback directory reads as a test verdict unless it is one — the loop's
file is named for what it judges.
