# Owner decisions

The 42 category-E files reduce to **23 real questions**. Each answer below has an executable consequence; related files share one ruling. Questions that unblock other work come first.

## 1. Enforce the declared dependency bar?

Issues: `issues/build/nothing-checks-the-dependency-bar.md`, `issues/filesystem/fat32-suite-needs-macos-binaries.md`.

**Question:** Should the existing “Rust and QEMU only; no host binaries” rule become a committed source/dependency gate that also removes or replaces the four macOS FAT tools?

- **Yes:** costs a gate and a source-based FAT oracle replacement; buys an enforceable self-hosting boundary and unblocks FAT-suite portability.
- **No:** costs continued unaudited drift and leaves FAT verification dependent on forbidden binaries; buys no immediate engineering work.
- **Blocks:** trustworthy FAT gates and dependency cleanup.
- **Recommendation:** **Yes**—the rule is already written as a hard boundary, so declining enforcement only makes violations silent.

## 2. Is T14 audio a supported qualification target now?

Issues: `issues/audio/doom-audio-callback-stalled-on-the-t14.md`, `issues/audio/hda-ring-fix-unverified-on-metal.md`, `issues/audio/t14-wake-lateness-is-bimodal-per-boot.md`.

**Question:** Should T14 audio receive a scheduled owner-held qualification session that blocks audio changes until the callback stall, HDA ring fix, and bimodal wake mode are measured?

- **Yes:** costs scarce metal time and a quiet audio window; buys evidence for three otherwise unresolvable audio defects.
- **No:** costs leaving T14 audio unsupported/unverified; buys faster host-only audio work.
- **Blocks:** landing the HDA ring change and classifying the two T14-only failures.
- **Recommendation:** **Yes**, as one bounded session—the existing counters can answer all three together.

## 3. What does “idle audio” promise?

Issues: `issues/audio/null-sink-applies-one-connect.md`, `issues/audio/stop-the-device-voice-keep-the-wake.md`, `issues/kernel/soundd-past-due-wake-max-1.md`.

**Question:** Should an idle/null audio session preserve client wake cadence while stopping only the physical voice, with at most one catch-up wake on resume?

- **Yes:** costs an explicit soundd state contract and tests; buys pause/resume liveness without running hardware unnecessarily.
- **No:** costs either permanent wakes/device power or a colder resume with possible silence; buys a simpler implementation.
- **Blocks:** soundd idle-policy cleanup and the three separate timing assertions.
- **Recommendation:** **Yes**—stopping the voice while retaining the wake is the only option that preserves liveness and saves device power.

## 4. May a harness inject an empty endowment?

Issue: `issues/build/a-harness-injected-program-can-be-endowed-with-nothing.md`.

**Question:** Should the harness reject a program whose declared endowment is empty?

- **Yes:** costs one manifest/harness refusal and fixture updates; buys a fail-fast test premise.
- **No:** costs allowing tests that may pass only because their subject has no authority; buys flexibility for deliberately authority-free probes.
- **Blocks:** confidence in injected-program isolation tests.
- **Recommendation:** **No**, but require an explicit `empty` declaration—the authority-free probe is useful, while accidental emptiness must remain unrepresentable.

## 5. Should `debug = true` emit debug information?

Issue: `issues/build/debug-true-produces-no-debug-info.md`.

**Question:** Should the ToyOS profile honor `debug = true` with real debug sections despite the recorded build-time cost?

- **Yes:** costs the measured build slowdown and larger artifacts; buys truthful configuration and better crash/debug tooling.
- **No:** costs keeping a misleading profile knob; buys faster everyday iteration.
- **Blocks:** none; this is a build-quality trade-off.
- **Recommendation:** **No** for the default profile; rename/remove the misleading knob and provide an explicit diagnostic profile.

## 6. Share compiled artifacts across worktrees?

Issue: `issues/build/every-worktree-builds-its-own-copy-of-the-same-crates.md`.

