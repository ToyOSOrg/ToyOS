# Instruments for the blocked defect set

This document audits the 24 rows marked `BLOCKED` in `PLAN.md` at
`72f0218f6cc05d3278920911b9ed09bf3db68565` against the current tree at
`273a321cc44e203bb441976f8a59c4586319c4ac`.  Both object names were verified as
commits before use.  The result is **21 of 24 unblocked through 12 instrument
families, with 3 not worth building now**.  “Worth” means the proposed observation can
discriminate a repair without merely restating the implementation under test.

## Tiers and cost notation

The tier is the strongest available independent observation: **T1** existing
in-tree checker or model differential; **T2** existing second implementation,
QMP observation, or kernel counter; **T3** new harness probe; **T4** new actuator.
Costs are estimates: **S** is a focused host-test change, **M** a reusable harness
or pure-state component, and **L** cross-subsystem model or actuator work.

## 1. One attributed session ledger — 8 defects

The shared instrument is an append-only host-side session ledger.  It records
monotonic start/end times, host identity, process and job identity, build-lock
holder intervals, QEMU/vCPU scheduling intervals where available, image-build
spans and cache keys, and guest progress markers.  The current audio baseline is
keyed only by test and SMP (`tests/toyos.rs:2107-2148`,
`tests/toyos.rs:2163-2187`); the existing host sample records load averages and
process counts (`tests/common/hostload.rs:1-90`) and is attached to an audio run
(`tests/toyos.rs:2204-2206`), but it is not an interval ledger.  Build and guest
slot records already name holders (`src/buildlock.rs:237-330`) and the committed
shard input is only per-test duration (`tests/toyos.rs:15470-15538`).

#### issues/audio/gate-a-has-no-runner-baseline.md — WORTH

- **Blind spot:** the committed baseline cannot answer how the self-hosted runner's distribution differs from the developer distribution because its key has no runner provenance (`tests/toyos.rs:2107-2148`).
- **Instrument:** add a baseline collection mode backed by the session ledger; read raw WAV harm, existing audio counters, host identity, vCPU intervals, and the exact tree/image key. **T2** because WAV harm and guest counters already provide independent outcomes.
- **Perturbation:** collection itself adds file writes and sampling; run no-probe/probe alternation and compare WAV/counter distributions, while attributing any delta that begins only with the probe to the probe.
- **Cost and reuse:** **M**; reused by all eight defects in this family and future flaky-test adjudication.
- **Judgment:** worth building; it replaces cross-host inference with a same-runner population.

#### issues/audio/thorough-tier-reds-on-unmodified-main.md — WORTH

- **Blind spot:** raw audio harm exists, but the tree cannot associate the zero-dropout and dropout modes with host scheduling, device, image, and revision in the same session (`tests/toyos.rs:2163-2187`, `tests/toyos.rs:2642-2703`).
- **Instrument:** alternate historical-base and current-tree arms under one ledger session, recording the existing WAV verdict and guest audio counters alongside host/vCPU intervals. **T2**.
- **Perturbation:** alternating builds can change cache warmth; record cache keys and counterbalance arm order so an order-only effect is distinguishable from a revision effect.
- **Cost and reuse:** **S** after the ledger exists; reuses the Gate-A baseline collector.
- **Judgment:** worth building; it directly separates revision, runner mode, and warm-cache effects.

#### issues/boot-media/kernel-log-file-reds-beside-other-guests-and-is-green-alone.md — WORTH

- **Blind spot:** the test proves the boot transcript contains logd output and later polls the device, but does not retain the interval in which logd, mount, or observation stopped progressing (`tests/common/volumes.rs:498-550`).
- **Instrument:** repeat the existing test in ledger sessions and timestamp its boot, mount, logd, poll, and device-read markers; read the final on-device log as the outcome. **T2**.
- **Perturbation:** extra timestamp lines may alter serial timing; emit host timestamps around existing reads first, and require the failure rate not to change between probe-off and host-only-probe arms.
- **Cost and reuse:** **S** after the ledger; shared with USB and parallel-suite adjudication.
- **Judgment:** worth building; a rate plus phase ownership is the missing discriminator.

#### issues/boot-media/usb-short-read-reds-beside-other-guests-and-is-green-alone.md — WORTH

