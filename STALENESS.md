# Defect-record staleness audit

Audit point: `cc3b1597935fa6b584c48e077ea38a4beae6890d` (verified as a commit with
`git cat-file -t`). The corpus command was
`rg -l '^kind: defect$' issues --glob '*.md' | sort`; it returned 129 files.
The entries below preserve that order. A missing observation is not promoted
to a source defect: those entries are `UNCHECKABLE`, with the observation that
would settle them. Negative source searches were run with `rg`, and the one
fixing commit cited below was verified with `git cat-file -t`.

## Summary

Counts are filled from the mechanically checked entries below:

- LIVE: 63
- ALREADY-FIXED: 0
- PARTLY-FIXED: 24
- MISDESCRIBED: 3
- UNCHECKABLE: 39
- Total: 129

### Ranked high-confidence `ALREADY-FIXED`

None. The strongest apparent candidate was
`issues/panic-path/panic-console-capture-untested.md`, but it fails the deletion
bar: the newer Loom tests exercise the factored latch/access primitives, not
the kernel's `capture()` function. Replacing `capture()` with an immediate
return would still leave those models green, and current source says this
explicitly (`kernel/src/drivers/panic_console/mod.rs:463-471`). The models close
the writer/reader exclusion gaps; they do not close this older no-op-testing
gap.

## File-by-file verdicts

1. **UNCHECKABLE** — `issues/audio/cpal-backend-hardcodes-the-format.md`.
   The asserted implementation is in the external Rust/CPAL fork, not present
   as source in this worktree. Checking out the pinned dependency and comparing
   its negotiated format with the device's offered format would settle it.
2. **LIVE** — `issues/audio/desktop-session-put-26ms-of-silence.md`.
   The mixer computes the corrected `target` but records `t_est` in `armed_on`
   (`userland/soundd/src/mix.rs:261-273`), then attributes wake latency to that
   stale estimate (`userland/soundd/src/mix.rs:367-377`).
3. **LIVE** — `issues/audio/disk-wait-pins-a-cpu.md`. Mass-storage operations
   still run while `with_disk` owns the controller (`kernel/src/drivers/xhci/wait/msc.rs:1-5,1132-1142`), and the block wait remains synchronous
   (`kernel/src/block.rs:58`).
4. **UNCHECKABLE** — `issues/audio/doom-audio-callback-stalled-on-the-t14.md`.
   A fresh T14 capture naming the callback thread's state would distinguish a
   surviving stall from the old sighting.
5. **UNCHECKABLE** — `issues/audio/gate-a-first-run-to-record-its-host.md`.
   The next attributed Gate-A session must record the host and control arm; no
   permitted host-only command can supply that observation.
6. **LIVE** — `issues/audio/gate-a-has-no-runner-baseline.md`. The schema is
   still `BTreeMap<test, BTreeMap<smp, entry>>` and selection takes only name
   and SMP (`tests/toyos.rs:2148,2165-2169`), so it has no host dimension.
7. **PARTLY-FIXED** — `issues/audio/gate-a-suspend-structure-verdict-unread.md`.
   The workflow/shard routing described as missing now exists, but the
   first-shard suspend distribution is still an observation without a recorded
   rate; an attributed Gate-A run settles that residual.
8. **UNCHECKABLE** — `issues/audio/hda-ring-fix-unverified-on-metal.md`. The
   named exit is a metal HDA run; current source cannot certify electrical DMA
   behavior.
9. **UNCHECKABLE** — `issues/audio/hda-tone-phase-check.md`. A fresh independent
   capture of the HDA stream is required to decide the phase claim.
10. **UNCHECKABLE** — `issues/audio/hda-tone-red-beyond-its-exemption.md`. The
    current pipeline must be sampled under the recorded load shape; static
    source does not decide whether the mid-tone gap recurs.
11. **UNCHECKABLE** — `issues/audio/idle-suspend-reds-on-a-loaded-host-and-on-main.md`.
    An attributed quiet/load A/B using the idle-suspend instrument settles it.
12. **UNCHECKABLE** — `issues/audio/null-sink-applies-one-connect.md`. The
    remaining claim is T14-only; a blocked-task dump on a recurrence is the
    file's discriminating observation.
13. **LIVE** — `issues/audio/stop-the-device-voice-keep-the-wake.md`. The mix
    loop still couples client signaling, device progress, and wake accounting
    in one loop (`userland/soundd/src/mix.rs:248-302,350-408`); no separate
    device-voice lifetime exists.
