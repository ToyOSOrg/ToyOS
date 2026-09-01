---
status: open
kind: track
opened: 2026-09-01
---

# There is no attributed session ledger, so seven flake records cannot name what overlapped them

Seven open records describe a test that reds beside other work and is green
alone, or a cost charged to the wrong artifact. Every one of them is blocked on
the same missing observation: **no record joins a guest's loss of progress to
the interval that overlapped it.** They are not seven mechanisms. They are one
instrument, seven times.

- `issues/audio/gate-a-has-no-runner-baseline.md`
- `issues/audio/thorough-tier-reds-on-unmodified-main.md`
- `issues/audio/idle-suspend-reds-on-a-loaded-host-and-on-main.md`
- `issues/boot-media/kernel-log-file-reds-beside-other-guests-and-is-green-alone.md`
- `issues/boot-media/usb-short-read-reds-beside-other-guests-and-is-green-alone.md`
- `issues/build/parallel-tests-red-under-other-suites.md`
- `issues/kernel/syscall-window-nmi-shortfalls-on-a-contended-host.md`

**Not the other two ledgers.** `issues/build/defect-events.md` records where a
defect came from and how far it escaped; the swarm track's metric programme
counts outcomes. This one records host *intervals* — who held what, when — and
is the only one a sighting can be joined against.

**What to build.** An append-only host-side ledger, one file per session,
recording *intervals* rather than samples: monotonic start and end, host
identity, process and job identity, build-lock and guest-slot holder intervals,
image-build spans with their content key and cache hit or miss, QEMU and vCPU
scheduling intervals where the host exposes them, and the guest progress markers
the tests already emit. A sighting is then a join, not an inference.

**What exists, and why each is not the thing.** `tests/common/hostload.rs` (180
lines) records load averages and process counts and is attached to an audio run
— a *sample*, not an interval, so it cannot say what overlapped a window.
`src/buildlock.rs` already names holders (`records_holder`, `guest_slot`,
`build_slot`) but the record is transient: it exists while the guard is held and
is gone when the question is asked. The audio baseline at `tests/toyos.rs:2107`
is keyed on `(test, smp)` and nothing else, so it cannot distinguish the
self-hosted runner's distribution from the developer's — that key has no runner
provenance at all. The committed shard input is per-test duration only, which is
why `issues/build/the-shard-split-prices-a-boot-and-not-the-image-behind-it.md`
charges an image build to whichever test followed it.

**The probe must prove itself inert.** Collection writes files and samples the
host, which is the same class of disturbance the ledger exists to measure. Every
consumer runs probe-off and probe-on arms and attributes to the probe any delta
that begins only with it. High-frequency scheduler sampling is the specific
danger: begin event-driven, from the lock and build markers that are already
free, and bound anything periodic.

**Order.** Build the ledger and the two cheapest consumers first — the shard
pricing and the USB short-read artifact retention — because both have an
existing independent oracle (`Shard::keep`'s partitioning tests; the returned-byte
sweep) and neither needs vCPU visibility. The scheduling-interval consumers come
last, and only on the runner where the host exposes them.
