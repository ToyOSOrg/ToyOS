---
status: open
kind: defect
opened: 2026-09-08
---

# A loaded host hangs a boot after the reboot spawn, and the boot deadline does not fire

**The T14's hang has a QEMU reproduction.** Run 19 hung three times and run 22
once, always with the kernel log's last line the spawn that follows a job list's
end; run 22 sat past 420 s with `boot-deadline=120000` armed and the deadline
never fired. The same shape reproduces on this host, on `metal-suite`'s own tip,
with nothing staged:

    cd <a metal-suite worktree>
    for n in $(seq 14); do (yes > /dev/null &); done      # 14 cores of load
    cargo test --test toyos-build boot_deadline_ends_a_wedge -- --nightly

**3 of 4 red under that load; 12 of 12 green without it** (tree 43524d3f, the
base of `metal-suite-lockup`, dev host, 14 cores). Every red is the same: the
guest's last word is

    spawn: /system/bin/reboot pid=6 tid=0 dst=0 ... total=153ms

and then nothing at all for the harness's whole drain — 66 to 86 s of wall
clock, against a 15 s `boot-deadline` the same boot logged itself arming. The
loader's next pass never runs, because the machine never resets.

Not the load alone: the guest's bound is TSC-derived and the TSC under TCG
tracks host time, so a boot merely running slowly still reaches 15 s of it and
still takes the timer interrupt that polls the bound. **A deadline that does not
fire is every CPU not taking an interrupt**, which is the state
`kernel/src/hardlockup` was built for — and it cannot be the instrument here,
because CPUID states no architectural performance counter on a TCG guest, so
nothing samples those CPUs (the boot's own arm line says so).

One red also carried, two seconds before the silence:

    usb-storage: 00:02.0 slot 1 transport broke on SCSI 0x28: no answer in the data phase in 2000 ms
    xHCI: 00:02.0 slot 1 endpoint 3 is Running, recovering
    usb-storage: read of 1 blocks at 10882 ran out of its operation budget on disk 0

which is the path the metal track already suspects (run 20's
`LOCK CONTENTION: 200M spins at src/vfs.rs:32` under stick I/O, and run 22's
stick carrying no kernel log file at all). Whether the two reds share one cause
is unmeasured: the other three carried no such line.

**Exit condition**: a red under this reproduction whose stuck CPU is named —
its `rip`, and what it holds or waits for. Two ways to get one, neither free:
a host where the guest has a performance counter (there is no x86 KVM on this
Mac), or the T14, where `hard-lockup-probe`'s metal arm proves the counter and
an unstaged boot would then name a real one.
