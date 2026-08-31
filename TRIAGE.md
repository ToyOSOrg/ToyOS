# Open-defect triage (working document)

This is a working review document, not an authority to delete or rewrite issue files. It classifies all 148 files that currently declare `kind: defect`: 73 A, 3 B, 9 C, 21 D, and 42 E. The corpus grew from 147 while this analysis was in progress when `issues/build/console-locale-detect-loses-every-typed-line.md` landed on `origin/main`.

## B — already fixed (3)

### `issues/diagnostics/the-panel-once-took-no-pixels-for-two-minutes-under-load.md` — B

The issue's own negative control attributes the failure to unpaced QMP typing (`issues/diagnostics/the-panel-once-took-no-pixels-for-two-minutes-under-load.md:35`), while `console_type_line` now bounds every burst and waits for the decoded echo (`tests/toyos.rs:6090`, `tests/toyos.rs:6100`, `tests/toyos.rs:6113`); commit `7a0334506b13033eab74574a716df792258f45e0` introduced that protection. A deletion review should rerun the issue's one-transmission negative control and the paced `console_graffiti`/`console_clear` oracle.

### `issues/kernel/io-uring-enter-trips-the-one-queue-invariant.md` — B

The failing second queue no longer exists: every wait registers on `TaskHandle::park_queue` (`toyos-sched/src/waitq.rs:131`, `kernel/src/completion/mod.rs:319`), and structural cleanup must finish before another registration (`toyos-sched/src/waitq.rs:379`, `kernel/src/sched/driver.rs:602`). Commit `1bfe4e5bf80bde443fa31d4331734cdc804b01ca` made fifteen park sites one; deletion review should revert it to reproduce the old io_uring panic and use the current io_uring test plus the one-queue drop assertion (`toyos-sched/src/waitq.rs:402`) as oracle.

### `issues/kernel/keyboard-flood-panics-blocked-read.md` — B

The keyboard reader can no longer leave a task registered on a private input queue before it parks elsewhere: all blocking goes through the task's single park queue (`toyos-sched/src/waitq.rs:131`, `kernel/src/completion/mod.rs:319`) and the registration is structurally removed on return (`kernel/src/sched/driver.rs:602`). Commit `1bfe4e5bf80bde443fa31d4331734cdc804b01ca` is the change; deletion review should revert it for the keyboard-flood panic and use the current flood test plus the registration drop assertion (`toyos-sched/src/waitq.rs:402`) independently.

## C — duplicate or subsumed (9)

### `issues/audio/thorough-tier-disagrees-with-its-sample.md` — C

Keep `issues/audio/thorough-tier-reds-on-unmodified-main.md`: both dispute the same recorded 0/120 baseline (`issues/audio/thorough-tier-disagrees-with-its-sample.md:27`, `issues/audio/thorough-tier-reds-on-unmodified-main.md:53`), and the survivor also carries the clean-main evidence.

### `issues/boot-media/boot-exists-only-on-a-usb-boot.md` — C

Keep `issues/build/page-cache-owns-one-device.md`: this file says internal-disk boot loses `/boot` because page-cache initialization takes the NVMe (`issues/boot-media/boot-exists-only-on-a-usb-boot.md:11`), which is the broader survivor's sole-device ownership defect.

### `issues/boot-media/unlink-and-reallocate-left-two-lost-cluster-chains.md` — C

Keep `issues/filesystem/fat-unlink-reallocate-leaks-a-cluster-under-load.md`: both report lost FAT cluster chains after unlink/reallocation, while the survivor names the reusable mechanism and load reproducer.

### `issues/build/syscall-window-nmi-reds-under-a-shared-host.md` — C

Keep `issues/build/parallel-tests-red-under-other-suites.md`: the former records a 220x host-load wall stretch (`issues/build/syscall-window-nmi-reds-under-a-shared-host.md:21`), one concrete instance of the survivor's unisolated parallel-suite timing defect.

### `issues/diagnostics/process-stats-exited-child-only.md` — C

Keep `issues/diagnostics/the-kernel-keeps-nothing-it-enumerates.md`: the exited-child-only observation is one manifestation of the broader enumeration interface retaining no stable record for objects that disappear.

### `issues/isolation/client-request-is-an-allocation.md` — C

