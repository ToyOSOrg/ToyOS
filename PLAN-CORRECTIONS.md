# Corrections to the executable defect plan

This is a record audit, not a replacement plan.  It compares all 73 entries in
`PLAN.md` (written at `72f0218f6cc05d3278920911b9ed09bf3db68565`) with each
authoritative issue file and the current tree at
`273a321cc44e203bb441976f8a59c4586319c4ac`.  Both object names were verified as
commits.  The current Rust source was read at the superproject's verified gitlink
`aab2f4de2b86717c457f4c72b127765fe010ff05`.

The result is **25 ACCURATE, 38 DRIFTED, and 10 DEAD**.  `ACCURATE` means the
plan still states the issue's present defect and a compatible exit.  `DRIFTED`
means the issue is live but the plan changes its claim, assumes an unproved
mechanism, omits a live half, or prescribes an instrument/fix inconsistent with
the authoritative record.  `DEAD` means the issue path is gone on the audited
tree; the deletion commit is recorded below.  No issue file is changed here.

## Bundle 1's three material mismatches

These rows are not editorial variations:

- `issues/build/the-gate-is-a-full-suite.md`: the sheet says a named gate aliases the whole suite (`PLAN.md:37`), but the issue's current heading says the defect is that the staged image vanished from the lane (`issues/build/the-gate-is-a-full-suite.md:7`).
- `issues/build/contention-has-no-owning-instrument.md`: the sheet narrows the gap to missing holder/test identity (`PLAN.md:38`), but the issue says no instrument owns the contention class (`issues/build/contention-has-no-owning-instrument.md:7`).  Current build slots already name their holder (`src/buildlock.rs:281-330`); the missing durable, joined observation is broader.
- `issues/hardware/pre-flash-gate-missed-the-milestone.md`: the sheet prescribes a GPT/FAT prepublication gate (`PLAN.md:39`), but the issue records a missed input milestone and missing checklist teeth across SMP, blocking input paths, and the flash decision (`issues/hardware/pre-flash-gate-missed-the-milestone.md:7`).

## Complete 73-entry status ledger

### Bundle 1 — Host build hygiene

- issues/build/worktree-add-help-panics-on-statvfs.md — DEAD
- issues/build/memmap2-fork-is-unreachable-code.md — DRIFTED
- issues/build/prose-ledger-carries-slack-a-sweep-never-booked.md — DRIFTED
- issues/build/the-gate-is-a-full-suite.md — DRIFTED
- issues/build/contention-has-no-owning-instrument.md — DRIFTED
- issues/hardware/pre-flash-gate-missed-the-milestone.md — DRIFTED

### Bundle 2 — Harness evidence and isolation

- issues/audio/gate-a-suspend-structure-verdict-unread.md — DRIFTED
- issues/build/console-locale-detect-loses-every-typed-line.md — DEAD
- issues/build/free-memory-verdicts-share-a-boot.md — DRIFTED
- issues/audio/gate-a-has-no-runner-baseline.md — DRIFTED
- issues/audio/thorough-tier-reds-on-unmodified-main.md — DRIFTED
- issues/boot-media/kernel-log-file-reds-beside-other-guests-and-is-green-alone.md — DRIFTED
- issues/boot-media/usb-short-read-reds-beside-other-guests-and-is-green-alone.md — DRIFTED
- issues/build/parallel-tests-red-under-other-suites.md — DRIFTED
- issues/build/the-shard-split-prices-a-boot-and-not-the-image-behind-it.md — ACCURATE
- issues/diagnostics/blocked-dump-cannot-fire-on-a-total-freeze.md — ACCURATE
- issues/kernel/syscall-window-nmi-shortfalls-on-a-contended-host.md — DRIFTED

### Bundle 3 — Audio correctness

- issues/audio/hda-tone-phase-check.md — DRIFTED
- issues/audio/hda-tone-red-beyond-its-exemption.md — DRIFTED
- issues/audio/disk-wait-pins-a-cpu.md — ACCURATE
- issues/audio/desktop-session-put-26ms-of-silence.md — ACCURATE
- issues/audio/idle-suspend-reds-on-a-loaded-host-and-on-main.md — DRIFTED

### Bundle 4 — cpal format negotiation

- issues/audio/cpal-backend-hardcodes-the-format.md — ACCURATE

