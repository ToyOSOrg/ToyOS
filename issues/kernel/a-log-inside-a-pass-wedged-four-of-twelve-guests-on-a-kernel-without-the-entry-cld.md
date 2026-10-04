---
status: open
kind: tooling
opened: 2026-10-04
---

# A log inside a pass wedged four of twelve guests on a kernel without the entry `cld`, and that tree was never measured again

Owner: the orchestrator.

## The recorded failure

4ff5221ee's first draft of the `heap-tripwire` depth ladder logged each new
high-water rung from `stack_depth`, inside `with_cpu`'s exclusive region.
Twelve-wide TCG boot storm, six minutes: four guests wedged, every one in the
spawn burst with the previously logged rung as its last line, all recorded as
hangs. The landed version, the same `sched-tripwire heap-tripwire` kernel
without the log, ran 7,205 boots with 0 deaths. Its commit
blamed "the log's readiness path", which `log::emit` does not reach, then or now
(`log::user::post_readiness` has one caller, `klogd`'s loop). No capture of the
four guests' registers was kept.

That kernel had no `cld` on any Ring 0 entry and `IA32_FMASK` was `0x40200`.
9cc5ad28f, the same day and later, found that a set direction flag inherited
from `memmove`'s `std` window makes every `rep movs`/`rep stos` write backwards.
All 37 deaths of its two unfixed arms (13,960 boots) had `DF` set on a vCPU,
and 25 of them printed nothing. Its own reader's first draft died formatting a
report under a set `DF`.

## Measured at 791367452

The first draft's probe (`log!` when `DEEPEST` rises, with an
`assert!(in_pass())`), twelve guests, `-smp cores=2`, `-m 2G`, q35, TCG,
`bootable.img` shape with the vIOMMU, `snapshot=on`, `compositor: ready` as
completion, `-action reboot=shutdown -action shutdown=pause`, 120 s ceiling,
45 min an arm, host load average 30 to 60 on 14 cores:

| arm | kernel features | probe | boots | deaths | with `DF` set at capture |
|---|---|---|---|---|---|
| H | `heap-tripwire` | yes | 2,475 | 0 | — |
| U | `heap-tripwire entry-df-unclean` | yes | 1,764 | 10 (4 panic, 5 parked, 1 hang) | 9 |
| N | `heap-tripwire entry-df-unclean` | no | 2,095 | 6 (2 panic, 3 parked, 1 hang) | 5 |

H logged from inside the pass 7,334 times. Under N's rate it expects 7.1
deaths and has none (Poisson p = 8.3e-4); under U's, 14.0 (p = 8.1e-7). U's
hang is in the spawn burst with a vCPU at `RFL=0x486`. N's hang is in the
loader, before the kernel. U against N does not separate (10 of 16, conditional
binomial p = 0.14), so at this tree the log adds nothing measurable to the
direction-flag class.

## What is still unexplained

Why the log took 4ff5221ee's tree from 0 deaths in 7,205 boots to four hangs
in six minutes. A log formats into a stack buffer and copies with `rep movs`, so
one emitted in a pass entered with `DF` set writes backwards over the stack.
That fits the record but has not been measured on that tree.

## Exit

Storm 4ff5221ee's tree with the first-draft probe, keeping each death's
`info registers -a`. Then storm it again with 9cc5ad28f's `arch::entry` `cld`
and `0x40600` mask applied. Delete this file if the hangs show `DF` set and the
second arm has none. If they do not, file what the captures show as a `defect`.