Keep `issues/isolation/no-physical-memory-fairness.md`: an unauthenticated client allocation “charged to nobody” (`issues/isolation/client-request-is-an-allocation.md:42`) is a direct instance of the survivor's missing physical-memory ownership and fairness policy.

### `issues/isolation/process-isolation-ungated.md` — C

Keep `issues/isolation/a-moved-handle-is-always-re-movable.md`: both identify that sending requires `Rights::TRANSFER` and preserves it (`issues/isolation/process-isolation-ungated.md:25`, `issues/isolation/a-moved-handle-is-always-re-movable.md:10`), so the receiver can always forward the capability.

### `issues/kernel/a-killed-peer-still-takes-a-write.md` — C

Keep `issues/kernel/deferred-release-outlives-its-syscall.md`: both hinge on `ZERO_PENDING` being cleared before deferred hooks run (`issues/kernel/a-killed-peer-still-takes-a-write.md:33`, `issues/kernel/deferred-release-outlives-its-syscall.md:10`), and the survivor covers the general lifetime race.

### `issues/kernel/internal-disk-boot-has-no-boot-mount.md` — C

Keep `issues/build/page-cache-owns-one-device.md`: this file expressly says `page_cache::init` takes sole ownership of the internal disk (`issues/kernel/internal-disk-boot-has-no-boot-mount.md:12`), the same root defect as the broader device-ownership issue.

## D — not a defect (21)

### `issues/audio/gate-a-first-run-to-record-its-host.md` — D

This is a **finding**: it records the first host measurement of Gate A and its environmental context, not a reproducible product failure.

### `issues/build/a-loaded-suite-reds-a-volume-checker-on-both-arms.md` — D

This is a **finding**: both experiment arms failed under load, so the record establishes an inconclusive measurement rather than a code defect.

### `issues/build/clippy-has-never-run-here.md` — D

This is a **track**: it proposes adding and paying for a new lint/build program; absence of that chosen program is work to stage, not a reproduced malfunction.

### `issues/build/i8042-health-sits-on-the-ten-second-line.md` — D

This is a **track** for a remaining performance optimization: the cited tier placement has since moved, while the record's live request is to shorten the i8042 health check.

### `issues/build/python-and-cc-are-declared.md` — D

This is a **track** for replacing declared host bootstrap dependencies, a staged self-hosting program rather than evidence that a declared dependency is broken.

### `issues/build/readdir-bound-is-priced-six-times-under-its-cost.md` — D

This is a **finding**: it records a price-versus-measurement mismatch pending a clean nightly sample, rather than isolating a behavioral defect.

### `issues/build/toyos-cc-has-no-codegen-gate.md` — D

This is a **track** to create a compiler-codegen validation program; the desired gate and corpus are staged work, not a current failing behavior.

### `issues/build/xhci-full-speed-device-jumped-47-percent-over-its-commitment.md` — D

This is a **finding**: it records one duration regression measurement and asks for attribution before changing the commitment.

### `issues/design-debt/what-is-owed-on-file-size.md` — D

This is a **track**: it inventories the steps required to change the file-size representation and verification surface.

### `issues/filesystem/usb-audit-findings-not-in-vcs.md` — D

This is a **track** to import and disposition an external audit's findings; the record does not itself specify one reproducible filesystem defect.

### `issues/hardware/eleven-names-red-on-ci.md` — D

This is a **finding**: it is an incident ledger of eleven CI names awaiting per-test adjudication, not one mechanism with one fix.

### `issues/hardware/four-runner-reds-unclassified.md` — D

This is a **finding**: it preserves four unclassified runner observations and their evidence rather than claiming a single product defect.

### `issues/hardware/metal-boot-accounting-is-stale.md` — D

This is a **finding**: it says the recorded metal-boot accounting no longer matches observed measurements and needs remeasurement.

### `issues/hardware/process-start-skew-on-a-runner.md` — D

This is a **finding**: it records a runner-specific skew measurement without yet identifying a defective mechanism.

### `issues/hardware/xhci-flap-wedges-under-kvm.md` — D

This is a **finding**: the original flap stopped reproducing and the remaining text records an incident plus an unclassified transport-break observation.

### `issues/kernel/a-double-fault-on-cpu-1-under-a-wide-suite.md` — D

This is a **finding**: it preserves a one-off crash signature and the owed reproduction count, but has no isolated mechanism yet.

### `issues/kernel/bcachefs-crate-is-not-bcachefs.md` — D

