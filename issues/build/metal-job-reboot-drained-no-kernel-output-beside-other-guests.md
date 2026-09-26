---
status: open
kind: finding
opened: 2026-09-26
---

# `metal_job_reboot` drained no kernel output beside other guests

Fast tier at `98e803cb` (PR #510's branch; another worktree's DNS work was
building at the same time): `the job drain carried no kernel output at all (24
bytes): every assertion below it would be a claim about nothing`, in 5 s. The
harness's re-run alone was green in 2 s, its log carrying `Boot: complete
(304ms)` and the loader's 32 lines. `cargo run -- --known-red` answers NO.

Fast tier at `4ad645a2` (PR #528's branch, load average 21–22 from other
worktrees' guests): red in the wide run (`QEMU died before ===READY===`) and
red again alone (`the job drain carried no kernel output at all (0 bytes)`),
so a re-run alone is not reliably green. Five interleaved rounds in one
session, that branch against `origin/main` in the same worktree: the branch
red in 1 of 5, `main` red in 2 of 5, with the same two shapes on both sides
(`main`: `…no kernel output at all (24 bytes)`, and `"===TEST_START
reboot===" never reached the job drain`). `cargo run -- --known-red` answers
NO.

Not shown: what the drained bytes were, or whether the guest booted at all in
a red run; the failure message names the drain's size and nothing of its
content.

**Exit**: the red run's drain printed on failure, and a cause for a job boot
that drained nothing beside other guests.
