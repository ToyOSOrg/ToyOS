---
status: open
kind: track
opened: 2026-09-29
---

# FSGSBASE is taken

Intel names what it gives, user programs' FS and GS bases without a system
call and a more efficient kernel context switch, and has it used "never in
production environments unless accompanied by a complete OS implementation":
one whose context switch and process-trace system calls manage the FS and GS
bases apart from their selectors, and whose ring transitions are insensitive
to the GS base (Guidance for Enabling FSGSBASE, ID 767615). AMD documents the
four instructions, enabled by CR4.FSGSBASE at every privilege level, and
recommends nothing on their use (APM 24594 rev. 3.36, Vol. 3, RDFSBASE and
WRFSBASE). ToyOS takes it under Intel's condition: its switch saves and
restores each thread's FS and GS bases as the thread left them, its ring
transitions are insensitive to the GS base they find, and it has no system
call that reads or writes another thread's registers
(`sdk/abi/src/syscall.rs`). Its figure is reported and decides nothing.

**Built**: the ring transitions (`kernel/src/arch/x86_64/entry.rs`). Every
gate but the IST ones swaps GS by its frame's `CS`, fenced as Intel's SWAPGS
guidance asks; NMI, `#DB`, `#MC` and `#DF` install the kernel's base from
`GDTR` whatever GS holds, and the NMI and `#DB` return to the base they
found. The
`nmi-user-gs` actuator enters vector 2 at CPL 0 under a decoy kernel-half
base, which a sign test of `IA32_GS_BASE` would take for the kernel's.
`CR4.FSGSBASE` is required of every CPU (`kernel/src/arch/x86_64/control_regs.rs`),
and the switch carries each thread's FS and GS bases
(`kernel/src/arch/x86_64/hw.rs`); `fsgs_bases_per_thread` reads them back
across yields on one CPU.

**Left**: the exit's runs: the vector-2 arm and the readback on each proving
machine and under TCG, and the T14's pipe round trips with and without
FSGSBASE.

**Owed on metal** (#1262; the benches were down when it was reviewed). On
the T14 and the AMD bench, main must show:
- `debug_trap`, on the `testcases` boot: every arm's child ends as
  `Fault::Trap` and the machine stays up, `tf-mov-ss-syscall`,
  `tf-mov-ss-int3`, `tf-mov-ss-int1` and `tf-int1` among them.
- For `tf-mov-ss-int1` and `tf-int1`, which path ended the child, read off
  the kernel's `FAULT rip=… rsp=…` line for it and recorded here. Either is a
  pass. `rip` and `rsp` 0 mean a held single-step landed on the `#DB`
  entry's first instruction and its CPL-0 path rewrote the frame; the
  user's address after the `INT1` means `INT1`'s own `#DB` ended it. A CPU
  may hold a single-step a `MOV SS` delays over the `INT1` (APM Vol. 2
  §8.1.4 documents the delay, not whether `INT1`'s `#DB` absorbs it), and
  nothing documents whether one is pending after `INT1` without one.
- `gs_segment_load`, on the `testcases` boot: all 12 selectors reported.
- `nmi_finds_the_kernels_gs`, on the `shared-debug` boot: `nmi-user-gs: PASS`.
- `syscall_cost`, on the `testcases` boot: within its bound.

**Owed on metal** (#1264, the same way). On the T14 and the AMD bench, main
must show:
- `fsgs_bases_per_thread`, on the `testcases` boot: both threads read back
  their own bases, and both crossing counts are above 0.
- The rows above, through the entries' `RDGSBASE` and `WRGSBASE`.
- An A/B of main before and after #1264 on the T14's `benchcase` boot:
  `pingpong.pipe.*`, reported. It decides nothing.

**Unobserved**: the `#DB` entry's resume into a gate it interrupted, where
a single-step held over a `MOV SS` lands on the first instruction of
`SYSCALL`'s entry or of the `#BP` gate (`tf-mov-ss-syscall`,
`tf-mov-ss-int3`), has run on no machine that could show it. Those arms end
the same way whether or not it ran, and the resume logs nothing, so no row of
main tells. **Its exit**: a CPU that holds the trap shows the resume ran,
through something main reports — a count of resumed CPL-0 `#DB`s that the
kernel logs and `debug_trap`'s judge reads after those arms, say.

**Exit**: on every proving machine and under TCG, a `boot-actuators` arm
enters vector 2 at CPL 0 while the user GS base is live, between entry and its
`swapgs` or after the exit path's, and the handler finds the kernel's per-CPU
data; a thread's FS and GS bases, written with WRFSBASE and WRGSBASE, read
back unchanged after its CPU has run another thread; `CR4_FORBIDDEN` is
deleted, and so is `gsbase_locked` with its `user-writable-gsbase` kernel from
stage C of `issues/the-guest-suite-runs-only-what-no-cheaper-tier-reaches.md`;
the T14's pipe round trips read with and without FSGSBASE. **Mutation**: an
IST entry that decides `swapgs` from the saved CS alone. **Oracle**: the
vector-2 arm on the T14.
