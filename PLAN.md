# Executable plan for the actionable defect set

Of the original 73 category-A records, **47 are dispatchable today**, **24 are blocked because the instrument or independent oracle needed to choose or prove a fix does not exist**, and **2 have already closed on `main`**. “Blocked” is deliberate: a plausible patch without an oracle is not a high-risk fix under this repository’s rules.

This is a working dispatch document derived from the live tree at `72f0218f6cc05d3278920911b9ed09bf3db68565` and from the earlier triage notes. It does not assign work or supersede an issue file.

## Bundle index

| Order | Bundle | Aggregate size | Defects | Ready / blocked | Lane |
|---:|---|:---:|---:|---:|---|
| 1 | Host build hygiene | M | 6 | 6 / 0 | ordinary |
| 2 | Harness evidence and isolation | L | 11 | 3 / 8 | ordinary; instrument first |
| 3 | Audio correctness | L | 5 | 3 / 2 | quiet audio window |
| 4 | cpal format negotiation | M | 1 | 1 / 0 | external-fork quiet window |
| 5 | Storage semantics and media validation | L | 11 | 11 / 0 | ordinary |
| 6 | PCI/xHCI device progress | L | 6 | 3 / 3 | ordinary, device-heavy |
| 7 | Memory and user-boundary safety | L | 7 | 3 / 4 | ordinary; model first |
| 8 | Diagnostics and accounting | M | 4 | 4 / 0 | ordinary |
| 9 | Process, loader and pipe semantics | L | 5 | 4 / 1 | ordinary |
| 10 | Scheduler retirement | M | 2 | 1 / 1 | ordinary; interleaving crate |
| 11 | Panic-path safety | L | 5 | 3 / 2 | ordinary; Loom first |
| 12 | Service acceptance oracles | L | 2 | 0 / 2 | instrument first |
| 13 | Rust standard-library fork | L | 5 | 4 / 1 | **machine-exclusive `rust/` lane** |
| 14 | Broken-pipe ABI | S | 1 | 1 / 0 | **single-claimant ABI lane** |

Bundles are cheapest-first internally. Every ordinary bundle must not touch `toyos-abi/src`, `toyos/src`, `userland/libc/src`, or `rust/`. Bundle 13 must touch only the std fork and its direct tests and needs the machine to itself. Bundle 14 is the only ABI-bearing bundle and must land alone.

## 1. Host build hygiene

Must not touch guest/kernel behavior, ABI sources, or `rust/`.

| Issue | What is wrong; concrete fix | Negative control; independent oracle | Size; collision set |
|---|---|---|---|
| `issues/build/worktree-add-help-panics-on-statvfs.md` | The help path performs a fallible volume query before it knows a worktree is requested. In `src/worktree.rs`, move `statvfs` below argument dispatch or make the help arm return before capacity discovery. | Inject the recorded `statvfs` error: the base panics naming `statvfs`, while `--worktree add --help` must exit zero. Oracle: help must create neither a directory nor a Git ref. | S; `src/worktree.rs`, its host tests |
| `issues/build/memmap2-fork-is-unreachable-code.md` | The manifest/lock graph cannot select the repository’s memmap2 fork. Remove the dead fork declaration or point the consuming dependency at it explicitly. | Add a fork-only package marker: the base resolver omits it; the fixed `cargo metadata` graph contains exactly the selected revision. Oracle: Cargo’s resolved graph. | S; root manifests, lockfiles, fork witness tests |
| `issues/build/prose-ledger-carries-slack-a-sweep-never-booked.md` | `src/prose-ledger` reserves growth that no executable sweep owns. Make `src/prosegate.rs` derive every row from the measured tree and reject orphan allowance, then book the actual sweep or remove the slack. | Restore one orphan row: the gate must name that path as unearned. Oracle: a fresh independent line count of the same source set. | S; `src/prosegate.rs`, `src/prose-ledger` |
| `issues/build/the-gate-is-a-full-suite.md` | A named gate aliases the whole suite, so its cost and ownership are unknowable. In `src/testargs.rs`/`tests/toyos.rs`, define the gate as an explicit closed name set and reject drift. | Add an unrelated test to the suite: the base gate silently grows; the fixed gate’s membership check names the unowned test. Oracle: registered-name set versus executed-name transcript. | M; `src/testargs.rs`, `tests/toyos.rs`, workflow gate invocation |
| `issues/build/contention-has-no-owning-instrument.md` | Contended host slots name holders but do not persist which test/job caused the overlap. Extend `src/buildlock.rs` acquisition records and harness summaries with holder/test identity and interval. | Run two synthetic holders: remove identity publication and the test must fail naming an unattributed overlap. Oracle: host PID start/end timestamps. | M; `src/buildlock.rs`, harness host-condition reporting |
| `issues/hardware/pre-flash-gate-missed-the-milestone.md` | Flashable-image validation is advisory instead of an enforced artifact predecessor. Make the build graph invoke GPT/FAT/image checks before it publishes a flash target. | Corrupt one GPT and one FAT fixture: the base still publishes; the fixed build must name the rejecting checker. Oracle: `toyos-gpt` plus `toyos-fat32-check`, neither image writer. | M; `src/image.rs`, build command graph, checker fixtures |

