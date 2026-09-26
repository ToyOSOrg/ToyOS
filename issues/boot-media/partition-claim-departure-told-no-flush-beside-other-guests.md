---
status: open
kind: finding
opened: 2026-09-26
---

# `partition_claim_departure` told no flush of the loss beside other guests

Fast tier at `a58abf50` (PR #511's merged head; another worktree's FAT suite
ran on the host at the same time): `departure: 0 flushes were told of the
loss, not 1`. The guest's own lines say the opposite: `usb-storage: disk 0
came back on port 3 slot 1 as the same device ... so the flush of each writer
whose writes they were fails`, then `partition_claimant: the departing
partition's flush, over a write the device lost, refused with Io` and
`partition_claimant: PASS`. The harness's re-run alone was green (`departure:
1 told`), and so was `cargo test --test toyos-build --
partition_claim_departure` alone afterwards (EXIT=0). `cargo run --
--known-red` answers NO.

Not shown: which line the judge counts as "told", and why it found none in a
capture that holds the claimant's refusal.

**Exit**: the judge's count shown to read the line the guest wrote on a
loaded host, or the red reproduced with its cause.
