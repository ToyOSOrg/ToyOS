# Readiness of live defect records

Scope: the 63 `LIVE` plus 24 `PARTLY-FIXED` entries in `STALENESS.md`, at
`cc3b1597935fa6b584c48e077ea38a4beae6890d`. This is 87 entries, checked as a
set against `STALENESS.md`. `NEEDS-ORACLE` rows explicitly map to the twelve
families in `/Users/jan/Dev/jan/toyos-codex5/INSTRUMENTS.md`; “not covered”
means none of those twelve supplies the missing observation.

## READY

The order is strongest judge first: independent format checker, focused model
or simulator, host/source gate, then a guest assertion/differential.

- `issues/filesystem/fat-replace-rename-swallows-its-rollback-failure.md` —
  **READY**: the two-refusal `BlockAccess` arm drives real `Fat32`, while the
  independent `toyos-fat32-check` implementation judges the resulting volume
  (`toyos-fat32/src/fs.rs:903-923`).
- `issues/filesystem/usb-esp-gate-holes.md` — **READY**: extend the existing ESP
  host reread with `toyos-fat32-check`; its builder/checker is independent of
  the mounted writer (`tests/toyos.rs:980-1000`).
- `issues/kernel/dlopen-dedup-only-holds-after-the-race-settles.md` — **READY**:
  a focused concurrent cache test can judge one registered allocation/path and
  a reverted whole change restores two; the lock boundaries are local to
  `kernel/src/elf/cache.rs:149-203`.
- `issues/kernel/granularity-bound-crossed-at-four-widths.md` — **READY**: the
  in-tree `toyos-sched` simulator and `granularity` tests are the judge
  (`toyos-sched/sim/src/invariants.rs:583-586,720-725`).
- `issues/filesystem/readdir-bound-is-per-mount.md` — **READY**: extend the
  existing bounded list tests with two mounts and continuation/exhaustion;
  `Vfs::list` is local (`kernel/src/vfs.rs:354-375`).
- `issues/build/nothing-checks-the-dependency-bar.md` — **READY**: the host
  source gates are the judge; add the remaining dependency classes to the same
  parsed manifest/source checks (`src/sourcegate.rs:1-120`).
- `issues/build/the-shard-split-prices-a-boot-and-not-the-image-behind-it.md` —
  **READY**: the `Shard::keep` unit tests can judge complete, unique partitioning
  and bin totals with a separate config-build cost (`src/testargs.rs:31-103`).
- `issues/build/the-build-system-does-not-compile-on-windows.md` — **READY**:
  the Windows-target host compile is the judge; the fix is the local
  Unix-symlink dependency at `src/toolchain.rs:1813`.
- `issues/audio/desktop-session-put-26ms-of-silence.md` — **READY**: factor the
  chosen deadline and judge it with soundd's existing pure timing/DLL unit tests
  (`userland/soundd/src/mix.rs:254-273,367-377`).
- `issues/audio/stop-the-device-voice-keep-the-wake.md` — **READY**: the existing
  null-sink and shipped-client audio arms judge that stopping the device voice
  does not lose the command-pipe wake (`userland/soundd/src/mix.rs:631-763`).
- `issues/boot-media/log-flush-retry-reds-two-ways-at-two-in-five.md` — **READY**:
  the record's local exit is a measured `src/redlist.rs` row, judged by
  `cargo run -- --known-red log_flush_retry`; the test dispatch is
  `tests/toyos.rs:931,8161`.
- `issues/build/free-memory-verdicts-share-a-boot.md` — **READY**: replace the
  remaining machine-wide verdict with the existing per-kind object census and
  its same-process baseline; that is independent of unrelated allocations.
- `issues/design-debt/what-is-owed-on-file-size.md` — **READY**: the remaining
  ACPI extraction case can use the existing file-size boundary/differential
  tests as its judge; the closed claims need only be removed from the record.
- `issues/diagnostics/blocked-time-is-invisible-while-the-park-lasts.md` —
  **READY**: extend `blocked_dump` so the report computes `now - parked_at` for
  a still-parked task; the existing dump test judges the live interval
  (`kernel/src/sched/dump.rs:72-123`).