14. **UNCHECKABLE** — `issues/audio/t14-wake-lateness-is-bimodal-per-boot.md`.
    Only a new per-boot T14 sample can establish whether the two modes remain.
15. **UNCHECKABLE** — `issues/audio/thorough-tier-reds-on-unmodified-main.md`.
    The claimed rates require a new attributed thorough-tier session.
16. **UNCHECKABLE** — `issues/boot-media/kernel-log-file-reds-beside-other-guests-and-is-green-alone.md`.
    Repeating the same arm with a session ledger is required to decide the
    recorded coexistence claim.
17. **LIVE** — `issues/boot-media/log-flush-retry-reds-two-ways-at-two-in-five.md`.
    `log_flush_retry` remains a Nightly test (`tests/toyos.rs:931,8161`), while
    `rg -n 'log_flush_retry' src/redlist.rs` finds only explanatory prose and no
    row (`src/redlist.rs:2828,2849`), so the record's missing adjudication is
    still true.
18. **UNCHECKABLE** — `issues/boot-media/usb-short-read-reds-beside-other-guests-and-is-green-alone.md`.
    A same-session alone/coexistence sample with attribution would settle it.
19. **LIVE** — `issues/build/a-harness-injected-program-can-be-endowed-with-nothing.md`.
    Build validation ensures a started program is declared
    (`src/build.rs:2190-2207`) but the program row's empty authority remains a
    legal shape; declaration and endowment are still separate checks.
20. **UNCHECKABLE** — `issues/build/a-loaded-suite-reds-a-volume-checker-on-both-arms.md`.
    The required quiet-host interleaved A/B was not available and no static
    line selects between scheduler placement and host interference.
21. **LIVE** — `issues/build/contention-has-no-owning-instrument.md`. The host
    slot/build-slot mechanism says when callers wait (`src/buildlock.rs:237-314`)
    but has no per-session attribution ledger for outside work; the negative
    `rg` search found no such record type.
22. **LIVE** — `issues/build/debug-true-produces-no-debug-info.md`. The profile
    still enables debuginfo (`Cargo.toml:196`) and the linker still drops debug
    sections (`toyos-ld/src/collect.rs:412-413`).
23. **LIVE** — `issues/build/every-worktree-builds-its-own-copy-of-the-same-crates.md`.
    Worktrees deliberately retain their own ten target directories
    (`src/worktree.rs:253-255`); no shared target-dir policy exists.
24. **UNCHECKABLE** — `issues/build/fork-branches-have-no-upstream.md`. The
    claim concerns clones/forks outside this checkout; inspecting each fork's
    branch configuration would settle it.
25. **UNCHECKABLE** — `issues/build/fork-estate-outside-the-warning-bar.md`.
    The estate outside this checkout must be enumerated and compared with the
    warning bar; this tree alone cannot do that.
26. **PARTLY-FIXED** — `issues/build/free-memory-verdicts-share-a-boot.md`.
    The false-leak cases now have settling/census controls, but machine-wide
    free-memory remains a shared-boot quantity; the file must narrow to that
    residual rather than be deleted.
27. **UNCHECKABLE** — `issues/build/memmap2-fork-is-unreachable-code.md`. The
    relevant fork source is not available in this worktree; checking the pinned
    submodule plus a target reachability build settles it.
28. **UNCHECKABLE** — `issues/build/mio-deregister-fd-leaves-a-pending-poll-live.md`.
    The claim is in the external Rust/mio fork; its current source and a focused
    poll model are needed.
29. **PARTLY-FIXED** — `issues/build/nothing-checks-the-dependency-bar.md`.
    Host clippy/source gates now cover named dependencies
    (`src/sourcegate.rs:1-5`), but there is still no complete dependency-policy
    ledger or gate over every dependency class.
30. **LIVE** — `issues/build/page-cache-owns-one-device.md`. The cache still
    owns one global `BLOCK_DEV` and initializes it by replacement
    (`kernel/src/page_cache.rs:10-27`).
31. **PARTLY-FIXED** — `issues/build/parallel-tests-red-under-other-suites.md`.
    Cross-worktree guest/build slots now exist (`src/buildlock.rs:237-314`),
    but they neither attribute outside contention nor make the host-wide
    timing/rate verdicts guest-clock based; those claims remain.