## 2. Harness evidence and isolation

Land the three ready rows first. The other eight wait for the named instrument; do not “fix” them by widening a ceiling or changing `Sched`.

| Issue | What is wrong; concrete fix | Negative control; independent oracle | Size; collision set |
|---|---|---|---|
| `issues/audio/gate-a-suspend-structure-verdict-unread.md` | The nightly produces a suspend-structure verdict but its workflow exit path can ignore it. Make `.github/workflows/gate-a.yml` parse and propagate the harness verdict as the job result. | Feed the recorded missing-suspend transcript: the base workflow arm passes/reds for shell mechanics; the fixed parser must name the absent `soundd: suspended`/device-stop evidence. Oracle: raw guest transcript. | S; `.github/workflows/gate-a.yml`, verdict parser tests |
| `issues/build/console-locale-detect-loses-every-typed-line.md` | `shell_type_once` bounds individual PS/2 batches but sends all batches before a guest acknowledgement. Pace `/bin/console` bursts on the decoded row, and give windowed shells a guest-side byte/drain acknowledgement. | Restore back-to-back batches: the 44-byte line must fail naming an unacknowledged burst; a 14-byte control stays green. Oracle: i8042 drain counts plus the decoded input row. | M; `tests/toyos.rs::{ps2_bursts,shell_type_once}`, `tests/common/qemu.rs` |
| `issues/build/free-memory-verdicts-share-a-boot.md` | Several memory verdicts inherit allocator state from earlier tests in one guest. Split them into fresh-boot groups or add a proven reset before each verdict. | Reverse the old order and seed allocations: the base changes its answer; fixed fresh boots remain invariant. Oracle: per-boot PMM stats from `kernel/src/mm/pmm.rs::stats`. | M; `tests/toyos.rs` registration/grouping, memory probes |
| `issues/audio/gate-a-has-no-runner-baseline.md` | **BLOCKED:** the runner is judged against a dev-host distribution. Build a runner-owned sampling command and provenance row before changing any audio threshold. | No valid mutation exists until the runner baseline exists. Oracle needed: same-runner raw WAV and counters over the committed sample size. | L; future audio-baseline instrument, `tests/audio-baseline.toml`, gate workflow |
| `issues/audio/thorough-tier-reds-on-unmodified-main.md` | **BLOCKED:** the recorded zero-dropout population and current dev-host population disagree, but no instrument attributes the mode. Add per-run host scheduling/device telemetry before choosing a code fix. | A ceiling/baseline edit is explicitly not a control. Oracle needed: recorded WAV harm plus same-session historical-base/current-tree arms. | L; future Gate-A instrument, audio harness only |
| `issues/boot-media/kernel-log-file-reds-beside-other-guests-and-is-green-alone.md` | **BLOCKED:** one timing sighting does not identify whether logd start, mount, or harness observation lost progress. Build a same-session rate runner with phase timestamps. | No code mutation is justified yet. Oracle needed: on-device log contents correlated with guest phase markers. | M; future rate runner around `kernel_log_file` |
| `issues/boot-media/usb-short-read-reds-beside-other-guests-and-is-green-alone.md` | **BLOCKED:** the short-read recovery red has no rate or retained failing device trace. Add repeat-mode artifact retention and per-transfer progress first. | No driver mutation is justified yet. Oracle needed: retained transfer ring plus returned-byte sweep. | M; future USB rate/trace harness |
| `issues/build/parallel-tests-red-under-other-suites.md` | **BLOCKED:** the umbrella record mixes unrelated mechanisms because no host-wide overlap instrument exists. First land Bundle 1’s contention attribution, then split this record by attributed cause. | Until attribution exists, changing `Sched` cannot distinguish cause. Oracle needed: host interval ledger joined to each guest’s progress markers. | L; future contention ledger, harness scheduler metadata |
| `issues/build/the-shard-split-prices-a-boot-and-not-the-image-behind-it.md` | **BLOCKED:** the duration ledger cannot separate image construction from boot/test execution. Add artifact-graph timestamps and cache-hit identity before changing shard weights. | Invalidate only the image: the future instrument must charge that time outside every test. Oracle needed: filesystem artifact mtimes plus build-span transcript. | M; future build-span instrument, `src/durations.rs` |
| `issues/diagnostics/blocked-dump-cannot-fire-on-a-total-freeze.md` | **BLOCKED:** every current trigger requires schedulable work. Build an external QMP/NMI actuator that freezes all schedulable CPUs yet preserves serial capture. | The future negative arm disables the NMI trigger and must time out naming “no dump”. Oracle needed: host QMP register state plus serial output. | L; future QMP actuator, `kernel/src/sched/dump.rs`, tests |
| `issues/kernel/syscall-window-nmi-shortfalls-on-a-contended-host.md` | **BLOCKED:** host descheduling and guest NMI delivery are not separately measured. Reuse Bundle 1’s interval ledger and add guest window markers before changing the bound. | No timeout widening is a control. Oracle needed: host vCPU run intervals correlated with guest NMI/window records. | M; future contention/NMI instrument, harness only |