- `issues/filesystem/a-shrink-unflushed-regrows-the-old-tail.md` — **READY**:
  the existing failed-flush actuator plus host volume checker can judge that a
  refused shrink never regrows old bytes (`kernel/src/vfs.rs:429-460`).
- `issues/filesystem/a-spawn-reads-round-an-open-file-s-dirty-pages.md` —
  **READY**: the existing dirty-backing spawn arm compares the child's bytes
  with the live file-cache bytes; the fix is local to `open_backing`
  (`kernel/src/vfs.rs:676-680`).
- `issues/isolation/libc-loses-the-kernels-word-on-three-write-paths.md` —
  **READY**: a C `send`/`sendto`/`fwrite` arm is judged against this libc's
  already-correct `write(2)` errno mapping
  (`userland/libc/src/socket.rs:354,430-431`; `userland/libc/src/stdio.rs:32`).
- `issues/isolation/undefined-flag-bits-cross-the-boundary-and-are-dropped.md` —
  **READY**: copy `endowment_denied`'s base/unknown-bit differential at each
  syscall; `SYS_NAMESPACE_BUILD` is the existing refusal precedent
  (`toyos-abi/src/syscall.rs:1340`).
- `issues/kernel/process-open-panics-on-a-reopened-process.md` — **READY**: the
  existing process-open syscall arm can reopen a retired process and require a
  refusal rather than the assertion at `kernel/src/object/handle.rs:109`.
- `issues/kernel/soundd-past-due-wake-max-1.md` — **READY**: Gate A's existing
  per-wake completion, lateness, and worst-batch record judges the queue-shaped
  fix (`userland/soundd/src/mix.rs:350-385`).
- `issues/panic-path/panic-console-capture-untested.md` — **READY**: strengthen
  `screen_late_panic` with a sibling record written after capture and assert the
  painted snapshot excludes it; replacing the real `capture()` body with
  `return` is the mandatory negative control
  (`tests/toyos.rs:3964-3999`; `kernel/src/drivers/panic_console/mod.rs:463-471`).

## NEEDS-ORACLE

- `issues/build/contention-has-no-owning-instrument.md` — **NEEDS-ORACLE**:
  missing observation is which suites/builders occupied the host during every
  verdict; instrument family 1 (attributed session ledger) covers it.
- `issues/build/parallel-tests-red-under-other-suites.md` — **NEEDS-ORACLE**:
  missing observation is a complete same-host overlap ledger tied to each red;
  family 1 covers it.
- `issues/build/poll-wake-pipe-bound-is-a-host-of-the-day-number.md` —
  **NEEDS-ORACLE**: missing observation is a source/guest-clock bound separated
  from host scheduling; family 1 covers the attribution half but not the bound
  itself.
- `issues/build/readdir-bound-is-priced-six-times-under-its-cost.md` —
  **NEEDS-ORACLE**: missing observation is the next nightly artifact's measured
  `readdir_bound` price; family 1 covers the attributed session, but the nightly
  duration merge still has to supply the number.
- `issues/design-debt/kernelslice-outlives-its-allocation.md` —
  **NEEDS-ORACLE**: missing observation is whether any returned slice remains
  usable after its allocation is released; family 5, the `KernelSlice`
  allocation/alias lifetime harness, covers it.
- `issues/diagnostics/blocked-dump-cannot-fire-on-a-total-freeze.md` —
  **NEEDS-ORACLE**: missing observation is state from CPUs that execute no guest
  instruction; family 2, the QMP/NMI total-freeze actuator, covers it.
- `issues/hardware/four-runner-reds-unclassified.md` — **NEEDS-ORACLE**: missing
  observation is an attributed current rate/capture for the two unresolved
  names; family 1 covers it.
- `issues/hardware/pulling-the-boot-stick-freezes-the-t14.md` —
  **NEEDS-ORACLE**: missing observation is the first non-progressing state on a
  serial-less hard freeze; family 2 covers it.
- `issues/hardware/pre-flash-gate-missed-the-milestone.md` —
  **NEEDS-ORACLE**: missing observation is whether the exact image about to be
  flashed accepts integrated keyboard and pointer input on the target; none of
  the twelve families covers target input (family 1 can attribute the session,
  but cannot create the observation).
