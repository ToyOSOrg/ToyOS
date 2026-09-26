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

Not shown: what the 24 bytes were, or whether the guest booted at all in the
red run; the failure message names the drain's size and nothing of its
content.

**Exit**: the red run's drain printed on failure, and a cause for a job boot
that drained nothing beside other guests.
