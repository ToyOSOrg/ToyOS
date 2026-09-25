---
status: open
kind: defect
opened: 2026-09-24
---

# A shutdown on a held USB disk left a CPU deaf to a TLB shootdown for five seconds, and the kernel panicked

One sighting, `usb_transport_break` in a local `--nightly` run on the logd
branch at `eef19bd1` (parameter line
`root=…,usb-transport-break,usb-reset-moves,blackbox=0x8000000`, two CPUs,
TCG). Alone it was green.

cpu1 took the `reboot` job's shutdown into `quiesce` at 1.072 s while the
staged break held disk 0 off its port. Every call it made there ran out of its
operation budget: `vfs: ["boot"] would not sync: would block` at 5.073,
`vfs: ["log"] would not sync: would block` at 7.074, then the censuses. cpu0,
meanwhile, ended a thread (`sys_thread_exit` → `thread_exit` → dropping its
`Unmapped` pages → `arch::tlb::shootdown`) and waited for cpu1's
acknowledgement:

    [kernel 8.088 cpu0 tid=1] PANIC: panicked at src/arch/tlb.rs:171:42:
    tlb: cpu 1 has not flushed for generation Generation(1) in 5000000000ns — it is not taking interrupts

So cpu1 took no interrupt for five seconds somewhere between 3.08 and 8.08 s,
inside the shutdown's sync and flush on a disk that was not coming back. The
panic names no process. The one thread on that boot that ends of its own
accord is logd's network thread, on a boot that gives logd no netd; the logd
branch no longer starts that thread there, which would remove this boot's
trigger and not the deaf CPU.

`a-disk-operation-can-spin-past-the-tlb-ack-tripwire-before-its-break.md` is
the arithmetic for one disk operation outrunning `ACK_TIMEOUT`; this is a boot
that did outrun it, in `quiesce`, across several operations each inside its
own budget. Whether `quiesce` holds `IF` clear between them is not measured.

## Exit condition

What holds cpu1's interrupts off across that window is named from a boot, and
either the shutdown's calls on a held disk take interrupts between them or the
shootdown on another CPU cannot need them then; a staged `usb-transport-break`
shutdown with a thread ending on the other CPU inside the window stays up.
