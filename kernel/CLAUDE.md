# Kernel

The module header at the site owns its subsystem — read it before changing a module. The scheduler core is `kernel/pure/sched/`, driven from `kernel/src/sched/`; every user transition's machine state is `arch/*/fpu.rs`, with AArch64's `entry.rs` and `trap.rs`; every syscall is `kernel/src/syscall/`, and `dispatch.rs` decodes every user pointer the ABI takes.

## Caveats that bite every agent

- **Nothing on the idle path touches a filesystem.** No gate holds this.
- **Anything added to the idle loop is an audio change** — housekeeping runs before `pass()`, so a woken CPU is late by what it costs; and on a machine with nothing to run the idle loop does not run, so a diagnostic there reports nothing exactly when it is needed.
- **The idle loop may not take a global lock unconditionally** — the crash report reads the process table through a `try_lock` it must never block on. `sched/reap_gate.rs` is the pattern: a relaxed-load gate in front of the lock.
- **Every x86 Ring 0 entry clears the direction flag, and every interrupt or trap gate `AC`** — no hardware gate does either; `memmove` sets `DF` across `rep` operations with interrupts on, and Ring 3 sets `AC`, under which SMAP lets Ring 0 reach user pages. `arch::entry::ring3_naked_asm` is where they live; `SYSCALL` clears `AC` through `IA32_FMASK`.
- **`BackendGuard` masks interrupts for its whole life**, so anything written under it is an interrupt latency; a new holder bounds itself as the console drain does.
- **A syscall that can block resolves its handle and clones the object out before it blocks** — a `with_object`/`with_process_data` guard held across a park is a runtime panic no compile check catches; the paged-object reads in `syscall/paged.rs` are the pattern.
- **A decision the process table makes lives in `kernel/proclife/`, never in `process.rs`** — its defects are interleavings and its host tests are the only machine that can enumerate one.
- **A task holds at most one watch registration** — a standing registration across a loop must not call anything that registers again. A double registration panics only at attempt ≥ 2, so the contention depth is the coverage.
- **`ops::close` cancels a poll only for a source its object really ends** — `Watch::cancel_polls` answers every ring's poll on that watch; `ops::close_ends_polls` is where a new object kind answers.
- **A page shared with userland is never reached through a Rust reference** — the words a protocol shares are `&AtomicU32` one at a time, everything else is a volatile copy of the whole value, and a page is laid out *before* it is mapped (`SharedMemObject::write` before `AddressSpace::map`). The kernel, `toyos-abi` and the SDK each hold one end of this rule.
- **A device a process drives reaches only the memory its grants map** — the domain is the gate, not the descriptor; a device address outside a grant is a `DMA FAULT` record and never a crash.
- **A bounds-checked accessor whose refusal panics inline is not inlined** — put the refusal in a `#[cold] #[inline(never)]` helper; and measure the emitted assembly before choosing a runtime check over a zero-sized witness.