32. **LIVE** — `issues/build/poll-wake-pipe-bound-is-a-host-of-the-day-number.md`.
    The committed redlist entry still sources this host-specific bound
    (`src/redlist.rs:2571`); no source-derived or guest-clock bound replaced it.
33. **PARTLY-FIXED** — `issues/build/python-and-cc-are-declared.md`. `cc` and
    Python are now named in tool discovery (`src/main.rs:18-30`), but discovery
    still reports rather than bootstraps them; the installation half remains.
34. **LIVE** — `issues/build/readdir-bound-is-priced-six-times-under-its-cost.md`.
    The two committed values remain 8,578 ms
    (`src/tiers.rs:312-313`; `tests/test-durations:288`), so no new measured
    nightly price has replaced the stale one.
35. **UNCHECKABLE** — `issues/build/std-fork-not-rustfmt-clean.md`. The Rust
    submodule source/toolchain is not available for the required rustfmt diff.
36. **UNCHECKABLE** — `issues/build/std-leaks-a-thread-stack-per-spawn.md`. The
    std implementation is external here; checking its current thread teardown
    and running the specified allocation census would settle it.
37. **UNCHECKABLE** — `issues/build/std-systemtime-now-returns-the-epoch.md`.
    The current std fork and a clock comparison are required.
38. **LIVE** — `issues/build/the-build-system-does-not-compile-on-windows.md`.
    The build system still imports and calls Unix-only symlink support
    unconditionally (`src/toolchain.rs:1813`).
39. **UNCHECKABLE** — `issues/build/the-console-input-path-can-stop-after-a-ps2-overflow.md`.
    Existing armed measurements disagree; a wedged boot with `RX_BYTES`
    observed while injection continues is the stated discriminator.
40. **UNCHECKABLE** — `issues/build/the-gate-is-a-full-suite.md`. The remaining
    record is an unexplained staged-image disappearance; only reproducing it
    with staging lifecycle observation can establish a current mechanism.
41. **LIVE** — `issues/build/the-shard-split-prices-a-boot-and-not-the-image-behind-it.md`.
    `Shard::keep` still accepts one scalar duration per task
    (`src/testargs.rs:72-95`), and the sole committed profile contains test
    prices, not per-config image-build costs (`tests/test-durations:1`).
42. **LIVE** — `issues/build/the-t14-runner-is-trusted-not-isolated.md`. Runner
    routing still treats the T14 as a host rather than a disposable isolation
    boundary; no sandbox/restore mechanism exists in the workflow-side source.
43. **UNCHECKABLE** — `issues/build/xhci-full-speed-device-jumped-47-percent-over-its-commitment.md`.
    The percentage is a runner measurement; a current attributed repetition is
    required, and no source line can validate it.
44. **LIVE** — `issues/design-debt/kernelslice-outlives-its-allocation.md`.
    `KernelSlice` remains `Copy` and carries no allocation lifetime
    (`kernel/src/mm/region.rs:13-15,62-69`).
45. **UNCHECKABLE** — `issues/design-debt/std-says-this-machine-has-one-cpu.md`.
    The relevant std source is external in this checkout; the pinned source and
    a `available_parallelism` comparison settle it.
46. **PARTLY-FIXED** — `issues/design-debt/what-is-owed-on-file-size.md`. Most
    size paths now have bounds/tests, but the ACPI extraction path remains the
    explicitly unverified residual; the file should narrow to it.
47. **LIVE** — `issues/diagnostics/a-record-cannot-name-thread-zero.md`. The log
    record still uses zero as the no-thread sentinel, so TID 0 is
    unrepresentable as a named thread (`toyos-abi/src/log.rs:20-31`).
48. **PARTLY-FIXED** — `issues/diagnostics/blocked-dump-cannot-fire-on-a-total-freeze.md`.
    Ctrl+Alt+D now handles responsive CPUs, but its request is still serviced
    from guest execution (`kernel/src/sched/dump.rs:128-150`); a freeze in which
    no vCPU executes still needs the QMP/NMI actuator.
49. **LIVE** — `issues/diagnostics/blocked-time-is-invisible-while-the-park-lasts.md`.
    Blocked accounting is still finalized on the wake/return path rather than
    exposed as a live interval (`kernel/src/sched/dump.rs:72-123`).