This is a **track** for the staged scope and naming of a filesystem implementation; it is not a claim that the current advertised interface malfunctions.

### `issues/kernel/echo-faulted-after-the-fault-arms.md` — D

This is a **finding**: it records a timing-sensitive fault observation whose causal mechanism remains unclassified.

### `issues/kernel/i13s-margin-at-32-cpus-is-down-to-two-milliseconds.md` — D

This is a **finding**: it is a measured safety-margin observation, with no violated contract or isolated cause yet.

### `issues/kernel/the-split-window-tlb-cost-is-unpriced.md` — D

This is a **finding**: it identifies an unmeasured cost and prescribes measurement before any design decision.

### `issues/panic-path/a-double-panic-at-boots-edge-says-nothing-but-its-name.md` — D

This is a **finding**: it records a single panic-path incident and the evidence needed to classify it, rather than a proven mechanism.

## E — owner-shaped (42)

### `issues/audio/doom-audio-callback-stalled-on-the-t14.md` — E

Owner question: should the T14-specific Doom audio stall be a release blocker requiring owner-held hardware validation, yes or no?

### `issues/audio/hda-ring-fix-unverified-on-metal.md` — E

Owner question: should the HDA ring change remain unlandable until the owner runs its metal-only validation, yes or no?

### `issues/audio/null-sink-applies-one-connect.md` — E

Owner question: should a null sink accept only one connection by design, yes or no?

### `issues/audio/stop-the-device-voice-keep-the-wake.md` — E

Owner question: should stopping a device voice preserve its wake schedule as the product policy, yes or no?

### `issues/audio/t14-wake-lateness-is-bimodal-per-boot.md` — E

Owner question: should the owner spend T14 time characterizing and gating the boot-dependent wake-lateness modes, yes or no?

### `issues/build/a-harness-injected-program-can-be-endowed-with-nothing.md` — E

Owner question: should harness-injected programs be forbidden from receiving an empty endowment, yes or no?

### `issues/build/debug-true-produces-no-debug-info.md` — E

Owner question: should `debug = true` emit debug information despite the recorded build-time cost, yes or no?

### `issues/build/every-worktree-builds-its-own-copy-of-the-same-crates.md` — E

Owner question: should worktrees share compiled crate artifacts despite the isolation and invalidation trade-off, yes or no?

### `issues/build/fork-branches-have-no-upstream.md` — E

Owner question: should newly created worktree branches automatically receive an upstream branch, yes or no?

### `issues/build/fork-estate-outside-the-warning-bar.md` — E

Owner question: should the repository's fork estate be brought inside the warning-policy bar now, yes or no?

### `issues/build/mio-deregister-fd-leaves-a-pending-poll-live.md` — E

Owner question: should ToyOS carry a local mio policy/fork to cancel pending polls on deregistration, yes or no?

### `issues/build/nothing-checks-the-dependency-bar.md` — E

Owner question: should dependency-policy enforcement become a committed gate, yes or no?

### `issues/build/the-build-system-does-not-compile-on-windows.md` — E

Owner question: is Windows a supported host whose build must compile, yes or no?

### `issues/build/the-t14-runner-is-trusted-not-isolated.md` — E

Owner question: should the T14 runner be isolated as an untrusted execution environment, yes or no?

### `issues/diagnostics/a-record-cannot-name-thread-zero.md` — E

Owner question: should thread zero become representable in the diagnostic record ABI, yes or no?

### `issues/filesystem/fat32-suite-needs-macos-binaries.md` — E

Owner question: should the independent FAT oracle depend on macOS binaries, or must the owner choose a different implementation, yes or no to retaining that dependency?

### `issues/hardware/gop-path-off-by-default.md` — E

Owner question: should the GOP display path be enabled by default, yes or no?

### `issues/hardware/pulling-the-boot-stick-freezes-the-t14.md` — E

Owner question: is surviving boot-stick removal on the owner's T14 a supported product requirement, yes or no?

### `issues/hardware/t14-hands-over-an-uninitialised-8042.md` — E

Owner question: should ToyOS explicitly initialize the T14's 8042 instead of relying on firmware handoff, yes or no?

### `issues/hardware/t14-keyboard-will-not-report-its-scancode-set.md` — E

Owner question: should the T14 keyboard's failure to report its scancode set block support until owner-held hardware work resolves it, yes or no?

### `issues/hardware/t14-lost-every-integrated-input.md` — E

