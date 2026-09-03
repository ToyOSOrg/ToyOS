---
status: open
kind: tooling
opened: 2026-08-20
---

# What W^X's one 4 KiB window per process costs the TLB is unpriced

W^X maps one 2 MiB window per binary at 4 KiB granularity — the window where
`.text` ends and `.data` begins — because at 2 MiB granularity only 48.9 % of
the boot set's text can be write-protected and 0 % of its data can be made
non-executable (measured 2026-08-20 over 20 binaries and 33 windows; the numbers
and the rejected alternatives are in `WindowProt`'s doc comment in
`kernel/src/mm/paging.rs`). The design question is settled by those numbers. The
*cost* is not measured, and this host cannot measure it.

512 4 KiB entries replace one 2 MiB entry for that window, so a program whose hot
loop straddles the `.text`/`.data` boundary pays more dTLB and iTLB pressure
there than it used to. Every other window in every image is still one 2 MiB
entry, so the exposure is bounded to one window per process rather than spread
across the image.

**Why nothing here can price it.** The dev host's guests are TCG, which models
no TLB at all, and a KVM runner reports the host's TLB and not a controlled one.
The instrument is the T14 with `--metal-sim`'s real shape and a workload that
loops across the boundary, comparing against a kernel built before this change.
Nothing depends on the answer today: a bad one does not send the design back to
2 MiB granularity, since 2 MiB granularity does not protect anything. It would
send it to 2 MiB-aligning `toyos-ld`'s segments, which was measured at +4 MiB of
physical memory per process.

**2026-08-25, promoted to `defect`.** Checked at the site: `WindowProt`'s doc
comment in `kernel/src/mm/paging.rs` carries the design measurement — 20
binaries, 33 windows, 20 mixed, 48.9 % and 0 % — and says nothing about what the
512 4 KiB entries cost the TLB, so the number lives nowhere in the tree. That
makes it a measurement owed, in the same class as the AP control-register delta
and the AP TSC trail, and it belongs on the metal session's table with them
(`issues/hardware/a-metal-session-runs-a-pre-flash-gate-first.md`). Owed by
whoever prepares that session: it needs a boundary-straddling workload and a
kernel built before the change to compare against.