- `issues/hardware/t14-keyboard-will-not-report-its-scancode-set.md` —
  **NEEDS-ORACLE**: missing observation is the actual controller translation
  bit plus bytes reported on the T14; none of the twelve families covers i8042
  wire identity (the existing i8042 trace must be used on metal).
- `issues/hardware/xhci-flap-wedges-under-kvm.md` — **NEEDS-ORACLE**: missing
  observation is the controller/port state at the original fourth collapsed
  replug; family 4, the xHCI completion/idle trace, covers it.
- `issues/isolation/bus-mastering-rides-memory-decode.md` — **NEEDS-ORACLE**:
  missing observation is that a staged post-decode refusal leaves mastering
  clear for HDA/NVMe/xHCI; none of the twelve families supplies PCI command-bit
  and per-driver failure actuators.
- `issues/isolation/kernelslice-over-user-memory.md` — **NEEDS-ORACLE**: missing
  observation is an alias remaining live across unmap/release; family 5 covers
  it.
- `issues/isolation/sshd-accept-path-unexercised.md` — **NEEDS-ORACLE**: missing
  observation is a protocol-independent client reaching the real accept path;
  family 10, the source-based SSH client, covers it.
- `issues/kernel/ap-tsc-trail-is-assumed-and-never-checked.md` —
  **NEEDS-ORACLE**: missing observation is per-AP skew and monotonicity against
  the BSP on real hardware; none of the twelve families covers TSC topology.
- `issues/kernel/io-uring-enter-trips-the-one-queue-invariant.md` —
  **NEEDS-ORACLE**: missing observation is a real task-state model proving every
  completion-wait exit clears `waiting`; none of the twelve families is that
  wait-state model.
- `issues/kernel/kernel-hashmaps-take-userland-chosen-keys.md` —
  **NEEDS-ORACLE**: missing observation is an actually colliding key set and its
  bucket/wall-time differential against the replacement hasher; none of the
  twelve families covers hash distribution.
- `issues/kernel/keyboard-flood-panics-blocked-read.md` — **NEEDS-ORACLE**:
  missing observation is a guest-side key generator that reproduces the stale
  wait flag without host pacing; none of the twelve families covers keyboard
  production.
- `issues/kernel/no-alloc-error-handler.md` — **NEEDS-ORACLE**: missing
  observation is deterministic failure at each allocation class; family 8, the
  allocator-failure actuator, covers it.
- `issues/kernel/one-mapping-is-written-in-two-ledgers.md` — **NEEDS-ORACLE**:
  missing observation is a differential inventory proving physical ownership,
  mappings, and teardown agree after every operation; family 6, the VM
  inventory/state model, covers it.
- `issues/kernel/scheduler-pass-blocks-in-xhci.md` — **NEEDS-ORACLE**: missing
  observation is per-operation time spent in the pass after each remaining
  storage/port path; family 4 covers it.
- `issues/kernel/spawn-thread-disagrees-about-a-reaped-parent.md` —
  **NEEDS-ORACLE**: missing observation is a real lifecycle-state model that
  interleaves spawn with parent reap; none of the twelve families covers
  process lifecycle (family 6 is VM state, not process state).
- `issues/kernel/spawned-process-never-starts.md` — **NEEDS-ORACLE**: missing
  observation is the scheduler state at the first total non-start; family 2
  supplies the total-freeze/NMI capture.
- `issues/kernel/steal-probe-node-dies-with-its-victim.md` —
  **NEEDS-ORACLE**: missing observation is every answering/victim interleaving
  through the real node primitive; family 7, the real `MailboxNode` Loom model,
  covers it.
- `issues/kernel/the-global-pipe-lock-spans-a-user-copy.md` —
  **NEEDS-ORACLE**: missing observation is per-CPU wait time on an unrelated pipe
  across a maximal copy and first allocation; none of the twelve families
  covers it (the instruments analysis explicitly rejects a generic ring
  shared-slice instrument as the wrong question).
- `issues/panic-path/a-double-panic-at-boots-edge-says-nothing-but-its-name.md` —
  **NEEDS-ORACLE**: missing observation is an attributed first panic before the
  second capture begins; families 1 and 2 together cover session attribution
  and the serial-less freeze.