## 3. Audio correctness

Must run in a quiet audio window and must not weaken Gate A or change audible output without the owner’s approval.

| Issue | What is wrong; concrete fix | Negative control; independent oracle | Size; collision set |
|---|---|---|---|
| `issues/audio/hda-tone-phase-check.md` | HDA validation checks frequency/amplitude but can miss a discontinuity between DMA periods. Extend the host-side PCM analyzer to compare phase at every period boundary. | Reset oscillator phase at one boundary: the control must name that boundary and phase jump. Oracle: analytic 440/660 Hz phase progression over captured PCM. | S; HDA audio tests/analyzer, PCM fixtures |
| `issues/audio/hda-tone-red-beyond-its-exemption.md` | A real out-of-envelope HDA tone can be hidden by an exemption broader than its named mechanism. Narrow the exemption predicate to the exact declared case and keep harm verdicts fatal. | Feed the recorded bad capture: the broad base exempts it; fixed code names the frequency/amplitude violation. Oracle: raw PCM analyzed independently of the exemption code. | S; audio expected-failure table, analyzer tests |
| `issues/audio/disk-wait-pins-a-cpu.md` | Disk completion is waited for under four preemption-disabling locks, so audio misses whole pipelines. Restructure VFS/FAT/device transactions so controller submission drops upper locks and completion resumes a staged transaction. | Reapply `usb-slow-device`: the old path produces the recorded 165–260 ms wakes/silence; the fixed arm must not pin a CPU. Oracle: Gate A WAV harm plus lock-depth probe. | L; `kernel/src/{vfs,fat32_adapter,log_file}.rs`, xHCI wait path |
| `issues/audio/desktop-session-put-26ms-of-silence.md` | **BLOCKED:** doom starvation and the stale `armed_on` resume statistic are not separated by any committed workload. Build a doom-plus-44-resumes Gate-A profile and record `target` beside `t_est` before editing `mix_thread`. | No single mutation is meaningful yet. Oracle needed: captured WAV plus producer period count and `target`/`t_est` trace. | L; future desktop-audio profile, `userland/soundd/src/mix.rs` |
| `issues/audio/idle-suspend-reds-on-a-loaded-host-and-on-main.md` | **BLOCKED:** the slow boot mode has guest attribution but no host-side evidence. Add per-vCPU `schedstat` and cpuidle residency capture when the mode appears. | No scheduler/audio mutation is justified. Oracle needed: host scheduling/residency capture aligned with existing IRQ/pickup split. | L; future host audio instrument |

## 4. cpal format negotiation

This is an external-fork quiet-window bundle. It must not alter the shared `.cargo/config.toml` while another agent is building, and must not absorb the larger client-liveness protocol track.

| Issue | What is wrong; concrete fix | Negative control; independent oracle | Size; collision set |
|---|---|---|---|
| `issues/audio/cpal-backend-hardcodes-the-format.md` | The ToyOS cpal backend asserts 44.1 kHz/stereo/i16, making soundd’s negotiated conversion paths unreachable. Change the fork backend’s supported-config enumeration and stream-open request to carry the selected format, refusing only formats soundd rejects. | Offer a supported non-default rate/channel shape: the base aborts at the assertion; the fix opens and plays it. Oracle: soundd’s negotiated format and captured PCM metadata/samples. | M; external cpal fork, temporary path override, soundd format tests |

## 5. Storage semantics and media validation

Must not touch std/libc error mapping (Bundle 13) or the ABI. FAT changes require both injected-error controls and `toyos-fat32-check`.