Owner question: should integrated input on the owner's T14 be a required supported configuration, yes or no?

### `issues/hardware/tearing-is-what-gop-cannot-give-back.md` — E

Owner question: is GOP tearing an accepted product limitation, yes or no?

### `issues/hardware/the-t14-mouse-may-be-another-defect.md` — E

Owner question: should owner-held T14 testing be spent to establish and support the mouse path, yes or no?

### `issues/isolation/a-moved-handle-is-always-re-movable.md` — E

Owner question: should the capability model gain a non-transferrable received handle even though transfer currently requires preserving `TRANSFER`, yes or no?

### `issues/isolation/a-provided-name-cannot-reach-an-undeclared-child.md` — E

Owner question: should provided names be allowed to reach undeclared descendant processes, yes or no?

### `issues/isolation/a-received-handle-has-no-knowable-type.md` — E

Owner question: should the ABI expose a stable query for a received handle's object type, yes or no?

### `issues/isolation/dtv-capacity-is-a-workload-bound.md` — E

Owner question: should DTV capacity become dynamic rather than a fixed workload bound, yes or no?

### `issues/isolation/no-physical-memory-fairness.md` — E

Owner question: should physical memory be charged and limited per process/principal, yes or no?

### `issues/isolation/so-cache-never-evicts.md` — E

Owner question: should the shared-object cache have an eviction policy, yes or no?

### `issues/isolation/sshd-authorized-keys-unprotected.md` — E

Owner question: should `authorized_keys` receive a mandatory protection/ownership policy before SSH is supported, yes or no?

### `issues/isolation/t14-desktop-froze-at-64s.md` — E

Owner question: should the owner spend scarce T14 access reproducing and gating the 64-second desktop freeze, yes or no?

### `issues/kernel/ap-control-registers-inherit-init.md` — E

Owner question: should application processors initialize control registers independently instead of inheriting bootstrap state, yes or no?

### `issues/kernel/ap-tsc-trail-is-assumed-and-never-checked.md` — E

Owner question: should unsupported/non-invariant multi-CPU TSC behavior be detected and rejected, yes or no?

### `issues/kernel/deferred-release-outlives-its-syscall.md` — E

Owner question: should deferred releases be made synchronous to the originating syscall, accepting the latency/locking consequences, yes or no?

### `issues/kernel/desktop-window-child-freeze.md` — E

Owner question: should the owner prioritize and gate the hardware/desktop-specific child freeze, yes or no?

### `issues/kernel/soundd-past-due-wake-max-1.md` — E

Owner question: is a maximum of one past-due `soundd` wake an enforceable product requirement, yes or no?

### `issues/kernel/spawned-process-never-starts.md` — E

Owner question: should process-start liveness receive a bounded product guarantee rather than remain scheduler-dependent, yes or no?

### `issues/kernel/syscall-preemption-is-incidental.md` — E

Owner question: should syscall preemption become a supported invariant with an explicit mechanism and gate, yes or no?

### `issues/kernel/the-blocked-task-dump-panics-when-a-cpu-is-inside-inbox-submit.md` — E

Owner question: should blocked-task dumps be made safe during inbox submission despite the diagnostic complexity, yes or no?

### `issues/kernel/thread-exits-completion-post-is-the-second-one.md` — E

Owner question: should thread-exit completion be consolidated into a single posting contract, yes or no?

### `issues/panic-path/no-console-between-boot-and-terminal.md` — E

Owner question: should early boot guarantee a visible panic console before the terminal owns the display, yes or no?

### `issues/panic-path/panic-holding-process-table-hangs.md` — E

Owner question: should panic reporting deliberately bypass process-table-backed diagnostics when that lock is held, yes or no?

## A — actionable now (73)

### `issues/audio/cpal-backend-hardcodes-the-format.md` — A

Negotiate the device's reported stream format instead of hard-coding it; negative-control with a deliberately different supported format, and independently oracle the emitted PCM metadata and decoded samples.

### `issues/audio/desktop-session-put-26ms-of-silence.md` — A

Remove the scheduling/buffering gap that inserts the recorded silence; negative-control by restoring the old refill boundary, and oracle both the captured waveform's longest zero run and the scheduler wake trace.

### `issues/audio/disk-wait-pins-a-cpu.md` — A

Replace the disk-wait spin with a blocking completion; negative-control by reinstating the poll loop, and oracle idle CPU time independently while the same disk operation is stalled.