50. **LIVE** — `issues/filesystem/a-shrink-unflushed-regrows-the-old-tail.md`.
    Shrink settlement and backing flush are still separate phases in the VFS
    flush plan (`kernel/src/vfs.rs:429-460`), leaving the recorded failed-flush
    regrowth shape reachable.
51. **LIVE** — `issues/filesystem/a-spawn-reads-round-an-open-file-s-dirty-pages.md`.
    `open_backing` still resolves through the filesystem rather than the live
    file cache (`kernel/src/vfs.rs:676-680`).
52. **LIVE** — `issues/filesystem/fat-replace-rename-swallows-its-rollback-failure.md`.
    The rollback result is still discarded (`toyos-fat32/src/fs.rs:909-913`).
53. **UNCHECKABLE** — `issues/filesystem/fat-unlink-reallocate-leaks-a-cluster-under-load.md`.
    A current loaded run plus `toyos-fat32-check` on the resulting volume is
    needed to determine whether the leak survives.
54. **LIVE** — `issues/filesystem/fat32-suite-needs-macos-binaries.md`. The
    independent FAT lane still depends on uncommittable macOS binaries; no
    in-tree second implementation has replaced them.
55. **LIVE** — `issues/filesystem/readdir-bound-is-per-mount.md`. `Vfs::list`
    still applies one `MAX_LIST_ENTRIES` budget after aggregating the mount
    (`kernel/src/vfs.rs:145,354-375`), rather than exposing a continuation.
56. **UNCHECKABLE** — `issues/filesystem/std-stat-conflates-io-with-notfound.md`.
    The mapping is in the external std fork; inspecting that source and a
    faulting-stat differential settles it.
57. **MISDESCRIBED** — `issues/filesystem/usb-audit-findings-not-in-vcs.md`.
    This is not three described defects: it contains no mechanism, source site,
    or exit condition for F-D/F-F/F-I (`issues/filesystem/usb-audit-findings-not-in-vcs.md:7-13`).
    The correct record is “three audit claims are unauditable because task
    #145's evidence was never copied into VCS”; obtaining that text is required
    before any code-defect verdict exists.
58. **PARTLY-FIXED** — `issues/filesystem/usb-esp-gate-holes.md`. The tree now
    has FAT structural/durability checks for some named holes, but the file's
    remaining independent-reader and failure-window arms are not all gated; it
    must be narrowed claim by claim.
59. **UNCHECKABLE** — `issues/hardware/a-bar-sharing-the-scanout-page.md`. The
    condition depends on a real firmware BAR/framebuffer topology; a dumped
    memory map and BAR inventory from the same boot settle it.
60. **LIVE** — `issues/hardware/anonymous-mmap-is-not-demand-paged.md`. Anonymous
    mappings are still backed by eager `PageAlloc` ownership and BSS uses the
    same eager allocation path (`kernel/src/arch/syscall/vm.rs:19-61`;
    `kernel/src/loader/mod.rs:399-459`).
61. **PARTLY-FIXED** — `issues/hardware/eleven-names-red-on-ci.md`. Several
    names now have fixes/adjudications, but the current redlist retains members
    of the recorded class; the eleven-name historical bundle should be narrowed
    to the rows still present, not treated as one live defect.
62. **PARTLY-FIXED** — `issues/hardware/four-runner-reds-unclassified.md`. The
    null-audio case is closed and the HDA stall's old idle-filesystem path is
    gone; the Doom and SSH runner-only observations remain unclassified.
63. **LIVE** — `issues/hardware/gop-path-off-by-default.md`. GOP is still an
    explicit `--gop` profile rather than the default (`src/main.rs:271-277`),
    and the bootloader still chooses the highest-resolution supported mode
    (`bootloader/src/main.rs:416-434`).
64. **LIVE** — `issues/hardware/hotplug-blocks-a-scheduler-pass.md`. Hotplug
    configuration is still performed from the driver's pending-work path, and
    no scheduler-owned deferred-callback facility replaced it
    (`kernel/src/drivers/xhci/mod.rs:1302-1321`).
65. **MISDESCRIBED** — `issues/hardware/kernel-log-unreadable-once-userland-owns-the-screen.md`.
    The durable-log claim is closed by logd, while the actual residual is live
    on-machine readability without input. The record itself admits its cited
    compositor and panic-console line numbers drifted
    (`issues/hardware/kernel-log-unreadable-once-userland-owns-the-screen.md:175-184`);
    current screen ownership is handled at
    `kernel/src/drivers/panic_console/mod.rs:688-710`. Correct description:
    “the shipping desktop has no input-independent surface for a post-claim
    kernel/logd message.”
