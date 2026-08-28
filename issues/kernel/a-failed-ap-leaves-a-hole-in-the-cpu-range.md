---
status: open
kind: defect
opened: 2026-08-28
---

# boot_aps spends a cpu id before the AP starts, so a failed AP leaves a hole in 0..CPU_COUNT that the next TLB shootdown panics on

# `boot_aps` spends a cpu id before the AP starts, so a failed AP leaves a hole in `0..CPU_COUNT` that the next TLB shootdown panics on

## The hole

`boot_aps` assigns the cpu id and publishes the LAPIC mapping before the AP has
executed an instruction:

- `kernel/src/arch/smp.rs:201-203` — `ap_cpu_id = next_cpu_id`, `next_cpu_id += 1`,
  `CPU_APIC_IDS[ap_cpu_id].store(ap_id)`, all before the INIT/SIPI at `smp.rs:220-228`.
- `kernel/src/arch/smp.rs:242-247` — `CPU_COUNT.fetch_add(1)` only under `AP_STARTED`;
  the else arm logs `failed to start!` and the loop moves on with `next_cpu_id`
  already spent.

Nothing rolls it back and nothing renumbers: `CPU_COUNT` is written in exactly two
places (`smp.rs:20`, `smp.rs:243`) and `CPU_APIC_IDS` in exactly two (`smp.rs:184`
for the BSP, `smp.rs:203`). So when a MADT entry that is *not* the last one fails
and a later one succeeds, the online set is not `0..CPU_COUNT`: three APs whose
first one fails leaves live ids `{0,2,3}` with `cpu_count() == 3`.

The Budget at `smp.rs:231-234` declares the degradation as "the AP is named as
failed to start and the machine boots one CPU short". That is what the code
actually delivers only when the failing AP is the last MADT entry.

`apic_id_for` (`smp.rs:31-34`) asserts only `cpu_id < cpu_count()`, so the phantom
id passes the one check there is and returns the dead AP's LAPIC id.

## What the hole costs

`arch::tlb::shootdown` walks `0..smp::cpu_count()` and waits on every id
(`kernel/src/arch/tlb.rs:36-50`). `wait_for` (`tlb.rs:55-75`) spins on
`Shootdown::served`, which is `flushed[cpu] >= generation`
(`kernel/src/shootdown.rs:71-74`), and every writer of `flushed[]` — `serve`,
`serve_if_owed`, `join` at `tlb.rs:78-102` — indexes by `percpu::cpu_id()` on a
CPU that is actually executing. No physical CPU carries the phantom id, so
`flushed[phantom]` stays zero, `wait_for` reaches `ACK_TIMEOUT` (5 s,
`tlb.rs:24-27`) and `panic!`s at `tlb.rs:66-70` naming a CPU that never existed.

`SIBLINGS_ANSWER` does not rescue this. It covers the opposite case — an AP that
*is* counted, is spinning on `SMP_READY` with `IF` clear, and will settle its
arrears through `join()` (`smp.rs:36-41`, `tlb.rs:18-20`, `tlb.rs:99-107`,
`smp.rs:268-274`). A phantom id never reached `ap_entry`, so it never calls
`join`. Once `smp::set_ready()` runs (`kernel/src/main.rs:527`) the `cpus <= 1`
early-out at `tlb.rs:37` is gone too, and the first shootdown kills the machine.
The callers are ordinary work: unmap from a syscall
(`kernel/src/arch/syscall/vm.rs:216`), process teardown
(`kernel/src/process.rs:600`), `kernel/src/mm/paging.rs:356` and `:903`,
`kernel/src/mm/unmapped.rs:19`. An unprivileged process does not create the hole,
but any one of them converts it into a kernel death.

It is already wrong before that. `sched::driver::init` builds a `CpuSched` for
every id in `0..cpu_count()` (`kernel/src/sched/driver.rs:349-367`), and
`CpuHandles::place` picks the least-loaded CPU that is `answering`
(`toyos-sched/src/cpu.rs:2338-2349`); `answering` is true while no kick is
pending (`cpu.rs:2241-2247`), so a phantom publishing load 0 is the *preferred*
placement target until it owes a message and goes stale — and a thread placed
there never runs. `apic::kick_cpu` and `send_nmi`
(`kernel/src/arch/apic.rs:121-133`) resolve the phantom through `apic_id_for`
and IPI the dead core's real LAPIC id.