- `issues/panic-path/crash-report-preemption-untested.md` —
  **NEEDS-ORACLE**: missing observation is every preemption point through the
  real crash-state primitive; family 9, the crash-preemption real-state model,
  covers it.

## NEEDS-DECISION

- `issues/build/a-harness-injected-program-can-be-endowed-with-nothing.md` —
  **NEEDS-DECISION**: must authority attach to the injected file/program row, or
  travel explicitly with the harness injection?
- `issues/build/debug-true-produces-no-debug-info.md` — **NEEDS-DECISION**: pay
  the measured debuginfo build cost, ship `debug=false`'s different codegen, or
  teach `toyos-ld` to preserve selected DWARF?
- `issues/build/every-worktree-builds-its-own-copy-of-the-same-crates.md` —
  **NEEDS-DECISION**: which artifacts may be shared without allowing one
  worktree to replace another's toolchain or stale path crates?
- `issues/build/python-and-cc-are-declared.md` — **NEEDS-DECISION**: should the
  build bootstrap host tools, print platform-specific installation steps, or
  remain declaration-only?
- `issues/build/page-cache-owns-one-device.md` — **NEEDS-DECISION**: use one
  cache instance per device or make device identity part of every cache and VFS
  ownership path?
- `issues/diagnostics/a-record-cannot-name-thread-zero.md` — **NEEDS-DECISION**:
  reserve a different no-thread encoding, widen the record, or make thread
  presence explicit?
- `issues/filesystem/fat32-suite-needs-macos-binaries.md` — **NEEDS-DECISION**:
  which independent, redistributable FAT implementation replaces the macOS
  driver as the committed judge?
- `issues/hardware/gop-path-off-by-default.md` — **NEEDS-DECISION**: should GOP
  become the default profile, and what aspect-ratio/current-mode policy replaces
  “largest mode wins”?
- `issues/hardware/t14-hands-over-an-uninitialised-8042.md` —
  **NEEDS-DECISION**: fully reinitialize the controller, preserve firmware
  translation state, or refuse a state the driver cannot identify?
- `issues/isolation/a-moved-handle-is-always-re-movable.md` —
  **NEEDS-DECISION**: is transferability a handle right, an object property, or
  a one-shot state consumed by send?
- `issues/isolation/a-received-handle-has-no-knowable-type.md` —
  **NEEDS-DECISION**: add an ABI kind query/tag, or require every receiving
  channel's schema to supply the type out of band?
- `issues/isolation/no-physical-memory-fairness.md` — **NEEDS-DECISION**: what
  per-process/accounting scope and refusal budget is enforceable for physical
  memory?
- `issues/isolation/probe-mounts-on-a-checksum.md` — **NEEDS-DECISION**: is the
  trust boundary authenticated media ownership, a keyed identity, or merely
  structural integrity with an explicit weak-threat model?
- `issues/isolation/so-cache-never-evicts.md` — **NEEDS-DECISION**: what byte or
  entry budget governs eviction, and who owns references while an entry can be
  removed?
- `issues/isolation/toybox-is-one-row-for-nineteen-applets.md` —
  **NEEDS-DECISION**: does authority follow the installed applet name, the
  executable inode, or a manifest-declared invocation identity?
- `issues/kernel/retire-tripwire-is-not-queue-shaped.md` — **NEEDS-DECISION**:
  replace the ten-second panic with which queue/liveness fact and what terminal
  refusal when release is lost?
- `issues/kernel/syscall-preemption-is-incidental.md` — **NEEDS-DECISION**:
  explicitly enable interrupts/preemption at syscall entry, or specify and gate
  the current non-preemptible contract?
- `issues/kernel/the-blocked-task-dump-panics-when-a-cpu-is-inside-inbox-submit.md` —
  **NEEDS-DECISION**: permit depth two with a proved lock budget, or defer the
  dump request until a pass that holds nothing?
- `issues/kernel/thread-exits-completion-post-is-the-second-one.md` —
  **NEEDS-DECISION**: is the second post required for promptness, or should
  release publication be the sole completion edge?
- `issues/kernel/volatile-composites-on-mmio-dma-structs.md` —
  **NEEDS-DECISION**: which per-device field ordering/atomicity contracts are
  required; the instruments analysis says a generic volatile-composite tool is
  not worth building.