66. **MISDESCRIBED** — `issues/hardware/metal-boot-accounting-is-stale.md`.
    It is a historical measurement ledger, not a current tree behavior: its
    own title/body asks for a new metal number. Correct description: “the next
    metal session owes a fresh boot-phase sample”; the old 1,151 ms does not
    prove a current defect.
67. **LIVE** — `issues/hardware/pre-flash-gate-missed-the-milestone.md`. The
    pre-flash checks still cannot certify integrated input on the target and no
    committed gate supplies that missing observation.
68. **UNCHECKABLE** — `issues/hardware/process-start-skew-on-a-runner.md`. A
    current runner sample is needed to determine whether skew remains after the
    placement changes.
69. **PARTLY-FIXED** — `issues/hardware/pulling-the-boot-stick-freezes-the-t14.md`.
    Storage recovery, TLB acknowledgement, and panic visibility changed, but
    the serial-less total-freeze mechanism has not been re-observed or ruled
    out; the record must narrow to the surviving freeze experiment.
70. **LIVE** — `issues/hardware/t14-hands-over-an-uninitialised-8042.md`. The
    driver still accepts firmware-owned controller state and the policy for a
    full reinitialization versus fail-closed handoff is not encoded
    (`kernel/src/drivers/i8042/mod.rs:930-1015`).
71. **PARTLY-FIXED** — `issues/hardware/t14-keyboard-will-not-report-its-scancode-set.md`.
    The translation fallback now permits attachment, but the actual wire/set
    identity remains unknown; the file should retain only that observation.
72. **PARTLY-FIXED** — `issues/hardware/t14-lost-every-integrated-input.md`.
    Diagnostic/health paths landed and the keyboard fallback changed, but the
    integrated-input outcome on the T14 has not been re-established.
73. **PARTLY-FIXED** — `issues/hardware/tearing-is-what-gop-cannot-give-back.md`.
    Exit-path accounting and a guest mode-change gate landed, but GOP still has
    no page flip, several clients still present whole windows
    (`userland/window/src/lib.rs:602`), and scanout/client damage work remains.
74. **UNCHECKABLE** — `issues/hardware/the-t14-mouse-may-be-another-defect.md`.
    A T14 boot with the keyboard path established and pointer interrupts traced
    is required to determine whether this is independent.
75. **PARTLY-FIXED** — `issues/hardware/xhci-flap-wedges-under-kvm.md`. The
    reproducible BOT reset race is structurally closed by quiescing both
    endpoints before the class reset (`kernel/src/drivers/xhci/wait/msc.rs:819-845`),
    but the original fourth collapsed-replug silence was never reduced and is
    not made impossible by that mass-storage fix.
76. **PARTLY-FIXED** — `issues/hardware/xhci-waits-are-spins.md`. Teardown and
    endpoint recovery moved out of scheduler-pass waits, but the block-storage
    interface is still synchronous under `with_disk`
    (`kernel/src/drivers/xhci/wait/msc.rs:1132-1142`).
77. **UNCHECKABLE** — `issues/isolation/a-broken-pipe-answers-not-found.md`. The
    kernel/libc half is closed, but the only residual is in the external std
    fork; its pinned source plus a Rust `BrokenPipe` differential settle it.
78. **LIVE** — `issues/isolation/a-moved-handle-is-always-re-movable.md`. Handle
    transfer still carries no non-transferable right/state in the ABI
    (`toyos-abi/src/handle.rs:1-35`).
79. **UNCHECKABLE** — `issues/isolation/a-provided-name-cannot-reach-an-undeclared-child.md`.
    The residual is in external std/process plumbing; inspecting the pinned
    fork and running the provided-name arm settle it.
80. **LIVE** — `issues/isolation/a-received-handle-has-no-knowable-type.md`. The
    received ABI value remains a raw handle without a queryable kind
    (`toyos-abi/src/handle.rs:1-35`).
81. **LIVE** — `issues/isolation/bus-mastering-rides-memory-decode.md`. HDA,
    NVMe, and xHCI still enable PCI bus mastering in bring-up
    (`kernel/src/drivers/hda.rs:445`; `kernel/src/drivers/nvme.rs:726`;
    `kernel/src/drivers/xhci/wait/boot.rs:214`) without an ownership state that
    separates it from decode.