The only tell in between is a log line: `heartbeat` reports
`cpu{n} has never reached a scheduler pass` (`kernel/src/heartbeat.rs:100-113`),
which is actuator-gated and never fails anything.

## The trampoline the failed AP may still be reading

One `TrampolineData` is built once (`smp.rs:187`) and rewritten in place at
physical `0x8F00` on every iteration (`smp.rs:206-210`). Its `SAFETY` note at
`smp.rs:209` states the precondition — "the previous AP already signaled
`AP_STARTED` before this loop wrote again, so no CPU is reading it" — and the
timeout `break` at `smp.rs:236-239` is exactly the path that does not establish
it: on expiry control falls through to the next MADT entry, `smp.rs:212` resets
the flag, and `smp.rs:210` rewrites the page with no proof the previous AP has
left it.

Only three fields differ between iterations — `stack_top`, `entry`, `percpu_ptr`
— and `entry` is always `ap_entry`, so every other field is rewritten
byte-identically and the exposure is precisely `stack_top` (read by the
trampoline at `smp.rs:350`) and `percpu_ptr` (read at `smp.rs:369`). An AP that
is slow rather than dead, and that reads them after the overwrite, comes up on
the *next* AP's stack and with GS pointing at the next AP's `PerCpu` — the block
`percpu::alloc_ap` handed out for one consumer
(`kernel/src/arch/percpu.rs:595-604`) and the one `percpu::cpu_id` reads back
from (`percpu.rs:636-638`). If that next AP then boots normally, two physical
cores run concurrently on one 64 KiB stack and one `PerCpu`.

The same window mis-attributes the flag, which is a second route into the hole:
the late AP's `AP_STARTED.store(true)` at `smp.rs:266` lands after iteration
N+1's reset at `smp.rs:212`, so `smp.rs:242-244` counts and logs cpu N+1 as
online on the strength of cpu N's signal.

## Precondition

No hostile actor, no userland input, no device. The trigger is an AP that does
not reach `smp.rs:266` inside the 10 ms + 1 ms + 100 ms of `smp.rs:215-239`
while a later MADT entry does: a defective core the firmware still reports
Enabled (`kernel/src/drivers/acpi.rs:542-544`), or a vCPU thread descheduled
past the budget on a loaded host.

**Not observed here.** QEMU starts every `-smp` CPU, so no test in this tree has
ever taken the failure arm at `smp.rs:245-247`. Staging the hole needs an
injection that skips the INIT/SIPI for one non-last MADT entry and then takes a
shootdown after `set_ready`; staging the trampoline arm needs one that delays a
non-last AP past the budget but leaves it running.

## Fix direction

Make `0..cpu_count()` the live set by construction, which is what
`tlb::shootdown` (`tlb.rs:46`) and `sched::driver::init` (`driver.rs:350`)
already assume: leave `next_cpu_id` and `CPU_APIC_IDS[ap_cpu_id]` uncommitted
until `AP_STARTED` is observed, or roll both back on the failure arm. Bound the
same loop while there — `smp.rs:203` indexes an eight-entry array
(`smp.rs:23-24`, `kernel/src/sched/mod.rs:12`) with an unbounded count from the
MADT, and `parse_madt` caps nothing (`kernel/src/drivers/acpi.rs:520-577`), so a
machine reporting nine enabled CPUs panics on that store long before
`driver.rs:351`'s `assert!(count <= MAX_CPUS)` can say so. And make the failure
arm establish the `SAFETY` note at `smp.rs:209` rather than assert it: either
give each AP its own data block so no page is ever reused, or hold the page
until the previous AP is provably out of the trampoline.

The check this owes: a negative control that fails without the fix — inject a
non-last AP that never starts, then take one shootdown after `set_ready` and
assert the machine survives — and an oracle independent of this kernel's own
model, since the acknowledgement protocol is already mirrored in
`kernel-loom/` (`kernel/src/shootdown.rs:1-12`) and the phantom id is a
participant that model can be given.