### Bundle 5 — Storage semantics and media validation

- issues/boot-media/the-gpt-floor-belongs-to-the-caller-not-the-parser.md — DEAD
- issues/filesystem/delete-prefix-keeps-the-unbounded-walk-list-gave-up.md — DEAD
- issues/filesystem/readdir-bound-is-per-mount.md — DRIFTED
- issues/filesystem/fat-overwrite-rename-frees-the-destination-first.md — DEAD
- issues/filesystem/fat-unlink-reallocate-leaks-a-cluster-under-load.md — DRIFTED
- issues/filesystem/a-shrink-unflushed-regrows-the-old-tail.md — DRIFTED
- issues/filesystem/a-spawn-reads-round-an-open-file-s-dirty-pages.md — ACCURATE
- issues/build/page-cache-owns-one-device.md — ACCURATE
- issues/filesystem/usb-esp-gate-holes.md — DRIFTED
- issues/isolation/probe-mounts-on-a-checksum.md — DRIFTED
- issues/kernel/past-eof-holes-wedge-a-shared-boot.md — DRIFTED

### Bundle 6 — PCI/xHCI device progress

- issues/isolation/bus-mastering-rides-memory-decode.md — ACCURATE
- issues/hardware/hotplug-blocks-a-scheduler-pass.md — DRIFTED
- issues/kernel/scheduler-pass-blocks-in-xhci.md — ACCURATE
- issues/hardware/a-bar-sharing-the-scanout-page.md — ACCURATE
- issues/hardware/xhci-waits-are-spins.md — ACCURATE
- issues/kernel/volatile-composites-on-mmio-dma-structs.md — ACCURATE

### Bundle 7 — Memory and user-boundary safety

- issues/hardware/anonymous-mmap-is-not-demand-paged.md — DRIFTED
- issues/isolation/untrusted-sites-not-yet-adopted.md — DRIFTED
- issues/kernel/kernel-hashmaps-take-userland-chosen-keys.md — ACCURATE
- issues/build/ring-rs-shared-slice-over-a-userland-writable-page.md — DEAD
- issues/design-debt/kernelslice-outlives-its-allocation.md — DRIFTED
- issues/isolation/kernelslice-over-user-memory.md — DRIFTED
- issues/kernel/one-mapping-is-written-in-two-ledgers.md — DRIFTED

### Bundle 8 — Diagnostics and accounting

- issues/design-debt/rights-log-names-a-holder-that-does-not-hold-it.md — DEAD
- issues/diagnostics/blocked-time-is-invisible-while-the-park-lasts.md — ACCURATE
- issues/kernel/granularity-bound-crossed-at-four-widths.md — DRIFTED
- issues/hardware/kernel-log-unreadable-once-userland-owns-the-screen.md — DRIFTED

### Bundle 9 — Process, loader and pipe semantics

- issues/kernel/lseek-past-eof-is-silently-clamped.md — DRIFTED
- issues/kernel/process-open-panics-on-a-reopened-process.md — DRIFTED
- issues/kernel/spawn-thread-disagrees-about-a-reaped-parent.md — DRIFTED
- issues/kernel/the-global-pipe-lock-spans-a-user-copy.md — DRIFTED
- issues/kernel/dlopen-dedup-only-holds-after-the-race-settles.md — DRIFTED

### Bundle 10 — Scheduler retirement

- issues/kernel/retire-tripwire-is-not-queue-shaped.md — DRIFTED
- issues/kernel/steal-probe-node-dies-with-its-victim.md — ACCURATE

### Bundle 11 — Panic-path safety

- issues/kernel/fatal-text-safety-comment-claims-a-write-that-recurs.md — DEAD
- issues/panic-path/panic-console-capture-untested.md — ACCURATE
- issues/panic-path/panic-on-wedged-virtio-console-spins.md — ACCURATE
- issues/kernel/no-alloc-error-handler.md — ACCURATE
- issues/panic-path/crash-report-preemption-untested.md — ACCURATE

### Bundle 12 — Service acceptance oracles

- issues/isolation/sshd-accept-path-unexercised.md — ACCURATE
- issues/isolation/toybox-is-one-row-for-nineteen-applets.md — DRIFTED

### Bundle 13 — Rust standard-library fork