- **Blind spot:** the test stages verdicts and performs a returned-byte sweep, then removes the image, so the failure has neither a session-wide contention record nor a retained failing artifact (`tests/common/usb.rs:356-426`).
- **Instrument:** ledger the existing stages and returned-byte counts and retain the image only on failure, identified by content hash. **T1** because the existing final byte sweep is an independent oracle.
- **Perturbation:** retaining every artifact would change disk pressure; retain only after the verdict and compare probe-off with metadata-only runs.
- **Cost and reuse:** **S** after the ledger; artifact retention is reusable by media flakes.
- **Judgment:** worth building; no driver theory should be chosen before the rate and failing bytes are known.

#### issues/build/parallel-tests-red-under-other-suites.md — WORTH

- **Blind spot:** the umbrella record cannot join a particular guest's loss of progress to the other job or resource interval that overlapped it; current slot ownership is transient (`src/buildlock.rs:237-330`).
- **Instrument:** join every test marker to guest-slot, build-slot, QEMU, image-build, and host-scheduling intervals in the session ledger, then split sightings by attributed signature. **T3**.
- **Perturbation:** high-frequency sampling can itself create contention; begin with event-driven lock/build markers, bound optional scheduling samples, and compare the uninstrumented wall-clock and failure rate.
- **Cost and reuse:** **M**; this is the principal cross-suite reuse case for the ledger.
- **Judgment:** worth building; the issue is not safely actionable as one mechanism without it.

#### issues/build/the-shard-split-prices-a-boot-and-not-the-image-behind-it.md — WORTH

- **Blind spot:** the shard planner consumes committed per-test durations and cannot charge image construction or a cache miss to the artifact rather than the following test (`tests/toyos.rs:15470-15538`).
- **Instrument:** ledger image-build start/end, content key, cache hit/miss, boot start, and test start/end, then derive shard costs from nonoverlapping spans. **T2** because filesystem artifact identity and the build transcript are independent of the pricing code.
- **Perturbation:** hashing large images can become part of the cost; reuse the build's content key or hash outside the measured interval and compare accounted time with session wall time.
- **Cost and reuse:** **S** after the ledger; reusable by every shard and cache report.
- **Judgment:** worth building; otherwise weight changes merely move unattributed cost.

#### issues/kernel/syscall-window-nmi-shortfalls-on-a-contended-host.md — WORTH

- **Blind spot:** the harness now detects the guest-side parked-victim signature and can declare host starvation (`tests/common/faults.rs:538-555`), but it still cannot correlate that signature with actual host vCPU descheduling.
- **Instrument:** add the NMI window and victim progress markers to the session ledger and sample the target QEMU thread's host scheduling intervals. **T2**.
- **Perturbation:** scheduler sampling changes the schedule; use coarse bounded sampling, retain the existing guest-only verdict as the control, and require the same signature without sampling before blaming the host.
- **Cost and reuse:** **S** after the ledger; reuse for every timeout that distinguishes guest work from host descheduling.
- **Judgment:** worth building, but no longer a prerequisite to recognize the already-modelled starvation shape; the plan's premise has drifted.

#### issues/audio/idle-suspend-reds-on-a-loaded-host-and-on-main.md — WORTH

- **Blind spot:** the guest owns an IRQ/pickup split, but its counters contain only guest observations (`tests/common/audio.rs:408-450`) and the nearby host-sensitive clock values are explicitly printed rather than asserted (`tests/common/audio.rs:452-461`), so no record says whether the vCPU ran or the host remained in an idle state during the slow mode.
- **Instrument:** on the affected runner, add bounded per-vCPU scheduling and CPU-idle residency snapshots to the same session ledger, aligned with existing guest markers. **T2**.
- **Perturbation:** polling residency can wake the host; prefer cumulative counters at phase boundaries and reject the probe if its own CPU-idle deltas differ materially from an unprobed arm.
- **Cost and reuse:** **M**; reuses the ledger and benefits all loaded-host audio defects.
- **Judgment:** worth building; guest attribution alone cannot select a host-versus-guest fix.

## 2. External total-freeze actuator — 1 defect

#### issues/diagnostics/blocked-dump-cannot-fire-on-a-total-freeze.md — WORTH

- **Blind spot:** the request and service path runs only through `drain_irqs`, so every current trigger presupposes a CPU that can schedule that path (`kernel/src/sched/driver.rs:651-667`).
- **Instrument:** a host QMP controller freezes schedulable progress, injects the architecture's NMI/dump signal, captures registers and serial/panel output, and enforces a deadline. **T4** with **T2** QMP/register evidence.
- **Perturbation:** QMP stop itself can prevent the handler; separately test “busy but interruptible” and “QMP-stopped” arms and accept only the actuator whose register trace proves delivery.
- **Cost and reuse:** **L**; reusable for total-freeze diagnostics and panic fallback tests.
- **Judgment:** worth building; it reaches a state no guest workload can certify.