82. **LIVE** — `issues/isolation/dtv-capacity-is-a-workload-bound.md`. The
    loader still hard-caps the DTV at 64 (`kernel/src/loader/tls.rs:13`).
83. **LIVE** — `issues/isolation/kernelslice-over-user-memory.md`. `KernelSlice`
    remains `Copy` and `as_slice` manufactures a reference without a lifetime
    tied to the allocation (`kernel/src/mm/region.rs:13-15,62-69`).
84. **LIVE** — `issues/isolation/libc-loses-the-kernels-word-on-three-write-paths.md`.
    `send`, `sendto`, and stdio still collapse or omit the kernel error
    (`userland/libc/src/socket.rs:354,430-431`;
    `userland/libc/src/stdio.rs:32`).
85. **LIVE** — `issues/isolation/no-physical-memory-fairness.md`. PMM accounting
    remains category/machine-wide rather than enforceable per-process ownership
    (`kernel/src/mm/pmm.rs:116-184`).
86. **PARTLY-FIXED** — `issues/isolation/probe-mounts-on-a-checksum.md`. Extent
    bounds/read-link checks landed, but probe identity is still accepted from a
    weak checksum and restamped without a stronger ownership/authenticity
    policy (`kernel/src/bcachefs_adapter.rs:500-526`;
    `bcachefs/src/superblock.rs:184-204`).
87. **LIVE** — `issues/isolation/so-cache-never-evicts.md`. Cache entries are
    still push-only and their allocation is deliberately immortal
    (`kernel/src/elf/cache.rs:146-149,200-216`).
88. **LIVE** — `issues/isolation/sshd-accept-path-unexercised.md`. The shipping
    accept loop remains, while source search finds no source-based SSH client
    exercising it (`userland/sshd/src/main.rs:150-235`).
89. **LIVE** — `issues/isolation/sshd-authorized-keys-unprotected.md`. Authorized
    keys remain filesystem content without a permission/capability ownership
    check (`userland/sshd/src/main.rs:70-105`).
90. **UNCHECKABLE** — `issues/isolation/t14-desktop-froze-at-64s.md`. A new T14
    recurrence with scheduler/panic observation is required; the historical
    timestamp alone cannot establish a live mechanism.
91. **LIVE** — `issues/isolation/toybox-is-one-row-for-nineteen-applets.md`. The
    manifest still grants authority to the toybox program row rather than the
    invoked applet (`system.toml:35-55`).
92. **LIVE** — `issues/isolation/undefined-flag-bits-cross-the-boundary-and-are-dropped.md`.
    `SYS_MMAP`, `SYS_OPEN`, `SYS_PROCESS_WAIT`, watch flags, and
    `Submission::flags` still lack complete unknown-bit refusal
    (`kernel/src/arch/syscall/vm.rs:19-35`;
    `kernel/src/object/ops.rs:70-78`; `kernel/src/inbox.rs:119-130`;
    `toyos-abi/src/inbox.rs:35`).
93. **PARTLY-FIXED** — `issues/isolation/untrusted-sites-not-yet-adopted.md`.
    Several named boundaries now refuse hostile inputs, but the remaining
    syscall conversions and device-failure actuators are not all adopted; the
    bundle must be narrowed to its still-unconverted sites.
94. **UNCHECKABLE** — `issues/kernel/a-double-fault-on-cpu-1-under-a-wide-suite.md`.
    A new attributed capture or a total-freeze actuator is needed; the one
    historic panic cannot identify a current source mechanism.
95. **UNCHECKABLE** — `issues/kernel/ap-control-registers-inherit-init.md`. The
    slug's code defect is closed, but the current file explicitly tracks the
    silicon-only performance number. The instrument exists
    (`kernel/src/arch/control_regs.rs:277-280`); running it on metal settles the
    remaining claim.
96. **LIVE** — `issues/kernel/ap-tsc-trail-is-assumed-and-never-checked.md`. AP
    time still relies on a cross-CPU TSC relationship without a boot-time
    skew/monotonicity check (`kernel/src/clock.rs:1-45`).
97. **LIVE** — `issues/kernel/deferred-release-outlives-its-syscall.md`.
    `drain_zero_handles` still clears `ZERO_PENDING` and takes the whole batch
    before running hooks (`kernel/src/object/mod.rs:262-275`), so another CPU
    can own in-flight release work while the queuer observes no pending batch.