- issues/build/std-fork-not-rustfmt-clean.md — ACCURATE
- issues/build/std-systemtime-now-returns-the-epoch.md — ACCURATE
- issues/design-debt/std-says-this-machine-has-one-cpu.md — ACCURATE
- issues/filesystem/std-stat-conflates-io-with-notfound.md — ACCURATE
- issues/build/std-leaks-a-thread-stack-per-spawn.md — ACCURATE

### Bundle 14 — Broken-pipe ABI

- issues/isolation/a-broken-pipe-answers-not-found.md — DRIFTED

### Already-closed section

- issues/diagnostics/a-console-tag-is-composed-by-replacing-a-bracket.md — DEAD
- issues/diagnostics/no-guest-can-change-the-display-mode.md — DEAD

## DRIFTED corrections — 38

1. **memmap2 fork:** the plan says the dependency graph cannot select the fork (`PLAN.md:35`); the issue says the fork is selected and load-bearing for its version pin, but its added APIs remain unreachable behind rustc-only cfgs.
2. **prose ledger:** the plan invents a `prosegate` derivation/orphan-allowance fix (`PLAN.md:36`); the issue's exit is the mechanical prose sweep followed by updating 25 ledger rows and `DATED_TOTAL` (`issues/build/prose-ledger-carries-slack-a-sweep-never-booked.md:57`).
3. **full-suite gate:** the plan's alias claim (`PLAN.md:37`) does not match the authoritative staged-image-removal claim (`issues/build/the-gate-is-a-full-suite.md:7`).
4. **contention instrument:** the plan reduces the gap to holder/test identity (`PLAN.md:38`); the issue says the contention class has no owning instrument, while build slots already name holders (`src/buildlock.rs:281-330`).
5. **pre-flash milestone:** the plan substitutes GPT/FAT artifact validation (`PLAN.md:39`) for the issue's missed input milestone and checklist enforcement (`issues/hardware/pre-flash-gate-missed-the-milestone.md:7`).
6. **Gate-A suspend structure:** the plan says workflow exit propagation is the defect (`PLAN.md:47`), but the workflow now uses `set -o pipefail` and propagates the harness exit (`.github/workflows/gate-a.yml:155-186`); the live issue asks for the remaining suspend-structure sighting and rate.
7. **free-memory shared boot:** the plan prescribes fresh boots/reset (`PLAN.md:49`), but the issue records that quiescent repeated sampling is already implemented (`issues/build/free-memory-verdicts-share-a-boot.md:204-218`; `tests/toyos-rust-tests/src/bin/handle_lifetime.rs:238`).
8. **runner baseline:** the plan says no runner baseline exists (`PLAN.md:50`), while the issue records a same-runner A/B; the remaining question is host dimension and the T14 bimodal mode (`issues/audio/gate-a-has-no-runner-baseline.md:64`).
9. **thorough tier:** the plan asks only for generic per-run telemetry (`PLAN.md:51`); the issue requires historical-base/current-tree arms in the same developer-host session and explanation of the changed population.
10. **kernel-log coexistence:** the plan asserts an unidentified phase (`PLAN.md:52`), while the issue's live exit is a measured coexistence rate (`issues/boot-media/kernel-log-file-reds-beside-other-guests-and-is-green-alone.md:31`).
11. **USB short read:** the plan asserts retained transfer tracing is prerequisite (`PLAN.md:53`); the issue first asks for reproduction/rate, and the existing test already has a staged verdict and byte sweep (`tests/common/usb.rs:356-426`).
12. **parallel tests:** the plan treats the umbrella as one future attribution split (`PLAN.md:54`), but the record contains several corrected or resolved mechanisms as well as live ones; it must be re-triaged per claim, not dispatched as one defect.
13. **syscall-window NMI:** the plan says host and guest starvation are not separately measured (`PLAN.md:57`), but the current harness declares the parked-victim starvation signature when arrivals exceed traversals (`tests/common/faults.rs:538-555`).  Host intervals would corroborate it, not create the first discriminator.
14. **HDA phase:** the plan asks to add a phase analyzer (`PLAN.md:65`), but one already exists and is called by the audio harness (`tests/common/audio.rs:265`; `tests/toyos.rs:2334`); the live issue is the measured boundary failures (`issues/audio/hda-tone-phase-check.md:84`).
15. **HDA exemption:** the plan says the exemption is broader than its mechanism (`PLAN.md:66`), but the predicate is already restricted to the phase case (`tests/toyos.rs:1166-1173`); the issue says a real red exists beyond that narrow exemption (`issues/audio/hda-tone-red-beyond-its-exemption.md:37`).
16. **idle suspend:** the plan says guest attribution exists but host evidence does not (`PLAN.md:69`); the issue records that the guest idle-wake instrument itself has landed and now asks a rate question (`issues/audio/idle-suspend-reds-on-a-loaded-host-and-on-main.md:90`).
17. **readdir:** the plan proposes a per-open counter/cursor (`PLAN.md:87`), but the issue says the implementation walks the whole mount with no directory index; current code still applies `MAX_LIST_ENTRIES` to mount-wide collection (`kernel/src/vfs.rs:145`, `kernel/src/vfs.rs:277`, `kernel/src/vfs.rs:320`).
18. **FAT unlink/reallocate:** the plan asserts a replayable transaction (`PLAN.md:89`), but the issue says the mechanism is not known and first requires discriminating which side leaks (`issues/filesystem/fat-unlink-reallocate-leaks-a-cluster-under-load.md:66`).
19. **shrink/regrow:** the plan names `FileBacking::truncate_to_blocks` (`PLAN.md:90`), which is not the issue's named mechanism; the issue describes the low-water mark and shared extents (`issues/filesystem/a-shrink-unflushed-regrows-the-old-tail.md:25`).
20. **USB ESP holes:** the plan reduces the record to malformed committed fixtures (`PLAN.md:93`); its live holes include the read comparator, a vacuous negative parser arm, and an unstaged designation stamp.
21. **probe checksum:** the plan describes FAT checksum identity (`PLAN.md:94`), but the issue is bcachefs checksum/designation validation, including the backup superblock.
22. **past-EOF wedge:** the plan asserts monolithic hole fill and an incremental fix (`PLAN.md:95`); the issue records the mechanism as unknown, so that prescription is not earned.
23. **hotplug:** the plan proposes a completion/cadence state machine (`PLAN.md:104`), but current xHCI already has `device::begin`, `port.step`, and `PORT_WORK_AT` (`kernel/src/drivers/xhci/mod.rs:333-351`, `kernel/src/drivers/xhci/mod.rs:1059`, `kernel/src/drivers/xhci/mod.rs:1137`); the residual claim includes deferred callback/cadence behavior.
24. **anonymous mmap:** the plan covers eager anonymous allocation only (`PLAN.md:116`); the issue also carries the linker `.bss` half, so the proposed bundle cannot close the record.
25. **untrusted sites:** the plan recasts the issue as user-memory windows and copies (`PLAN.md:117`), but the record is specifically about adoption of `Untrusted<T>` numeric wrappers; some named sites already use `Untrusted::new` while other raw casts remain.
26. **KernelSlice lifetime:** the plan requires an allocation-generation model first (`PLAN.md:120`), but the issue's strongest oracle is a compiler-enforced `KernelSlice<'alloc>` lifetime; the present type is `Copy` and has no lifetime (`kernel/src/mm/region.rs:1-29`).
27. **KernelSlice over user memory:** the plan describes generic mutation of user mappings (`PLAN.md:121`), but the issue's concrete risk is the loader's borrow/write overlap across untrusted ELF ranges.
28. **two VM ledgers:** the plan makes a failure-edge model the exit (`PLAN.md:122`); the issue calls for consolidating `AddressSpace.regions` and `ProcessData.mmap_regions`, which remain separate (`kernel/src/mm/paging.rs:425-431`; `kernel/src/process.rs:480-490`).
29. **granularity bound:** the plan turns the defect into integer-width checked arithmetic (`PLAN.md:132`); the issue is a scheduler fairness bound crossing its promised granularity.
30. **kernel log on glass:** the plan points at panic snapshot reclamation (`PLAN.md:133`), but panic snapshot work has landed; the live issue is readability of live kernel logs after compositor ownership on serial-less, dead-input hardware (`issues/hardware/kernel-log-unreadable-once-userland-owns-the-screen.md:150-159`).
31. **lseek:** the plan treats sparse write as directly dispatchable (`PLAN.md:141`), but the issue says the earlier fix was reverted because its workload wedged in four of five runs (`issues/kernel/lseek-past-eof-is-silently-clamped.md:32`); the dependency and cause must be resolved first.
32. **process open:** the plan proposes idempotent insertion/existing-object return (`PLAN.md:142`), while the issue identifies object retirement policy for `ProcessObject` as the required ownership decision.
33. **spawn thread:** the plan claims a reap/check/commit race and prescribes a `toyos-proclife` transition (`PLAN.md:143`), but the issue asks for a reachability proof; the current model cannot exhibit a live spawn syscall after the parent entry is gone (`issues/kernel/spawn-thread-disagrees-about-a-reaped-parent.md:42`).
34. **global pipe lock:** the plan proposes a bounded-buffer reservation transaction (`PLAN.md:144`), but the issue's stated fix is a per-pipe lock with allocation moved outside the lock.
35. **dlopen dedup:** the plan requires a pure model before implementation (`PLAN.md:145`), but the issue accepts one held lock or a second check under the publication lock (`issues/kernel/dlopen-dedup-only-holds-after-the-race-settles.md:25`); lookup and publish are visibly separated today (`kernel/src/arch/syscall/vm.rs:176-192`, `kernel/src/arch/syscall/vm.rs:306-313`).
36. **retire tripwire:** the plan proposes per-record deadlines (`PLAN.md:153`), but the issue records that the queue-shaped-deadline approach was considered and withdrawn; its current exit removes the scalar deadline through reservation/report semantics.  The scalar `GIVE_UP` remains in the tree (`kernel/src/scheduler.rs:538-558`).
37. **toybox authority:** the plan asks for nineteen behavioral output corpora (`PLAN.md:175`), but the issue is capability authority union: init resolves applet symlinks to one binary row (`userland/init/src/main.rs:422-440`) whose manifest row owns the union (`system.toml:90-101`).
38. **broken pipe:** the plan says the entire ABI/kernel/SDK/libc chain remains (`PLAN.md:195`), while the issue records those halves closed; only Rust std pipe/stdio conversion is live (`issues/isolation/a-broken-pipe-answers-not-found.md:9-28`; `rust/library/std/src/sys/pipe/toyos.rs:6-13`; `rust/library/std/src/sys/stdio/toyos.rs:11-13`).

## DEAD corrections — 10

Each object below was verified as a commit before citation.

- `issues/build/worktree-add-help-panics-on-statvfs.md` — deleted by `2ec518fba1083ded92dc897373c5902a0725b3ab`.
- `issues/build/console-locale-detect-loses-every-typed-line.md` — deleted by `1b140f50f16fcdfd70a27eabfc7f946f368c0f67`.
- `issues/boot-media/the-gpt-floor-belongs-to-the-caller-not-the-parser.md` — deleted by `392b65534ca64907899b3b7f6e6077f51c5a3cce`.
- `issues/filesystem/delete-prefix-keeps-the-unbounded-walk-list-gave-up.md` — deleted by `4cae5cf1770799d3d169ca0f65b844c21cc9ca51`.
- `issues/filesystem/fat-overwrite-rename-frees-the-destination-first.md` — deleted by `b1c5f6b7fb414f1fbe5b04669bc5bf08aeb2d333`.
- `issues/build/ring-rs-shared-slice-over-a-userland-writable-page.md` — deleted by `df2eac53ec37cac22b553159bc362de7e4243ca4`.
- `issues/design-debt/rights-log-names-a-holder-that-does-not-hold-it.md` — deleted by `3eed73e364e4714e8b0d05b1d1dd6b2a726b7e8d`.
- `issues/kernel/fatal-text-safety-comment-claims-a-write-that-recurs.md` — deleted by `07160bf34cc3484694cbda6c19e741707f89551a`.
- `issues/diagnostics/a-console-tag-is-composed-by-replacing-a-bracket.md` — deleted by `86efe080be0642ed1ade616ae6ddbdbe92574530`.
- `issues/diagnostics/no-guest-can-change-the-display-mode.md` — deleted by `0f6937fe89d62185f3accc9cfce19d72522a1a59`.

## Accounting

- Plan entries audited: **73**.
- ACCURATE: **25**.
- DRIFTED: **38**.
- DEAD: **10**.