## 3. Doom/resume Gate-A profile — 1 defect

#### issues/audio/desktop-session-put-26ms-of-silence.md — WORTH

- **Blind spot:** the mixer computes the future `target` but records stale `t_est` in `armed_on` (`userland/soundd/src/mix.rs:254-273`) and later attributes both lateness halves to that stale instant (`userland/soundd/src/mix.rs:367-378`), while no committed workload reproduces doom starvation and the 44-resume history with both values visible.
- **Instrument:** add a bounded Gate-A profile that drives doom plus the recorded resume sequence and records raw WAV, producer periods, resume count, `target`, and `t_est`. **T2**.
- **Perturbation:** trace emission can disturb the mixer; write fixed-size records to an existing buffer, compare WAV with tracing disabled, and identify a probe effect when only traced runs cross the harm bound.
- **Cost and reuse:** **M**; reusable for mixer resume and starvation regressions.
- **Judgment:** worth building; it makes the two competing mechanisms distinguishable.

## 4. xHCI completion and idle-residency trace — 1 defect

#### issues/hardware/xhci-waits-are-spins.md — WORTH

- **Blind spot:** both command and transfer waits spin (`kernel/src/drivers/xhci/wait/mod.rs:260-323`), and the existing depth probe (`kernel/src/drivers/xhci/wait/mod.rs:13-38`) cannot say whether a completion-based replacement releases CPU time without violating an upper-lock context.
- **Instrument:** record wait entry/exit, lock-context/depth, completion event, and host/guest idle residency around the real wait primitive. **T2**.
- **Perturbation:** per-iteration logging would lengthen the spin; log only state transitions into a fixed buffer and compare completion latency with the probe compiled out.
- **Cost and reuse:** **M**; reusable for storage/audio wait attribution.
- **Judgment:** worth building; it can prove both progress and reclaimed CPU time after transaction boundaries are safe.

## 5. Allocation/alias lifetime harness — 2 defects

#### issues/design-debt/kernelslice-outlives-its-allocation.md — WORTH

- **Blind spot:** `KernelSlice` is `Copy` and carries only an address and length, so the type cannot express that its backing allocation still exists (`kernel/src/mm/region.rs:1-29`, `kernel/src/mm/region.rs:54-79`).
- **Instrument:** first make the compiler the oracle by giving `KernelSlice` an allocation lifetime and add compile-fail cases for escape/recycle; add a small allocation-generation runtime harness only for paths the lifetime cannot encode. **T1**.
- **Perturbation:** generation checks can mask the type design and add hot-path work; the primary negative control must be compile-fail, with runtime generations restricted to test instrumentation.
- **Cost and reuse:** **M**; shared with the user-memory alias defect and future borrowed kernel regions.
- **Judgment:** worth building; the plan's generation-model-first prescription is weaker than the available compiler oracle.

#### issues/isolation/kernelslice-over-user-memory.md — WORTH

- **Blind spot:** the unsafe slice accessors can expose a shared reference while userland retains a writable alias (`kernel/src/mm/region.rs:54-79`).
- **Instrument:** drive the real construction/copy primitive in a host harness with a writer that mutates at every copy boundary; require either an owned copy or word-atomic protocol, and retain the lifetime compile-fail cases. **T3**.
- **Perturbation:** a cooperative writer can miss hardware races; exhaust the factored state transitions and separately stress the real copy, treating only results common to both as system evidence.
- **Cost and reuse:** **M**; shares the lifetime/alias harness with the preceding issue.
- **Judgment:** worth building; it distinguishes copy and atomic designs without blessing a shared Rust slice.

## 6. VM inventory and state model — 2 defects

#### issues/kernel/one-mapping-is-written-in-two-ledgers.md — WORTH

- **Blind spot:** address-space regions live in `AddressSpace.regions` (`kernel/src/mm/paging.rs:425-431`) while process mmap metadata lives in `ProcessData.mmap_regions` (`kernel/src/process.rs:480-490`), so no single inventory proves they remain identical after every return edge.
- **Instrument:** expose a test-only mapping inventory from both ledgers and factor the commit/rollback transitions into a pure model exercised at each failure point. **T1**.
- **Perturbation:** test-only inventory locking can serialize the race; model the transition separately and sample inventories only at quiescent barriers.
- **Cost and reuse:** **L**; shared with loader publication and later VM transactions.
- **Judgment:** worth building, although consolidation—not merely a model—is the issue's eventual exit.