**Question:** Should ordinary host crates share a content-addressed cache across worktrees while worktree-local final artifacts remain isolated?

- **Yes:** costs cache-key/invalidation engineering; buys substantial disk and rebuild savings without sharing mutable targets.
- **No:** costs repeated compilation and disk use; buys simple isolation.
- **Blocks:** build-throughput improvement only.
- **Recommendation:** **Yes**, but only content-addressed immutable outputs—never one shared Cargo target directory.

## 7. Automatically manage fork branch upstreams and warning debt?

Issues: `issues/build/fork-branches-have-no-upstream.md`, `issues/build/fork-estate-outside-the-warning-bar.md`.

**Question:** Should the worktree/fork tool set an upstream on creation and make fork warning drift a checked maintenance obligation?

- **Yes:** costs remote-branch lifecycle and periodic fork cleanup; buys reliable push/status behavior and visible fork debt.
- **No:** costs manual upstream repair and continued warning divergence; buys fewer automated remote mutations.
- **Blocks:** dependable fork maintenance, not product code.
- **Recommendation:** **Yes**—creation already owns branch mechanics, and warning drift is cheapest to stop at that boundary.

## 8. Carry a mio fork for deregistration semantics?

Issue: `issues/build/mio-deregister-fd-leaves-a-pending-poll-live.md`.

**Question:** Should ToyOS maintain a mio fork whose deregistration cancels pending polls on ToyOS?

- **Yes:** costs another maintained fork and upstream tracking; buys defined readiness teardown for Rust networking/event loops.
- **No:** costs documenting/working around live polls or avoiding mio; buys lower fork burden.
- **Blocks:** reliable mio-based services.
- **Recommendation:** **Yes** only if an in-tree consumer is ready to land with it; otherwise reject mio as unsupported rather than carry an unused fork.

## 9. Support Windows as a build host?

Issue: `issues/build/the-build-system-does-not-compile-on-windows.md`.

**Question:** Is Windows a supported host that must compile the build system and run host checks?

- **Yes:** costs path/process/locking portability work and a Windows CI lane; buys a second supported development host.
- **No:** costs excluding Windows contributors; buys focus on the current Unix host contract.
- **Blocks:** Windows development only.
- **Recommendation:** **No** until a real Windows maintainer/runner exists; declare the host set instead of carrying aspirational portability debt.

## 10. Treat the T14 runner as untrusted?

Issue: `issues/build/the-t14-runner-is-trusted-not-isolated.md`.

**Question:** Should repository jobs on the owner’s T14 run inside a disposable, least-privilege environment with secrets and host devices explicitly granted?

- **Yes:** costs runner isolation and device plumbing; buys containment for pull-request code.
- **No:** costs trusting repository code with the owner’s machine; buys simpler hardware access.
- **Blocks:** safely expanding self-hosted PR workloads.
- **Recommendation:** **Yes** before any untrusted PR is routed there; trusted/manual hardware jobs may remain a separate lane.

## 11. Represent thread zero in diagnostics?

Issue: `issues/diagnostics/a-record-cannot-name-thread-zero.md`.

**Question:** Should the diagnostic record ABI encode “no thread” separately so real thread ID zero is representable?

- **Yes:** costs an ABI-only migration across producers/consumers; buys lossless attribution.
- **No:** costs permanently reserving thread zero or mislabelling its records; buys no ABI work.
- **Blocks:** correct thread-zero diagnostics and any future use of that ID.
- **Recommendation:** **Yes**, in its own ABI PR; sentinel overloading is avoidable ambiguity.

## 12. Make GOP the default despite tearing, with an early panic console?

Issues: `issues/hardware/gop-path-off-by-default.md`, `issues/hardware/tearing-is-what-gop-cannot-give-back.md`, `issues/panic-path/no-console-between-boot-and-terminal.md`.

**Question:** Should GOP become the default display path only after an always-available early/panic console exists, accepting visible tearing until a different presentation mechanism exists?