### `issues/audio/gate-a-has-no-runner-baseline.md` — A

Record and enforce a runner-specific Gate A baseline; negative-control with a fixture outside the accepted envelope, and oracle the same capture with the independent audio analyzer.

### `issues/audio/gate-a-suspend-structure-verdict-unread.md` — A

Make the gate consume and assert the suspend-structure verdict it already produces; negative-control by injecting a structurally invalid suspend, and oracle the raw trace independently of the gate's summary.

### `issues/audio/hda-tone-phase-check.md` — A

Add a phase-continuity assertion to the HDA tone test; negative-control by resetting phase at a buffer boundary, and oracle the captured samples with an independent phase-delta calculation.

### `issues/audio/hda-tone-red-beyond-its-exemption.md` — A

Remove or narrow the exemption so the observed out-of-envelope tone fails the owning gate; negative-control with the recorded bad capture, and oracle frequency/amplitude from the raw PCM independently.

### `issues/audio/idle-suspend-reds-on-a-loaded-host-and-on-main.md` — A

Replace the host-wall-clock verdict with a guest/device-state completion or isolate the load sensitivity; negative-control under the recorded host load, and oracle the suspend/resume transition from device traces.

### `issues/audio/thorough-tier-reds-on-unmodified-main.md` — A

Rebaseline only after fixing the mismatch between the 0/120 recorded sample and the supported runner environment; negative-control with the current baseline on clean main, and oracle the waveform independently of the harness verdict.

### `issues/boot-media/kernel-log-file-reds-beside-other-guests-and-is-green-alone.md` — A

Remove shared-host timing from the kernel-log-file verdict or serialize its scarce resource; negative-control beside the competing guests, and oracle the produced log's contents independently.

### `issues/boot-media/the-gpt-floor-belongs-to-the-caller-not-the-parser.md` — A

Move the minimum-disk-size policy out of the GPT parser and into the boot-media caller; negative-control with a valid smaller GPT, and oracle its structure with an independent GPT checker.

### `issues/boot-media/usb-short-read-reds-beside-other-guests-and-is-green-alone.md` — A

Make the short-read test wait on guest/device progress rather than contended host time; negative-control beside the competing guests, and oracle returned bytes and completion status independently.

### `issues/build/contention-has-no-owning-instrument.md` — A

Add a committed instrument that attributes guest-slot and host contention to the owning jobs; negative-control with two deliberate contenders, and oracle the overlap from host process timestamps.

### `issues/build/console-locale-detect-loses-every-typed-line.md` — A

Add a real guest-side acknowledgement between `shell_type_once`'s PS/2 bursts; negative-control with the current 44-byte unpaced handshake, and oracle it against a 14-byte one-batch control plus panel-paced delivery and i8042 drain counts.

### `issues/build/free-memory-verdicts-share-a-boot.md` — A

Give free-memory verdicts isolated boot state or prove/reset all state between them; negative-control by reversing their order, and oracle each test from a fresh-boot memory snapshot.

### `issues/build/memmap2-fork-is-unreachable-code.md` — A

Remove the unreachable fork or wire the intended dependency override so it is actually built; negative-control by introducing a fork-only marker, and oracle the resolved Cargo graph independently.

### `issues/build/page-cache-owns-one-device.md` — A

Make page-cache registration support multiple block devices without consuming the only device handle; negative-control by booting the internal-disk topology, and oracle both boot mount and page-cache I/O through separate probes.

### `issues/build/parallel-tests-red-under-other-suites.md` — A

Make scarce guest slots explicit scheduler resources or replace wall-clock verdicts with progress; negative-control with the documented concurrent suites, and oracle isolated-arm parity plus host contention telemetry.

### `issues/build/prose-ledger-carries-slack-a-sweep-never-booked.md` — A

Add the omitted sweep to the executable ledger or remove the unsupported prose claim; negative-control with the sweep absent, and oracle generated ledger coverage against the declared inventory.

### `issues/build/ring-rs-shared-slice-over-a-userland-writable-page.md` — A

Stop constructing a shared immutable slice over user-writable memory by copying or pinning/excluding writes; negative-control with a concurrent user mutation, and oracle under Miri/Loom plus a value-integrity check.

### `issues/build/std-fork-not-rustfmt-clean.md` — A