#### issues/kernel/dlopen-dedup-only-holds-after-the-race-settles.md — WORTH

- **Blind spot:** lookup occurs under one lock (`kernel/src/arch/syscall/vm.rs:176-192`) and publication under a later lock (`kernel/src/arch/syscall/vm.rs:306-313`), with no oracle for duplicate transient backing/mappings.
- **Instrument:** reuse the VM inventory and model reserve/publish/abort around the real shared-object key; the negative arm pauses both callers after lookup. **T1**.
- **Perturbation:** synchronization hooks can create rather than reveal the race; require the pure model to find the duplicate transition and use the hook only to reproduce it in the adapter.
- **Cost and reuse:** **M** after the VM model; shared with mapping transaction work.
- **Judgment:** worth building, though a held lock or second in-lock check remains a valid direct fix once the real race is reproduced.

## 7. Real MailboxNode steal-probe model — 1 defect

#### issues/kernel/steal-probe-node-dies-with-its-victim.md — WORTH

- **Blind spot:** an outstanding probe suppresses reposting (`toyos-sched/src/cpu.rs:2040-2075`) and a stopped victim can cost half the pulls (`toyos-sched/src/cpu.rs:2090-2093`); consumer completion controls `in_flight` (`toyos-sched/src/mailbox.rs:117-203`).
- **Instrument:** factor the real `MailboxNode` publish, consume, victim-retire, drop, and repost transitions into the scheduler's Loom crate and count exact-once reclamation. **T1**.
- **Perturbation:** a transliterated node would validate the test; import the real factored primitive and make the negative control restore victim-owned lifetime.
- **Cost and reuse:** **L**; reusable for mailbox reclamation and CPU-retirement races.
- **Judgment:** worth building; only the real primitive can adjudicate lifetime ownership.

## 8. Allocator-failure actuator — 1 defect

#### issues/kernel/no-alloc-error-handler.md — WORTH

- **Blind spot:** the kernel tree has no `alloc_error_handler` occurrence, while its global allocator can return null before initialization and delegates ready allocations directly to dlmalloc (`kernel/src/mm/alloc.rs:520-553`), with no controlled way to force the next allocation to fail while preserving a terminal observer.
- **Instrument:** add a test-only, countdown allocation failure actuator entered before a known allocation, with allocation-independent serial/panel capture and a host deadline. **T4**.
- **Perturbation:** the actuator's accounting must not allocate or alter normal allocator order; prove that countdown disabled is byte-for-byte inert and use a preallocated result channel.
- **Cost and reuse:** **M**; reusable for every allocation-failure and panic-path test.
- **Judgment:** worth building; an ordinary stress test cannot prove the terminal path.

## 9. Crash-preemption real state model — 1 defect

#### issues/panic-path/crash-report-preemption-untested.md — WORTH

- **Blind spot:** real preemption state combines a nesting count and `need_resched` (`kernel/src/preempt.rs:1-68`), while crash entry assumes an exact invariant (`kernel/src/arch/idt/exceptions.rs:140-186`); no model crosses nested crash entry and exit.
- **Instrument:** factor those real state transitions into `kernel-loom`, model interrupt/nested-crash schedules, and observe fallback-channel completion. **T1**.
- **Perturbation:** a simplified Boolean model would erase nesting; import the real counter transition and make the negative control restore preemptible crash entry.
- **Cost and reuse:** **M**; reusable across exception and panic entry paths.
- **Judgment:** worth building; the invariant is concurrency-sensitive and already has a natural real-state model boundary.

## 10. Source-based SSH client — 1 defect

#### issues/isolation/sshd-accept-path-unexercised.md — WORTH

- **Blind spot:** sshd defines authentication callbacks (`userland/sshd/src/main.rs:248-280`) and enables only public-key authentication (`userland/sshd/src/main.rs:386-395`), but no independent client drives accept, auth, and session startup.
- **Instrument:** build a source-based host protocol client from an independent implementation, pin its source, and drive a disposable key through accept/auth/command/close. **T2**.
- **Perturbation:** a client sharing sshd parsing code would reproduce the same bug; prohibit shared protocol code and make wire capture plus exit status the oracle.
- **Cost and reuse:** **L**; reusable for all SSH service acceptance and negative-auth cases.
- **Judgment:** worth building; do not commit an opaque host binary as the gate.

