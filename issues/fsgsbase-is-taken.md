---
status: open
kind: track
opened: 2026-09-29
---

# FSGSBASE is taken

`CR4.FSGSBASE` is forbidden (`CR4_FORBIDDEN`, `kernel/src/arch/x86_64/control_regs.rs`):
the switch moves no thread's GS base, since every GS load Ring 3 can make
leaves it zero, so a `WRGSBASE` would reach the next thread on the CPU.
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
guidance asks; NMI, `#MC` and `#DF` install the kernel's base from `GDTR`
whatever GS holds, and the NMI returns to the base it found. The
`nmi-user-gs` actuator enters vector 2 at CPL 0 under a decoy kernel-half
base, which a sign test of `IA32_GS_BASE` would take for the kernel's.

**Left**: `CR4.FSGSBASE` on every CPU, and the switch saving and restoring
each thread's GS base beside its FS base.

**Owed on metal** (#1262; the benches were down when it was reviewed). On
the T14 and the AMD bench, main must show:
- `debug_trap`, on the `testcases` boot: every arm's child ends as
  `Fault::Trap` and the machine stays up. That includes `tf-mov-ss-syscall`,
  `tf-mov-ss-int3`, `tf-mov-ss-int1` and `tf-int1`. On the AMD bench these
  are the CPL-0 path of the `#DB` entry, which a single-step trap held over
  a `MOV SS` reaches (APM Vol. 2 §8.1.4) and the guest runner's Intel part
  never takes.
- `gs_segment_load`, on the `testcases` boot: all 12 selectors reported.
- `nmi_finds_the_kernels_gs`, on the `shared-debug` boot: `nmi-user-gs: PASS`.
- `syscall_cost`, on the `testcases` boot: within its bound.

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