- **Yes:** costs early-console work and accepts a documented visual limitation; buys hardware-representative default boots and visible failures.
- **No:** costs keeping the default on the less representative path and leaving early failures invisible on target hardware; buys tear-free current development output.
- **Blocks:** default GOP adoption and serial-less panic visibility.
- **Recommendation:** **Yes, sequenced after the panic console**; debuggability matters more than tearing, but the transition must not create a blind boot interval.

## 13. Support removal of the boot stick after boot?

Issue: `issues/hardware/pulling-the-boot-stick-freezes-the-t14.md`.

**Question:** Must a supported ToyOS session survive removal of its boot USB device?

- **Yes:** costs block-device revocation, mount degradation, and hardware tests; buys laptop-like removable-media behavior.
- **No:** costs declaring the boot device required for the session; buys a much smaller storage lifecycle contract.
- **Blocks:** USB-removal recovery only.
- **Recommendation:** **No for now**—make “boot medium remains attached” an explicit support condition until storage revocation is a planned product feature.

## 14. Commit to full integrated input on the T14?

Issues: `issues/hardware/t14-hands-over-an-uninitialised-8042.md`, `issues/hardware/t14-keyboard-will-not-report-its-scancode-set.md`, `issues/hardware/t14-lost-every-integrated-input.md`, `issues/hardware/the-t14-mouse-may-be-another-defect.md`.

**Question:** Is working integrated keyboard and pointing input on the T14 a release requirement, authorizing a single metal wave for 8042 initialization, scancode detection, and mouse/touchpad classification?

- **Yes:** costs owner-held hardware time and likely i8042/I2C-HID work; buys a usable target laptop without external input.
- **No:** costs requiring external USB input and leaving several hardware paths unclassified; buys focus elsewhere.
- **Blocks:** practical T14 desktop use.
- **Recommendation:** **Yes**—integrated input is foundational, and one coordinated wave avoids repeatedly rediscovering the same controller state.

## 15. Make received capabilities attenuable and typed?

Issues: `issues/isolation/a-moved-handle-is-always-re-movable.md`, `issues/isolation/a-received-handle-has-no-knowable-type.md`.

**Question:** Should capability transfer atomically attenuate rights and expose a stable object type to the receiver?

- **Yes:** costs an ABI-only transfer/type design and migration; buys least-authority delegation and safe receiver dispatch.
- **No:** costs every moved handle remaining re-transferable and type-blind; buys a smaller ABI.
- **Blocks:** robust capability delegation APIs.
- **Recommendation:** **Yes**, as one ABI design—the right attenuation and type tag belong in the same transfer result.

## 16. Let declared names reach undeclared descendants?

Issue: `issues/isolation/a-provided-name-cannot-reach-an-undeclared-child.md`.

**Question:** Should a process be allowed to delegate a provided name to a child that was not named in the original manifest?

- **Yes:** costs a dynamic namespace-delegation rule and audit trail; buys flexible process trees.
- **No:** costs declaring every receiving child shape up front; buys manifest-closed authority.
- **Blocks:** dynamic service delegation.
- **Recommendation:** **No**—pass the underlying handle explicitly; ambient name propagation weakens the project’s capability model.

## 17. Add bounded resource policies for DTV, memory, and shared objects?

Issues: `issues/isolation/dtv-capacity-is-a-workload-bound.md`, `issues/isolation/no-physical-memory-fairness.md`, `issues/isolation/so-cache-never-evicts.md`.

**Question:** Should every process/principal receive explicit physical-memory and DTV budgets, with shared-object cache eviction charged to those budgets?

- **Yes:** costs accounting, refusal semantics, and eviction policy; buys bounded denial-of-service behavior.
- **No:** costs workload-sized constants and unbounded cross-process pressure; buys simpler allocation paths.
- **Blocks:** multi-tenant isolation claims.
- **Recommendation:** **Yes**, starting with accounting/refusal before eviction optimization.