## 11. Manifest-authority differential — 1 defect

#### issues/isolation/toybox-is-one-row-for-nineteen-applets.md — WORTH

- **Blind spot:** init resolves a symlink to its binary row (`userland/init/src/main.rs:422-440`) while the manifest grants a union of authority to the toybox binary (`system.toml:90-101`); the missing observation is per-applet authority, not nineteen behavioral output contracts.
- **Instrument:** enumerate installed toybox links, resolve each through the real init lookup, and compare effective authority with an explicit per-applet policy table; exercise one allowed and one forbidden operation per distinct authority class. **T1**.
- **Perturbation:** duplicating the same resolver in the checker would be circular; parse the manifest and symlink inventory independently and compare their outputs.
- **Cost and reuse:** **M**; reusable by every multicall binary and manifest audit.
- **Judgment:** worth building; it directly tests the issue's authority-union claim.

## 12. Mapping/PMM residency counter — 1 defect

#### issues/build/std-leaks-a-thread-stack-per-spawn.md — WORTH

- **Blind spot:** ToyOS std allocates a 2 MiB stack but stores only the thread ID and `join` only performs the join syscall (`rust/library/std/src/sys/thread/toyos.rs:8-50`), while no bounded observation says whether mappings and resident frames return after join.
- **Instrument:** expose test-only per-process mapping and resident-frame counts, then run bounded spawn/join plateaus around the real std path. **T2**.
- **Perturbation:** the counter must not allocate per mapping or retain dead objects; compare its total to a quiescent page-table/PMM walk at the beginning and end.
- **Cost and reuse:** **M**; reusable for mmap, loader, process-retirement, and stack lifetime work.
- **Judgment:** worth building; it supplies the independent plateau required before changing the std fork.

## Instruments not worth building now — 3 defects

#### issues/build/ring-rs-shared-slice-over-a-userland-writable-page.md — NOT WORTH

- **Blind spot:** the plan requested a future model, but the issue is already closed: the source gate rejects slice construction across the ring boundary (`src/sourcegate.rs:697-729`) and the ABI exposes raw `Src`/`Dst` accessors instead (`toyos-abi/src/ring.rs:44-95`).
- **Instrument:** no new instrument; retain the existing source gate and raw-access API as **T1** enforcement.
- **Perturbation:** a new model would test an unreachable former primitive and could drift from the compile-time rule.
- **Cost and reuse:** avoided **M/L** work; the source gate already covers every caller.
- **Judgment:** not worth building now; delete the dead plan row in the separate reviewed cleanup.

#### issues/hardware/a-bar-sharing-the-scanout-page.md — NOT WORTH

- **Blind spot:** the only current observation is the range-overlap assertion (`kernel/src/mm/paging.rs:867-879`); no supported or observed machine topology supplies the disputed BAR/scanout alias.
- **Instrument:** defer until a real firmware map or independently specified emulator topology exists; then retain that map and compare it with an independent physical-range inventory. A synthetic allocator would be **T3**.
- **Perturbation:** a fixture built from the same allocator rules can prove only its own assumptions and may force an impossible topology.
- **Cost and reuse:** deferred **M** with low immediate reuse.
- **Judgment:** not worth building now; keep the issue recorded and attach the first real topology rather than manufacturing a verdict.

#### issues/kernel/volatile-composites-on-mmio-dma-structs.md — NOT WORTH

- **Blind spot:** generic composite volatile reads/writes exist (`kernel/src/mm/dma.rs:113-125`), but the required width/order differs by NVMe, virtio, xHCI, and virtio-gpu call sites, and there is no independent bus trace or per-device specification oracle in-tree.
- **Instrument:** defer a generic façade; for each device, first derive a scalar access contract from its normative specification or capture a real bus/device trace. That future work is **T2/T3**.
- **Perturbation:** an access-recording façade written from the current driver can validate the driver's own struct layout and still miss hardware transaction width or ordering.
- **Cost and reuse:** deferred **L**; reuse is superficial because the device contracts differ.
- **Judgment:** not worth building as one instrument now; retain the issue until device-specific oracle work is funded.

## Accounting

- Blocked rows audited: **24**.
- Unblocked by proposed instruments: **21**.
- Not worth building now: **3**.
- Reusable instrument families: **12**.