Format the fork and add its formatting check; negative-control with a known misformatted fixture, and oracle `rustfmt --check` over the fork independently of the wrapper.

### `issues/build/std-leaks-a-thread-stack-per-spawn.md` — A

Release or reuse the stack mapping when a spawned thread exits; negative-control by restoring the missing release, and oracle resident/mapped stack pages across a bounded spawn/join loop.

### `issues/build/std-systemtime-now-returns-the-epoch.md` — A

Implement `SystemTime::now` from the ToyOS wall clock instead of the epoch stub; negative-control with the stub, and oracle monotonic plausibility against the kernel-reported real-time value.

### `issues/build/the-gate-is-a-full-suite.md` — A

Split the gate into declared, independently selectable checks rather than aliasing the full suite; negative-control by breaking one component, and oracle the gate manifest against the commands actually executed.

### `issues/build/the-shard-split-prices-a-boot-and-not-the-image-behind-it.md` — A

Charge or cache the image construction separately from each shard's boot price; negative-control with an invalidated image, and oracle build and boot timestamps from the host artifact graph.

### `issues/build/worktree-add-help-panics-on-statvfs.md` — A

Handle `statvfs` failure on the help-only path instead of unwrapping it; negative-control by injecting the recorded `statvfs` error, and oracle that `worktree add --help` exits successfully without creating a worktree.

### `issues/design-debt/kernelslice-outlives-its-allocation.md` — A

Tie `KernelSlice` ownership/lifetime to its allocation or copy the data; negative-control by freeing/reusing the backing allocation, and oracle with an allocation-generation check under the existing stress path.

### `issues/design-debt/rights-log-names-a-holder-that-does-not-hold-it.md` — A

Log the actual post-transfer holder rather than the stale source identity; negative-control with a cross-process transfer, and oracle the rights table independently of the log rendering.

### `issues/design-debt/std-says-this-machine-has-one-cpu.md` — A

Implement the standard-library CPU-count query from kernel topology; negative-control on a multi-vCPU guest with the stub restored, and oracle against the kernel topology export.

### `issues/diagnostics/a-console-tag-is-composed-by-replacing-a-bracket.md` — A

Render console tags structurally instead of mutating a delimiter byte; negative-control with a tag containing the problematic bracket shape, and oracle the parsed tag fields independently.

### `issues/diagnostics/blocked-dump-cannot-fire-on-a-total-freeze.md` — A

Add an NMI/watchdog-owned trigger that does not depend on the frozen scheduler; negative-control by freezing all schedulable work, and oracle the emitted dump from an external serial capture. Citation drift: the old trigger locations have moved, but the total-freeze reachability claim remains.

### `issues/diagnostics/blocked-time-is-invisible-while-the-park-lasts.md` — A

Account current in-progress blocked duration when producing stats; negative-control with a task held parked across the sample, and oracle elapsed guest time from the park trace.

### `issues/diagnostics/no-guest-can-change-the-display-mode.md` — A

Expose a bounded display-mode operation to the guest/display owner; negative-control by requesting a second supported mode, and oracle scanout geometry through the device state independently.

### `issues/filesystem/a-shrink-unflushed-regrows-the-old-tail.md` — A

Invalidate or truncate dirty cache pages when file size shrinks; negative-control by shrinking before flush then extending, and oracle bytes and size after remount with an independent reader.

### `issues/filesystem/a-spawn-reads-round-an-open-file-s-dirty-pages.md` — A

Make spawn/exec observe the coherent page-cache view of an open executable; negative-control with unflushed modifications, and oracle the child's bytes against a direct cache read.

### `issues/filesystem/delete-prefix-keeps-the-unbounded-walk-list-gave-up.md` — A

Replace the unbounded prefix walk with a bounded/streaming traversal or enforce and prove a namespace bound; negative-control with a tree beyond the old retained-list capacity, and oracle the surviving namespace after `cargo test -p bcachefs`.

### `issues/filesystem/fat-overwrite-rename-frees-the-destination-first.md` — A

Make overwrite-rename commit the destination replacement before reclaiming its old chain; negative-control with the existing injected device error, and oracle the resulting image with `toyos-fat32-check`.

### `issues/filesystem/fat-unlink-reallocate-leaks-a-cluster-under-load.md` — A

Make FAT unlink and allocation update ordering/recovery preserve reachability; negative-control with the recorded concurrent unlink/reallocate load, and oracle the image with the independent FAT checker.