98. **PARTLY-FIXED** — `issues/kernel/desktop-window-child-freeze.md`. The
    harness double-close/amplifier was fixed, but the freeze itself remains
    unlocalized and its faithful reproduction depends on the harness-injected
    program receiving real endowment.
99. **LIVE** — `issues/kernel/dlopen-dedup-only-holds-after-the-race-settles.md`.
    Cache lookup and registration remain separate lock acquisitions
    (`kernel/src/elf/cache.rs:149-203`), so two loaders can both miss and push.
100. **UNCHECKABLE** — `issues/kernel/echo-faulted-after-the-fault-arms.md`. A
    fresh fault-arm run with capture is required to tell whether the recorded
    failure survives.
101. **LIVE** — `issues/kernel/granularity-bound-crossed-at-four-widths.md`. The
    simulator's current bound is still crossed by the named width configurations
    (`toyos-sched/sim/src/invariants.rs:583-586,720-725`;
    `toyos-sched/sim/src/sweep.rs:150-156`).
102. **UNCHECKABLE** — `issues/kernel/i13s-margin-at-32-cpus-is-down-to-two-milliseconds.md`.
    The two-millisecond figure is a measurement; rerunning the host simulator
    at 32 CPUs is required to validate it at this HEAD.
103. **PARTLY-FIXED** — `issues/kernel/io-uring-enter-trips-the-one-queue-invariant.md`.
    The captured multi-queue path is gone because waits use the task's own
    park queue, but no model or recurrence proves the stale waiting flag's root
    cause impossible; the record should retain only that narrower uncertainty.
104. **PARTLY-FIXED** — `issues/kernel/kernel-hashmaps-take-userland-chosen-keys.md`.
    `created_dirs` now refuses above 16,384 entries
    (`kernel/src/vfs.rs:147-149,521-524`), closing the unbounded-count claim;
    hashbrown's default hasher and user-chosen strings remain
    (`kernel/Cargo.toml:368`; `kernel/src/vfs.rs:138,524`).
105. **LIVE** — `issues/kernel/keyboard-flood-panics-blocked-read.md`. The
    waiting-state assertion still exists (`toyos-sched/src/waitq.rs:124`) and
    the keyboard read loop still repeatedly prepares waits
    (`kernel/src/arch/syscall/io.rs:55-90`).
106. **LIVE** — `issues/kernel/lseek-past-eof-is-silently-clamped.md`. Seek still
    clamps the result to the current file size (`kernel/src/object/ops.rs:451`).
107. **LIVE** — `issues/kernel/no-alloc-error-handler.md`. The kernel still has
    no `#[alloc_error_handler]`; allocation failure remains outside a named
    refusal path (negative `rg` over `kernel/src`).
108. **LIVE** — `issues/kernel/one-mapping-is-written-in-two-ledgers.md`.
    `ProcessData::mmap_regions` and `AddressSpace::regions` both remain live
    state (`kernel/src/process.rs:742`; `kernel/src/mm/paging.rs:546`), with
    syscall code keeping them in step manually
    (`kernel/src/arch/syscall/vm.rs:19-115`).
109. **UNCHECKABLE** — `issues/kernel/past-eof-holes-wedge-a-shared-boot.md`.
    The enabling change is intentionally reverted; a restored fix plus the
    prescribed isolated/full-tier arms and a first-waiter capture are required.
110. **LIVE** — `issues/kernel/process-open-panics-on-a-reopened-process.md`.
    The reopened-process path still reaches the retired-state assertion
    (`kernel/src/object/handle.rs:109`).
111. **LIVE** — `issues/kernel/retire-tripwire-is-not-queue-shaped.md`. The
    fixed ten-second `GIVE_UP` tripwire remains in `retire_task`
    (`kernel/src/scheduler.rs:538-568`).
112. **PARTLY-FIXED** — `issues/kernel/scheduler-pass-blocks-in-xhci.md`.
    Several endpoint/teardown waits moved to stepped operations, but storage
    calls still synchronously own `with_disk`
    (`kernel/src/drivers/xhci/wait/msc.rs:1132-1142`); the file must narrow to
    the remaining pass/storage sites.
113. **LIVE** — `issues/kernel/soundd-past-due-wake-max-1.md`. The soundd wake
    accounting still records a maximum-one symptom rather than enforcing a
    queue-shaped wake contract (`userland/soundd/src/mix.rs:350-385`).
