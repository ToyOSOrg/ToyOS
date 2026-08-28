# Kernel

The module header at the site owns its subsystem — read it before changing a module. The scheduler core is `toyos-sched/`, driven from `kernel/src/sched/`; every Ring 3 transition's machine state is `kernel/src/arch/fpu.rs`; every syscall is `kernel/src/arch/syscall/`, and `dispatch.rs` decodes every user pointer the ABI takes.

## Caveats that bite every agent

- **Nothing on the idle path touches a filesystem.** No gate holds this.
- **Anything added to the idle loop is an audio change** — housekeeping runs before `pass()`, so a woken CPU is late by what it costs; and on a machine with nothing to run the idle loop does not run, so a diagnostic there reports nothing exactly when it is needed.
- **The idle loop may not take a global lock unconditionally** — the crash report reads the process table through a `try_lock` it must never block on. `sched/reap_gate.rs` is the pattern: a relaxed-load gate in front of the lock.
- **Every Ring 0 entry clears the direction flag** — no hardware gate does it, and `memmove` sets it across `rep` operations with interrupts on; `arch::entry::ring3_naked_asm` is where it lives.
- **`BackendGuard` masks interrupts for its whole life**, so anything written under it is an interrupt latency; a new holder bounds itself as the console drain and the userland `write` flush do.
- **No disk wait in this kernel can park** — at the moment a transfer is waited for, the CPU is four ticket spinlocks deep, each disabling preemption.
- **`crate::log!` may not be called inside `with_cpu`'s exclusive region unless a `panic!` follows it** — the log's readiness path re-enters `driver::pass` and wedges the machine.
- **`drain_irqs` is the drivers' engine and nothing on it may wait** — a blocking call there empties the audio pipeline on every plug.
- **A syscall that can block resolves its handle and clones the object out before it blocks** — a `with_object`/`with_process_data` guard held across a park is a runtime panic no compile check catches; the `SYS_FSYNC` arm in `arch/syscall/dispatch.rs` is the pattern.
- **A block-layer `BudgetExpired` is not-durable-yet and never a loss** — it is retried on a fresh budget above every lock; a flush that discards its pages on one splits a FAT mirror.
- **A decision the process table makes lives in `toyos-proclife`, never in `process.rs`** — its defects are interleavings and that crate is the only machine that can enumerate one.
- **A task holds at most one completion arm** — a standing arm across a loop must not call anything that arms again. A double-arm panics only at attempt ≥ 2, so the contention depth is the coverage.
- **A console is per holder, minted at spawn** — the object *is* the line buffer; `console_line_atomicity` is the gate.
- **`ops::close` cancels a poll only for a source its object really ends** — `cancel_by_source` walks every ring in the machine; `ops::ends_its_sources` is where a new object kind answers.
- **A page shared with userland is never reached through a Rust reference** — the words a protocol shares are `&AtomicU32` one at a time, everything else is a volatile copy of the whole value, and a page is laid out *before* it is mapped (`SharedMemObject::phys_before_mapping`). The kernel, `toyos-abi` and the SDK each hold one end of this rule.
- **A DMA pool a device claim publishes contains buffers only** — a descriptor's `addr` is a physical address the device dereferences, so a claimant that can rewrite one holds an arbitrary write. No gate holds this; `virtio_net`'s `assert_queues_are_private` is the pattern for stating it at bind.
- **A bounds-checked accessor whose refusal panics inline is not inlined** — put the refusal in a `#[cold] #[inline(never)]` helper; and measure the emitted assembly before choosing a runtime check over a zero-sized witness.
- **Pressing Ctrl+Alt+D destroys the evidence it reports on** — capture `info registers -a` over QMP *first*; `kernel/src/sched/dump.rs` explains the report.