## 18. Require protected SSH authorization state?

Issue: `issues/isolation/sshd-authorized-keys-unprotected.md`.

**Question:** Must sshd refuse to start unless `authorized_keys` comes from a protected owner-controlled object rather than ambient writable storage?

- **Yes:** costs provisioning/ownership plumbing; buys meaningful SSH authentication.
- **No:** costs treating SSH authorization as mutable by ambient filesystem writers; buys easier demos.
- **Blocks:** calling sshd secure or enabling it by default.
- **Recommendation:** **Yes**—otherwise the authentication boundary is nominal.

## 19. Make desktop child/start liveness a supported contract?

Issues: `issues/isolation/t14-desktop-froze-at-64s.md`, `issues/kernel/desktop-window-child-freeze.md`, `issues/kernel/spawned-process-never-starts.md`.

**Question:** Should process/window startup have a bounded liveness contract that is release-gated on both QEMU and the T14?

- **Yes:** costs liveness instrumentation and metal runs; buys actionable guarantees for desktop applications.
- **No:** costs accepting indefinite start/freeze states; buys no new scheduler/desktop gate.
- **Blocks:** dependable desktop use.
- **Recommendation:** **Yes**, but define the progress markers before choosing a time bound.

## 20. Specify AP control-register and TSC admission?

Issues: `issues/kernel/ap-control-registers-inherit-init.md`, `issues/kernel/ap-tsc-trail-is-assumed-and-never-checked.md`.

**Question:** Should every AP apply the declared control-register state and be rejected from scheduling unless its TSC satisfies the clock contract?

- **Yes:** costs AP bring-up checks and a degradation/refusal path; buys explicit SMP correctness across future hardware/ARM work.
- **No:** costs relying on firmware/BSP inheritance and assumed clock symmetry; buys shorter bring-up.
- **Blocks:** credible heterogeneous/ARM64 portability.
- **Recommendation:** **Yes**—the root architecture rule already favors one declared CPU state applied everywhere.

## 21. Make deferred release synchronous to the syscall?

Issue: `issues/kernel/deferred-release-outlives-its-syscall.md`.

**Question:** Must resource release complete before the originating syscall returns?

- **Yes:** costs syscall latency and lock/ordering redesign; buys simple lifetime semantics.
- **No:** costs callers observing effects after return and requires a formally safe deferred queue; buys lower synchronous latency.
- **Blocks:** choosing the fix for the deferred-release race.
- **Recommendation:** **No**—retain deferral, but make queue ownership and completion explicit in the interleaving model.

## 22. Make syscall preemption and thread-exit completion explicit contracts?

Issues: `issues/kernel/syscall-preemption-is-incidental.md`, `issues/kernel/thread-exits-completion-post-is-the-second-one.md`.

**Question:** Should the kernel guarantee preemptible syscalls and one canonical thread-exit completion post?

- **Yes:** costs entry/exit invariant work and consolidation; buys testable scheduling and lifecycle semantics.
- **No:** costs retaining incidental behavior and duplicate completion paths; buys less near-term kernel churn.
- **Blocks:** reliable latency and exit-wait reasoning.
- **Recommendation:** **Yes**—both are boundary contracts already depended on by higher layers.

## 23. Sacrifice detail to guarantee panic diagnostics never block?

Issues: `issues/kernel/the-blocked-task-dump-panics-when-a-cpu-is-inside-inbox-submit.md`, `issues/panic-path/panic-holding-process-table-hangs.md`.

**Question:** Should panic/blocked-task diagnostics skip any process/inbox detail whose lock or snapshot cannot be obtained immediately?

- **Yes:** costs incomplete reports in contested states; buys guaranteed panic progress.
- **No:** costs possible recursive panic or hang while collecting a complete report; buys richer best-case output.
- **Blocks:** sound fixes for both diagnostic deadlocks.
- **Recommendation:** **Yes**—a partial report that terminates is strictly more useful than a complete report that can never be emitted.