| Issue | What is wrong; concrete fix | Negative control; independent oracle | Size; collision set |
|---|---|---|---|
| `issues/boot-media/the-gpt-floor-belongs-to-the-caller-not-the-parser.md` | `toyos-gpt::parse_header` grants every caller slack caused by one coarse kernel adapter. Extend `Sectors`/parser input with actual byte granularity and compute only that caller’s remainder. | Restore fixed seven-LBA slack: a byte-exact malformed backup array parses; fixed code rejects it naming the backup bound. Oracle: UEFI GPT layout rules already quoted in-tree and an exact file-backed sector view. | S; `toyos-gpt/src/lib.rs`, kernel GPT adapter, parser tests |
| `issues/filesystem/delete-prefix-keeps-the-unbounded-walk-list-gave-up.md` | `bcachefs::delete_prefix` retains an unbounded walk list. Replace it with cursor-based delete/reseek or enforce a proved namespace bound in the iterator type. | Build a tree beyond the old retained capacity: base exhausts/refuses; fixed code deletes exactly the prefix. Oracle: independent post-operation namespace enumeration. | S; `bcachefs/src/fs.rs::delete_prefix`, integration tests |
| `issues/filesystem/readdir-bound-is-per-mount.md` | The adapter’s directory bound is shared per mount, so one reader spends another’s budget. Move the counter/cursor into each open directory object. | Interleave two large readers: base truncates the second; fixed readers each return their complete set. Oracle: direct filesystem namespace enumeration. | S; filesystem adapter directory-handle state, readdir tests |
| `issues/filesystem/fat-overwrite-rename-frees-the-destination-first.md` | `toyos_fat32::Fat32::rename` frees the destination chain before the replacement is durable. Write/flush the new directory entry first, then reclaim the old chain as the final recoverable step. | Existing injected device error between replacement and reclaim must preserve either old or new destination, never neither. Oracle: `toyos-fat32-check`. | M; `toyos-fat32/src/fs.rs::rename`, FAT/directory helpers, host-write tests |
| `issues/filesystem/fat-unlink-reallocate-leaks-a-cluster-under-load.md` | Unlink/reallocation can commit FAT and directory updates in an order that leaves allocated unreachable chains. Make allocation/free and directory publication one replayable transaction. | Reproduce the unlink/reallocate interleaving with the error cut at each write; base leaves lost clusters, fix does not. Oracle: retained image through `toyos-fat32-check`. | L; `toyos-fat32/src/{fs,fat,dir}.rs`, adapter stress test |
| `issues/filesystem/a-shrink-unflushed-regrows-the-old-tail.md` | Dirty page-cache extents beyond a new EOF survive truncate and reappear after extension. In `FileBacking::truncate_to_blocks` and adapter `truncate_to`, invalidate/split dirty extents before publishing size. | Shrink before flush, then extend: base restores the old tail; fixed bytes are zero/new data. Oracle: remount and read through the independent filesystem implementation/checker. | M; `kernel/src/file_backing.rs`, FAT/bcachefs adapters, truncate test |
| `issues/filesystem/a-spawn-reads-round-an-open-file-s-dirty-pages.md` | Spawn opens executable backing that can bypass an already-open file’s dirty cache pages. Make `Vfs::open_backing` settle or share the authoritative page-cache object before loader reads. | Modify an executable without closing/flushing, then spawn: base runs old bytes; fixed child observes new bytes. Oracle: direct cached read/hash of the executable. | M; `kernel/src/{vfs,file_backing,loader}.rs`, spawn dirty-file test |
| `issues/build/page-cache-owns-one-device.md` | `page_cache::init` consumes the only NVMe handle, preventing the same disk’s GPT volumes from mounting. Publish a shared block-device object with serialized requests instead of moving sole ownership into page cache. | Boot the internal-NVMe topology: base finds GPT but cannot mount `/boot`; fixed boot mounts and performs page-cache I/O. Oracle: GPT identity plus filesystem read from the same device. | L; block-device ownership, page cache, GPT/FAT adapters |
| `issues/filesystem/usb-esp-gate-holes.md` | The USB ESP gate omits named corruption/recovery cases, so writer regressions can pass. Add each issue’s malformed image as a committed host fixture and require both parser and checker verdicts. | Flip each omitted structure independently; deleting one assertion must let its fixture pass and make the gate red by name. Oracle: GPT parser plus `toyos-fat32-check`. | M; boot-media host tests and fixtures |
| `issues/isolation/probe-mounts-on-a-checksum.md` | FAT probing treats a matching checksum/identity as sufficient to mount malformed structure. Make `Fat32::probe` return only geometry and require full structural validation before publication. | Use a checksum-valid malformed image: base mounts; fixed path refuses the first structural violation. Oracle: `toyos-fat32-check`. | M; `toyos-fat32/src/fs.rs::{probe,mount}`, kernel probe adapter |
| `issues/kernel/past-eof-holes-wedge-a-shared-boot.md` | Sparse writes past EOF leave shared boot-device progress stuck. Make hole materialization incremental and release the device transaction between bounded writes/completions. | Restore monolithic hole fill and run the recorded concurrent boot user: it wedges; fixed arm progresses and creates the exact sparse bytes. Oracle: file image plus independent continued-device progress marker. | L; VFS/page cache/filesystem adapters, sparse-I/O test |