### `issues/filesystem/readdir-bound-is-per-mount.md` — A

Enforce the iteration bound per directory stream rather than sharing it per mount; negative-control with concurrent large-directory readers, and oracle complete entry sets from independent directory walks.

### `issues/filesystem/std-stat-conflates-io-with-notfound.md` — A

Preserve I/O errors through the standard-library stat mapping instead of returning `NotFound`; negative-control with an injected device read error, and oracle the raw syscall errno.

### `issues/filesystem/usb-esp-gate-holes.md` — A

Add the missing ESP integrity scenarios to the committed USB gate; negative-control each named corruption fixture, and oracle the images with an independent GPT/FAT checker.

### `issues/hardware/a-bar-sharing-the-scanout-page.md` — A

Reject or safely partition a BAR mapping that aliases the scanout page; negative-control with the documented overlap topology, and oracle physical ranges from PCI resources and framebuffer ownership independently.

### `issues/hardware/anonymous-mmap-is-not-demand-paged.md` — A

Install lazy anonymous mappings and fault pages on first access; negative-control with the eager allocator restored, and oracle committed physical pages before and after sparse touches.

### `issues/hardware/hotplug-blocks-a-scheduler-pass.md` — A

Move the remaining debounce/work deadline out of the scheduler pass into asynchronous stepped work; negative-control with the hotplug delay restored, and oracle maximum pass time plus successful enumeration. Citation drift: enumeration is now stepped, but the current `PORT_WORK_AT` wait still keeps a CPU awake.

### `issues/hardware/kernel-log-unreadable-once-userland-owns-the-screen.md` — A

Provide a serial/log retrieval path independent of the userland-owned scanout; negative-control by handing the display to userland then panicking, and oracle the full kernel log from the independent channel. Citation drift: the referenced ownership code moved, but the visibility failure remains.

### `issues/hardware/pre-flash-gate-missed-the-milestone.md` — A

Make the pre-flash validation an enforced prerequisite rather than a dated milestone; negative-control with a deliberately invalid image, and oracle the same image using independent structural checkers.

### `issues/hardware/xhci-waits-are-spins.md` — A

Convert xHCI completion waits from CPU spins to interrupt/completion blocking; negative-control with a delayed device response, and oracle CPU idle time and transfer completion separately.

### `issues/isolation/a-broken-pipe-answers-not-found.md` — A

Map the closed-pipe condition to the correct broken-pipe error; negative-control by closing the peer before write, and oracle the raw syscall result independently of the std wrapper.

### `issues/isolation/bus-mastering-rides-memory-decode.md` — A

Authorize PCI bus mastering separately from memory decode; negative-control with a device granted MMIO but denied DMA, and oracle command-register bits and IOMMU-visible DMA independently.

### `issues/isolation/kernelslice-over-user-memory.md` — A

Remove kernel slices that borrow mutable user pages across trust boundaries by copying or pinning with exclusion; negative-control with concurrent user mutation, and oracle value integrity under a race model.

### `issues/isolation/probe-mounts-on-a-checksum.md` — A

Require structural filesystem validation beyond a matching checksum before mounting; negative-control with a checksum-valid malformed image, and oracle it with an independent filesystem checker.

### `issues/isolation/sshd-accept-path-unexercised.md` — A

Add a host-side accept/auth/session test for the sshd path; negative-control by breaking accept or authentication, and oracle the negotiated session from an independent SSH client.

### `issues/isolation/toybox-is-one-row-for-nineteen-applets.md` — A

Give each supported applet an executable contract instead of one aggregate row; negative-control by breaking one applet, and oracle outputs against independent fixtures per applet.

### `issues/isolation/untrusted-sites-not-yet-adopted.md` — A

Move the named user-controlled pointer sites onto the untrusted-memory primitives; negative-control with mutation/fault injection at each site, and oracle that no borrowed user reference survives validation.

### `issues/kernel/dlopen-dedup-only-holds-after-the-race-settles.md` — A

Reserve a shared-object load identity atomically before concurrent loaders allocate/map it; negative-control with synchronized competing `dlopen` calls, and oracle one backing object and one mapping identity after both complete.

### `issues/kernel/fatal-text-safety-comment-claims-a-write-that-recurs.md` — A

Add a single atomic state machine that excludes refresh writers from live fatal-text readers, rather than weakening the comment; negative-control by reverting the whole state change in the Loom model, and oracle the real factored primitive with reader-versus-refresh interleavings.

