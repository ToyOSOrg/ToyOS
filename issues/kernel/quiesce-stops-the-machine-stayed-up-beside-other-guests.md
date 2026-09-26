---
status: open
kind: finding
opened: 2026-09-26
---

# `quiesce_stops_the_machine` stayed up after asking for a reboot, beside other guests

Fast tier at `a58abf50` (PR #511's merged head; another worktree's FAT suite
ran on the host at the same time): `QEMU never reported stopping: the guest
asked for a reboot and stayed up`, after 306 s. Its capture holds the
writers' progress lines and a `usb-storage ... transport broke on SCSI 0x2a:
no answer in the status phase in 2000 ms` that recovered after one break. The
harness's re-run alone was green in 3 s (`stop: 9 of 9 userland thread(s)
stopped across 2 cpu(s) in 29 ms of a 2010 ms budget over 2 sweep(s)`), and so
was `cargo test --test toyos-build -- quiesce_stops_the_machine` alone
afterwards (EXIT=0). `cargo run -- --known-red` answers NO.

Not shown: where the loaded run's stop went, since its capture has no `stop:`
record.

**Exit**: the stop's record, or its absence, explained on a loaded run.