## 6. PCI/xHCI device progress

Do not mix this with filesystem transaction restructuring or ABI work.

| Issue | What is wrong; concrete fix | Negative control; independent oracle | Size; collision set |
|---|---|---|---|
| `issues/isolation/bus-mastering-rides-memory-decode.md` | `PciDevice::enable` couples MMIO decode and DMA authority. Split it into explicit memory-decode and bus-master capabilities; only a successfully bound DMA driver receives the latter. | Refuse a staged device after BAR mapping: base leaves COMMAND.bus-master set; fixed code clears it while MMIO remains readable. Oracle: PCI command-register readback and IOMMU fault visibility. | M; `kernel/src/drivers/pci.rs`, binding sites, `src/sourcegate.rs` |
| `issues/hardware/hotplug-blocks-a-scheduler-pass.md` | `PORT_WORK_AT` keeps a CPU awake and lets debounce/recovery occupy scheduler passes. Move port work to a completion/cadence state machine whose pass step is bounded and never waits. | Restore the in-pass delay: pass-latency actuator names the overrun; fixed hotplug still enumerates. Oracle: xHCI port state and independent scheduler-pass timing. | L; `kernel/src/drivers/xhci/{mod.rs,wait/boot.rs}`, scheduler timing tests |
| `issues/kernel/scheduler-pass-blocks-in-xhci.md` | `drain_irqs` can enter long xHCI recovery before the measured scheduler window. Make every recovery step budgeted/asynchronous and measure the full pass including its prologue. | Delay one controller step: base exceeds the pass bound outside measurement; fixed code reports deferred work and returns. Oracle: transfer completion plus external pass timestamp instrument. | L; xHCI service/recovery, `kernel/src/scheduler.rs`, tests |
| `issues/hardware/a-bar-sharing-the-scanout-page.md` | **BLOCKED:** no committed topology can present an overlapping BAR and scanout allocation. Build a QEMU/test allocator fixture that forces the overlap before choosing rejection versus partition. | Future overlap fixture must make the base publish aliases. Oracle needed: independent physical-range inventory. | M; future PCI/framebuffer topology fixture |
| `issues/hardware/xhci-waits-are-spins.md` | **BLOCKED:** `wait_transfer` spins under upper locks, so replacing it with park would panic and still pin the CPU. Bundle 3’s transaction split must land before a completion-based design can be tested. | Future control restores spin after locks are dropped and must show CPU busy time. Oracle needed: completion trace plus idle residency. | L; xHCI wait module after storage refactor |
| `issues/kernel/volatile-composites-on-mmio-dma-structs.md` | **BLOCKED:** composite volatile accesses have no bus-level access oracle. Build an access-recording MMIO/DMA façade that records width/order before rewriting fields as scalar operations. | Future mutation reinstates one composite load/store and the trace must name its width. Oracle needed: device-spec field access sequence or hardware trace. | L; future MMIO trace façade, affected drivers |

## 7. Memory and user-boundary safety

Formal/race instruments land before the blocked implementation rows. Do not touch ABI sources.