- `issues/panic-path/panic-on-wedged-virtio-console-spins.md` —
  **NEEDS-DECISION**: what bounded write budget and fallback/drop semantics are
  acceptable on the panic path?

## DEPENDS

- `issues/audio/disk-wait-pins-a-cpu.md` — **DEPENDS** on the asynchronous
  block-operation/callback interface also named by
  `issues/hardware/xhci-waits-are-spins.md`; the synchronous storage API is the
  ownership boundary, not one call site.
- `issues/audio/gate-a-has-no-runner-baseline.md` — **DEPENDS** on
  `issues/audio/t14-wake-lateness-is-bimodal-per-boot.md`; choosing a host key
  and recording a sample cannot precede understanding which per-boot
  distribution it represents.
- `issues/audio/gate-a-suspend-structure-verdict-unread.md` — **DEPENDS** on
  `issues/audio/t14-wake-lateness-is-bimodal-per-boot.md`; a runner baseline is
  meaningless until the per-boot modes are understood.
- `issues/build/the-t14-runner-is-trusted-not-isolated.md` — **DEPENDS** on a
  listed runner-provisioning/restore mechanism; no local test-harness edit can
  isolate the host it is executing on.
- `issues/hardware/anonymous-mmap-is-not-demand-paged.md` — **DEPENDS** on the
  VM residency/ownership work represented by
  `issues/kernel/one-mapping-is-written-in-two-ledgers.md` before demand paging
  can have one authoritative ledger.
- `issues/hardware/eleven-names-red-on-ci.md` — **DEPENDS** on the current
  constituent redlist/issue owners; it is a historical roll-up and has no
  single fix after the closed names are removed.
- `issues/hardware/hotplug-blocks-a-scheduler-pass.md` — **DEPENDS** on the
  scheduler deferred-callback/deadline work that can own enumeration outside a
  pass.
- `issues/hardware/t14-lost-every-integrated-input.md` — **DEPENDS** on
  `t14-keyboard-will-not-report-its-scancode-set.md` and
  `the-t14-mouse-may-be-another-defect.md` being separated on hardware.
- `issues/hardware/tearing-is-what-gop-cannot-give-back.md` — **DEPENDS** on a
  display-driver scanout/page-flip decision plus the listed whole-window client
  damage conversions.
- `issues/hardware/xhci-waits-are-spins.md` — **DEPENDS** on an asynchronous
  block-operation/callback interface; the remaining waits are the storage API,
  not a local polling loop.
- `issues/isolation/dtv-capacity-is-a-workload-bound.md` — **DEPENDS** on the
  Ring-3 loader/TLS ownership work that can resize or refuse DTV growth.
- `issues/isolation/sshd-authorized-keys-unprotected.md` — **DEPENDS** on a
  filesystem permission or capability ownership model; no local SSH check can
  create protection the VFS cannot express.
- `issues/isolation/untrusted-sites-not-yet-adopted.md` — **DEPENDS** on the
  individually listed boundary conversions and their per-device failure
  actuators; it is a roll-up, not one patch.
- `issues/kernel/deferred-release-outlives-its-syscall.md` — **DEPENDS** on the
  listed object-release/sleep-lock track: hook ownership must be chosen without
  allowing a hook to park under a lock.
- `issues/kernel/desktop-window-child-freeze.md` — **DEPENDS** on
  `issues/build/a-harness-injected-program-can-be-endowed-with-nothing.md` so the
  reproduction reaches the shipping authority shape.
- `issues/kernel/lseek-past-eof-is-silently-clamped.md` — **DEPENDS** on
  `issues/kernel/past-eof-holes-wedge-a-shared-boot.md`; the POSIX seek fix is
  intentionally reverted until that induced wedge is understood.
- `issues/panic-path/no-console-between-boot-and-terminal.md` — **DEPENDS** on
  choosing and building an input-independent persistent/on-screen output
  channel after userland claims the framebuffer.
- `issues/panic-path/panic-holding-process-table-hangs.md` — **DEPENDS** on the
  panic-path ownership redesign that supplies process context without acquiring
  a possibly-held process-table lock.