### `issues/kernel/granularity-bound-crossed-at-four-widths.md` — A

Use checked arithmetic and a representation that preserves the stated granularity bound at all four widths; negative-control at the first crossing values, and oracle results with a wider independent integer model.

### `issues/kernel/kernel-hashmaps-take-userland-chosen-keys.md` — A

Use a keyed collision-resistant hasher or a non-hash lookup for attacker-chosen kernel keys; negative-control with a generated collision set, and oracle bounded operation cost under the adversarial corpus.

### `issues/kernel/lseek-past-eof-is-silently-clamped.md` — A

Preserve legal seek positions beyond EOF rather than clamping them; negative-control by seeking past EOF then writing, and oracle the resulting hole, size, and bytes through an independent reader.

### `issues/kernel/no-alloc-error-handler.md` — A

Install a panic-safe allocation-error handler with a bounded diagnostic path; negative-control by exhausting a controlled allocator, and oracle the emitted terminal reason without recursive allocation.

### `issues/kernel/one-mapping-is-written-in-two-ledgers.md` — A

Make one mapping transaction update a single authoritative ledger or atomically commit both; negative-control with failure between the two updates, and oracle page tables against the mapping inventory.

### `issues/kernel/past-eof-holes-wedge-a-shared-boot.md` — A

Fix sparse past-EOF I/O so hole creation cannot wedge shared boot state; negative-control with the recorded sparse write while another boot user runs, and oracle file contents plus continued boot-device progress independently.

### `issues/kernel/process-open-panics-on-a-reopened-process.md` — A

Make reopening an existing process return a defined handle/error instead of violating an assertion; negative-control with the duplicate open sequence, and oracle handle-table state after the call.

### `issues/kernel/retire-tripwire-is-not-queue-shaped.md` — A

Replace the scalar retirement tripwire with accounting shaped to every queued retire operation; negative-control with overlapping retirements past the old `GIVE_UP` path, and oracle that every queued object is reclaimed exactly once.

### `issues/kernel/scheduler-pass-blocks-in-xhci.md` — A

Move xHCI recovery/hotplug work fully outside the scheduler pass and measure the complete pass including its prologue; negative-control with a delayed xHCI step, and oracle pass latency plus transfer progress independently. Citation drift: xHCI work has changed, but the measured window still omits the relevant prologue.

### `issues/kernel/spawn-thread-disagrees-about-a-reaped-parent.md` — A

Make parent liveness/reaping a single atomic contract for thread spawn; negative-control with spawn synchronized against parent reap, and oracle the process/thread tables after every interleaving.

### `issues/kernel/steal-probe-node-dies-with-its-victim.md` — A

Give steal-probe nodes ownership independent of the victim task's lifetime; negative-control by retiring the victim during a probe, and oracle queue reachability and reclamation under Loom/stress.

### `issues/kernel/syscall-window-nmi-shortfalls-on-a-contended-host.md` — A

Base the NMI verdict on guest progress/cycles or isolate its host scheduling budget; negative-control with the recorded contender, and oracle captured NMIs against guest-side window markers.

### `issues/kernel/the-global-pipe-lock-spans-a-user-copy.md` — A

Stage pipe data so the global lock is not held across faultable user copies; negative-control with a deliberately faulting/slow user buffer, and oracle concurrent pipe progress and byte ordering.

### `issues/kernel/volatile-composites-on-mmio-dma-structs.md` — A

Replace volatile reads/writes of composite MMIO/DMA structures with field-sized accesses and explicit ordering; negative-control with a boundary-torn fixture, and oracle the emitted access sequence independently.

### `issues/panic-path/crash-report-preemption-untested.md` — A

Add a host/Loom check that preemption state cannot invalidate crash-report progress; negative-control by restoring preemptible entry at the dangerous point, and oracle completion from the independent panic channel.

### `issues/panic-path/panic-console-capture-untested.md` — A

Exercise the real panic-console capture primitive across writer/reader interleavings, not only its latch; negative-control with the capture exclusion removed, and oracle captured text equality under Loom plus a host parser.

### `issues/panic-path/panic-on-wedged-virtio-console-spins.md` — A

Bound or bypass virtio-console transmission in panic context; negative-control with a device that never completes, and oracle that the fallback channel reports the panic within the bound.