| Issue | What is wrong; concrete fix | Negative control; independent oracle | Size; collision set |
|---|---|---|---|
| `issues/hardware/anonymous-mmap-is-not-demand-paged.md` | `SYS_MMAP` allocates/maps an anonymous range eagerly. Install non-present region metadata and allocate/map a page in the fault path on first touch. | Map a large range and touch one page: base commits all pages; fixed code commits one. Oracle: PMM stats and page-table inspection. | L; `kernel/src/arch/syscall/dispatch.rs`, VM regions, page fault, tests |
| `issues/isolation/untrusted-sites-not-yet-adopted.md` | Named syscall/loader paths still make ad-hoc decisions over user memory. Route them through `toyos-userbound` windows and copy/validate before any kernel reference exists. | Mutate/fault each buffer after validation: old sites panic or observe mixed values; fixed sites refuse/copy. Oracle: `toyos-userbound` models plus abuse tests. | L; named kernel call sites, `toyos-userbound`, tests |
| `issues/kernel/kernel-hashmaps-take-userland-chosen-keys.md` | Kernel maps use predictable hashing for attacker-selected names/keys. Replace those maps with keyed hashing initialized at boot, or ordered maps where iteration is required. | Feed a generated collision corpus: base operation cost grows pathologically; fixed cost stays bounded. Oracle: operation-count instrument over the same key corpus. | M; named kernel maps, hash seed source, host corpus test |
| `issues/build/ring-rs-shared-slice-over-a-userland-writable-page.md` | **BLOCKED:** ring constructs an immutable Rust slice over concurrently writable user memory. Build a Loom/Miri façade around the real access primitive before choosing copy versus atomic-word protocol. | Future model mutates during read and must red on the base. Oracle needed: model of the real primitive, not a transliteration. | L; future model plus ring fork/access site |
| `issues/design-debt/kernelslice-outlives-its-allocation.md` | **BLOCKED:** `kernel/src/mm/region.rs::KernelSlice` carries no ownership generation. First build an allocation-lifetime model that can recycle the backing frame while a slice exists. | Future recycle interleaving must red on base. Oracle needed: real allocation-generation model. | L; future model, `kernel/src/mm/region.rs` |
| `issues/isolation/kernelslice-over-user-memory.md` | **BLOCKED:** user-backed memory can be exposed through a kernel slice while user writes continue. Reuse the same real primitive model, then choose copy or per-word atomics. | Future concurrent mutation red is mandatory. Oracle needed: model plus value-integrity corpus. | L; same model, user mapping/copy sites |
| `issues/kernel/one-mapping-is-written-in-two-ledgers.md` | **BLOCKED:** VM region and page-table ledgers can diverge on failure, but no model enumerates the cut points. Factor the transaction into a pure state machine and model every failure edge first. | Future failure between writes must red on base. Oracle needed: page-table/region equivalence model. | L; future VM model, region and mapping code |

## 8. Diagnostics and accounting

Must not change diagnostic record ABI; the thread-zero ABI question belongs in `DECISIONS.md`.

| Issue | What is wrong; concrete fix | Negative control; independent oracle | Size; collision set |
|---|---|---|---|
| `issues/design-debt/rights-log-names-a-holder-that-does-not-hold-it.md` | A transfer log renders the pre-transfer/source holder after ownership moved. Pass the committed destination identity into the one formatter instead of reconstructing it. | Transfer across processes: old log names the source; fixed log names the table that actually owns the handle. Oracle: handle-table inspection. | S; rights/transfer logging and its tests |
| `issues/diagnostics/blocked-time-is-invisible-while-the-park-lasts.md` | `ProcessStats` reports only completed blocked intervals. Add `now - blocked_since` for the current parked state without mutating cumulative counters. | Hold a task parked across two samples: base remains flat; fixed value increases. Oracle: guest clock timestamps around the park. | S; `kernel/src/process.rs::{stats_of,stats_from}`, stats test |
| `issues/kernel/granularity-bound-crossed-at-four-widths.md` | Checked bounds use a width-dependent intermediate that crosses the promised granularity. Compute in a wider checked domain and convert only after proving the final bound. | Test first crossing and adjacent values at all four widths: old code accepts/wraps one; fixed code refuses by name. Oracle: wider integer reference model. | S; owning pure bound function and host tests |
| `issues/hardware/kernel-log-unreadable-once-userland-owns-the-screen.md` | Once userland owns scanout, the kernel’s only visible diagnostic can disappear on serial-less hardware. Preserve an independent crash snapshot/pager path that can reclaim or overlay the panel without consulting userland. | Hand scanout to userland, then panic: base leaves no readable report; fixed panel contains it. Oracle: host screendump OCR/decoder plus captured kernel snapshot. | L; panic console/framebuffer ownership, screen harness |

## 9. Process, loader and pipe semantics

Must not touch libc/std mappings or the syscall ABI.