114. **LIVE** — `issues/kernel/spawn-thread-disagrees-about-a-reaped-parent.md`.
    Spawn still reads process liveness and reaping through distinct state
    transitions without a single primitive deciding the race
    (`kernel/src/process.rs:980-1060`).
115. **PARTLY-FIXED** — `issues/kernel/spawned-process-never-starts.md`. The
    known harness amplifier was removed, but a task can still be published yet
    never observed running; the root scheduler state transition remains
    unmodeled (`kernel/src/scheduler.rs:300-390`).
116. **LIVE** — `issues/kernel/steal-probe-node-dies-with-its-victim.md`. The
    steal probe still publishes a node whose lifetime is tied to the victim
    while another CPU can answer it (`toyos-sched/src/cpu.rs:2055,2247`).
117. **LIVE** — `issues/kernel/syscall-preemption-is-incidental.md`. Syscall
    entry still relies on incidental preemption/interrupt state rather than an
    explicit entry contract (`kernel/src/arch/syscall/gate.rs:36-68`).
118. **UNCHECKABLE** — `issues/kernel/syscall-window-nmi-shortfalls-on-a-contended-host.md`.
    The shortfall is a host-load rate; only an attributed NMI session can decide
    whether it persists.
119. **LIVE** — `issues/kernel/the-blocked-task-dump-panics-when-a-cpu-is-inside-inbox-submit.md`.
    `dump::request` still asserts preempt depth at most one
    (`kernel/src/sched/dump.rs:128-134`), while `pass_block` can service pending
    IRQ work inside a nested wait; the direct contradiction remains.
120. **LIVE** — `issues/kernel/the-global-pipe-lock-spans-a-user-copy.md`. One
    global `PIPES` lock still encloses `Ring::read`/`write` and the user copy
    (`kernel/src/pipe.rs:174,181-184,210-254`).
121. **UNCHECKABLE** — `issues/kernel/the-split-window-tlb-cost-is-unpriced.md`.
    The claim is a metal timing cost; a same-session split-window A/B is needed.
122. **LIVE** — `issues/kernel/thread-exits-completion-post-is-the-second-one.md`.
    Exit still publishes release and separately posts completion, leaving two
    notifications for one transition (`kernel/src/sched/payload.rs:137-170`).
123. **LIVE** — `issues/kernel/volatile-composites-on-mmio-dma-structs.md`. MMIO
    and DMA code still performs composite volatile reads/writes without a
    project-wide primitive defining field ordering: generic `Dma::read/write`
    still accept any `T` (`kernel/src/mm/dma.rs:114-125`), including whole
    queue entries (`kernel/src/drivers/nvme.rs:140-178` and
    `kernel/src/drivers/xhci/wait/boot.rs:344`).
124. **PARTLY-FIXED** — `issues/panic-path/a-double-panic-at-boots-edge-says-nothing-but-its-name.md`.
    Panic capture/reporting now has exclusion and tests, but the early-boot
    loaded double-panic cause remains an observation without attribution.
125. **LIVE** — `issues/panic-path/crash-report-preemption-untested.md`. Current
    crash-report paths read preemption/process state, while no model covers
    preemption at each read (`kernel/src/arch/idt/exceptions.rs:181-354`).
126. **LIVE** — `issues/panic-path/no-console-between-boot-and-terminal.md`. The
    successful-boot screen stops at userland ownership and the persistent file
    is not an on-screen, input-independent console
    (`kernel/src/drivers/panic_console/mod.rs:688-710`).
127. **LIVE** — `issues/panic-path/panic-console-capture-untested.md`. The newer
    Loom models exercise `CaptureLatch`/`CaptureAccess`, but none calls the
    kernel's `capture()`; a no-op body would leave every model green. The
    current function's own comment still records exactly that missing
    discriminator (`kernel/src/drivers/panic_console/mod.rs:463-471`).
128. **LIVE** — `issues/panic-path/panic-holding-process-table-hangs.md`. Panic
    reporting still attempts process-table-derived context on paths that may
    have interrupted its owner: recovery later reaches an unconditional table
    lock (`kernel/src/scheduler.rs:607-618`).
129. **LIVE** — `issues/panic-path/panic-on-wedged-virtio-console-spins.md`. The
    panic console still has a synchronous virtio-console fallback with no
    bounded device-failure result (`kernel/src/drivers/virtio_console.rs:180-250`).