| Issue | What is wrong; concrete fix | Negative control; independent oracle | Size; collision set |
|---|---|---|---|
| `issues/kernel/lseek-past-eof-is-silently-clamped.md` | Kernel seek state clamps a legal position to EOF. Preserve the checked requested offset and let the next write create a sparse range. | Seek past EOF then write: base writes at EOF; fixed file has the requested hole. Oracle: direct file size/byte map after remount. | S; kernel file-offset operation, sparse-file test |
| `issues/kernel/process-open-panics-on-a-reopened-process.md` | Reopening an already represented process reaches an assertion instead of a defined handle/error path. Make insertion idempotent or return the existing object before table mutation. | Execute the duplicate-open sequence: base panics naming the assertion; fixed call returns the declared result and leaves one object. Oracle: handle/process-table inventory. | S; process-object open path, regression test |
| `issues/kernel/spawn-thread-disagrees-about-a-reaped-parent.md` | Spawn checks parent liveness separately from committing the new thread, so reap can win between them. Move the decision into `toyos-proclife::Model::spawn_thread` and commit one returned transition. | Loom/interleaving arm reaps between check and commit: base admits an orphan/panics; fixed model refuses or attaches consistently. Oracle: `toyos-proclife` exhaustive model. | M; `toyos-proclife`, kernel process adapter, tests |
| `issues/kernel/the-global-pipe-lock-spans-a-user-copy.md` | Pipe state remains globally locked while a user copy can fault or stall. Reserve/copy through a bounded kernel buffer, release the pipe lock, perform user copy, then commit/rollback the reservation. | Use a faulting/slow destination: base blocks unrelated pipes; fixed unrelated pipe progresses and bytes remain ordered. Oracle: independent pipe transcript plus fault result. | M; `kernel/src/{pipe,object/ops,user_ptr}.rs`, tests |
| `issues/kernel/dlopen-dedup-only-holds-after-the-race-settles.md` | **BLOCKED:** concurrent `dlopen` callers can both allocate/map before late deduplication. Factor reserve/publish/abort into a real pure state machine and exhaust interleavings before changing loader ownership. | Future synchronized duplicate load must red on base. Oracle needed: real state-machine model plus one backing/mapping inventory. | L; future loader model, kernel loader/shared-object cache |

## 10. Scheduler retirement

Use the scheduler’s real interleaving machinery; no guest-only stress is sufficient.

| Issue | What is wrong; concrete fix | Negative control; independent oracle | Size; collision set |
|---|---|---|---|
| `issues/kernel/retire-tripwire-is-not-queue-shaped.md` | One scalar `GIVE_UP` deadline governs arbitrarily many retirement records. Give each queued retirement its own bounded attempt/state and remove only completed entries. | Queue overlapping retirements beyond the old deadline: base abandons/misattributes the batch; fixed code accounts each once. Oracle: retire queue inventory and reclamation count. | M; `kernel/src/scheduler.rs` retire path, stress/model tests |
| `issues/kernel/steal-probe-node-dies-with-its-victim.md` | **BLOCKED:** a steal probe borrows node lifetime from the victim it is trying to retire. Build a model of victim death, probe publication and reclamation using the real node primitive first. | Future victim-retire interleaving must red on base. Oracle needed: Loom model plus exact-once reclamation ledger. | L; future scheduler model, steal/probe node code |

## 11. Panic-path safety

The Loom/model changes precede kernel edits. Preserve the panic-path invariant: no allocation, blocking locks, unchecked arithmetic, trait-object calls, indexing, unwrap or expect.

| Issue | What is wrong; concrete fix | Negative control; independent oracle | Size; collision set |
|---|---|---|---|
| `issues/kernel/fatal-text-safety-comment-claims-a-write-that-recurs.md` | `refresh_capture` can write `SNAPSHOT` while `fatal_text` reads it. Factor one atomic read-modify-write capture state into `kernel-loom` and make readers fall back to `LIVE` when a writer is active. | Revert the whole state machine on the measured base: reader-versus-refresh Loom must red; restore must green while existing models stay green. Oracle: Loom driving the real factored primitive. | M; `kernel/src/drivers/panic_console/mod.rs`, `kernel-loom` capture primitive/tests |
| `issues/panic-path/panic-console-capture-untested.md` | Existing checks cover the latch but not equality/safety of the real snapshot reader/writer paths. Extend the same factored primitive model through capture, refresh and discard. | Remove exclusion/release on each writer branch: the model must name overlap or stale capture. Oracle: Loom plus byte-for-byte captured-text host parser. | M; same panic-console and kernel-loom files as preceding row |
| `issues/panic-path/panic-on-wedged-virtio-console-spins.md` | Panic output waits forever on a virtio console that cannot complete. Give panic transmission a checked fixed poll budget, then continue to the independent panel/snapshot channel. | Stage a device that never completes: base never reaches fallback; fixed path names abandonment and paints the report. Oracle: host deadline plus panel/snapshot content. | M; virtio-console panic flush, panic harness actuator |
| `issues/kernel/no-alloc-error-handler.md` | **BLOCKED:** the kernel has no controlled, allocation-free way to force allocator exhaustion and observe the terminal path. Build an allocator-failure actuator and independent serial/panel capture first. | Future actuator must make the base hit the missing handler. Oracle needed: bounded terminal report with allocator disabled. | M; future allocator actuator, kernel allocation runtime |
| `issues/panic-path/crash-report-preemption-untested.md` | **BLOCKED:** no model expresses preemption state across nested crash-report entry. Factor the state transition into `kernel-loom` before changing panic code. | Future model restores preemptible entry and must red. Oracle needed: real state model plus fallback-channel completion. | M; future model, panic entry/report code |

## 12. Service acceptance oracles

These are not dispatchable until allowed, independent clients/fixtures exist; do not commit host binaries as gates.

| Issue | What is wrong; concrete fix | Negative control; independent oracle | Size; collision set |
|---|---|---|---|
| `issues/isolation/sshd-accept-path-unexercised.md` | **BLOCKED:** sshd’s accept/auth/session path has no end-to-end client. Build a source-based in-tree SSH protocol probe or approve an independent implementation before changing sshd. | Future mutation breaks accept/auth and must be named. Oracle needed: independent protocol implementation, not sshd’s own parser. | L; future SSH probe, sshd tests |
| `issues/isolation/toybox-is-one-row-for-nineteen-applets.md` | **BLOCKED:** one aggregate row cannot prove nineteen applet contracts, and no independent expected-output corpus exists. Define per-applet fixtures from upstream specifications first. | Future mutation breaks one applet and only its row reds. Oracle needed: independent per-applet corpus. | L; future fixtures, toybox applets/harness |

## 13. Rust standard-library fork — machine-exclusive lane

One PR at a time, with no ordinary bundle mixed in. Must not touch ABI sources.

| Issue | What is wrong; concrete fix | Negative control; independent oracle | Size; collision set |
|---|---|---|---|
| `issues/build/std-fork-not-rustfmt-clean.md` | ToyOS std fork sources are outside the formatting gate. Add their exact source set to a fork-local `rustfmt --check` wrapper and format only those files. | Reintroduce a known formatting delta: wrapper must name it. Oracle: upstream `rustfmt --check`. | S; `rust/library/std` ToyOS files, fork-check wrapper |
| `issues/build/std-systemtime-now-returns-the-epoch.md` | ToyOS `SystemTime::now` is an epoch stub. Implement it through the existing ToyOS wall-clock syscall and checked duration conversion. | Restore epoch return: wall-clock Rust test must name the zero/stale time. Oracle: kernel wall-clock report and `toyos-wallclock` conversion. | M; `rust/library/std/src/sys/pal/toyos`, Rust guest wall-clock test |
| `issues/design-debt/std-says-this-machine-has-one-cpu.md` | `available_parallelism` reports one regardless of kernel topology. Call the existing CPU-count syscall and return a nonzero checked `NonZero`. | Boot multi-vCPU with stub restored: base prints one; fixed equals kernel topology. Oracle: kernel CPU enumeration. | S; ToyOS std threading PAL, `std_threading` test |
| `issues/filesystem/std-stat-conflates-io-with-notfound.md` | std’s ToyOS metadata path maps all stat failures to not-found. Preserve `SyscallError::Io` through the PAL error conversion. | Inject backing read failure: base returns `NotFound`; fixed returns I/O. Oracle: raw ToyOS syscall result. | S; ToyOS std fs PAL, Rust guest stat test |
| `issues/build/std-leaks-a-thread-stack-per-spawn.md` | **BLOCKED:** thread exit has no committed mapping/residency instrument that can distinguish released stack pages. Build a bounded spawn/join mapping counter before editing std cleanup. | Future loop restores missing unmap and must show monotonic mappings. Oracle needed: kernel mapping/PMM count per joined thread. | L; future mapping instrument, ToyOS std threading PAL |

## 14. Broken-pipe ABI — single-claimant lane

This PR necessarily touches ABI-facing error definitions and must contain no unrelated commit and no `rust/` change.

| Issue | What is wrong; concrete fix | Negative control; independent oracle | Size; collision set |
|---|---|---|---|
| `issues/isolation/a-broken-pipe-answers-not-found.md` | Writing after the last reader closes is encoded as `NotFound`, erasing pipe semantics. Add/route a dedicated broken-pipe error through kernel syscall result and SDK/libc conversion. | Close reader then write: base says not-found; fixed raw syscall and wrapper both say broken-pipe. Oracle: pipe endpoint state plus raw syscall word. | S; `toyos-abi/src` error, kernel pipe/syscall, `toyos/src`, `userland/libc/src`, tests |

## Already closed while this plan was being prepared

These remain part of the original 73-accounting but are not dispatchable work.

| Former issue | Closure evidence |
|---|---|
| `issues/diagnostics/a-console-tag-is-composed-by-replacing-a-bracket.md` | Commit `86efe080be0642ed1ade616ae6ddbdbe92574530` routes the kernel tag through the record’s formatter; the issue file is gone. |
| `issues/diagnostics/no-guest-can-change-the-display-mode.md` | Commit `0f6937fe89d62185f3accc9cfce19d72522a1a59` adds the guest mode-change operation and QEMU-backed oracle; the issue file is gone. |
