# Guest gate audit: absence and refusal pass

This is a reading audit, not a test run.  For every row I asked what plausible
wrong or partial implementation would still pass the cited assertion.  `SOUND`
means a concrete wrong implementation is rejected by the gate; it does not mean
the gate proves properties outside its stated scenario.

## Gate-by-gate results

| Gate | Deciding assertion | Verdict | What still passes, or what makes it fail |
|---|---|---|---|
| `screen_log_absent` | `tests/toyos.rs:3164` | **WEAK** | A fallback log file that is still created but whose `logd: this boot's kernel log is` announcement is renamed or removed passes; this arm never reads the image. |
| `screen_late_panic` | `tests/toyos.rs:4044` | **SOUND** | Re-reading the live ring at paint time puts `AFTER_CAPTURE` on the panel and fails, while line 4038 first proves that exact needle reached the console. |
| `screen_recoverable_untouched` | `tests/toyos.rs:4760` | **NARROW** | A recoverable panic that paints transiently and is repainted by the compositor before the second screendump passes; the gate proves equal endpoints, not an untouched interval. |
| `xhci_msi_only` | `tests/toyos.rs:9167` | **SOUND** | Retaining MSI-X, claiming polled mode, binding a storage device that can drain the ring, or programming MSI incorrectly fails the paired mechanism and delivered-input checks. |
| `xhci_no_interrupt` | `tests/toyos.rs:9317` | **SOUND** | The former wrong implementation that announces a HID on the refused controller populates `parse_xhci_binds` and fails; this no longer depends on the retired `ready on slot` spelling. |
| `nvme_wide_sector` | `tests/toyos.rs:9532` | **WEAK** | A driver that prints the named refusal and then continues into block-device construction after the ready marker passes because the capture ends at the refusal and is not extended. |
| `klogd_panic_halts` | `tests/toyos.rs:9971` | **SOUND** | Recovering the fatal klogd panic reaches the ordinary ready marker during the explicit three-second post-report drain and fails; the opposite `usbd` arm must reach it. |
| `reentry_names_the_first_panic` | `tests/toyos.rs:10053` | **SOUND** | Letting the ordinary first-panic report start before the staged report panic emits `PANIC:` before `PANIC REENTRY` and fails, while both panic identities are required positively. |
| `double_panic_names_the_fault` | `tests/toyos.rs:10107` | **SOUND** | Running the ordinary fault report before the second panic emits `FAULT rip=` before `DOUBLE PANIC` and fails; the combined and raw reports must also name the fault. |
| `nested_fault_is_recursive` | `tests/toyos.rs:10181` | **WEAK** | Printing `RECURSIVE` and then continuing into a second crash report can pass: the ready-marker capture stops at `RECURSIVE`, and the immediate UART snapshot is not a post-marker drain. |
| `pre_idle_wedge_speaks` | `tests/toyos.rs:10239` | **VACUOUS** | `ready_marker` is the earlier `WEDGE` line, and `boot_log` contains only lines through that marker; later `Boot: peripherals ready` and `Boot: complete` therefore cannot establish that the machine stayed wedged. |
| `i8042_no_spurious_wake` | `tests/toyos.rs:7515` | **SOUND** | Waking on the swallowed Pause fails, and a filter that merely reports no drains fails the zero-event and real-key positive controls at lines 7525 and 7536. |
| `i8042_health_cadence` | `tests/toyos.rs:10979` | **SOUND** | A timer-driven reporter produces lines during the staged three-second silence and makes the exact two-line count fail; both injected keystrokes are positive controls. |
| `driver_wait_refused` | `tests/toyos.rs:11362` | **WEAK** | Drivers may print both timeout refusals and still expose or bind the stuck devices; the gate never asserts that either device is absent after the boot. |
| `i8042_absent` | `tests/toyos.rs:12095` | **SOUND** | Falling into the sixteen bounded handshake waits instead of taking the floating-bus exit fails the named refusal and the paired boot-time comparison. |
| `sshd_fail_closed` | `tests/toyos.rs:12373` | **WEAK** | sshd can listen without emitting the exact `sshd: listening on port 22` line; no connection or socket-table probe independently establishes that port 22 stayed closed. |
| `input_claim_absent` | `tests/toyos.rs:12693` | **SOUND** | Either claim succeeding or returning an error other than `NotFound` panics in `tests/toyos-rust-tests/src/bin/input_absent.rs`; the host also proves the devices are absent from argv. |
| `console_line_atomicity` | `tests/common/console.rs:125` | **SOUND** | Byte-interleaving two writers creates a mixed line and fails, while complete sequence sets for both writers prevent an empty or truncated capture from passing. |
| `c_capture_ignores_daemon_lines` | `tests/common/console.rs:596` | **SOUND** | Disabling the filter makes the same staged capture compare unequal, and a non-daemon line must survive; filtering everything or nothing fails an explicit arm. |
| `serial_vocabulary` | `tests/common/serial.rs:393` | **SOUND** | A `must_not_say` that ignores a present line, an empty capture, or a capture with no kernel output fails the self-check at lines 393--396. |
| `double_fault_stack` | `tests/common/faults.rs:93` | **SOUND** | Corrupting the IST guard or merely omitting a recognized intact verdict fails, and the measured high-water mark must retain a half-stack margin. |
| `idle_stack_guard` | `tests/common/faults.rs:179` | **SOUND** | Leaving the guard mapped returns from the debug read and fails; the positive page-fault identity and zero PTE prevent silence from passing. |
| `virtio_net_no_msix` | `tests/common/faults.rs:282` | **SOUND** | Keeping MSI-X fails the absence, while logging a refusal but still handing netd a NIC fails the independent `NETD_EXITS` check at line 285. |
| `diskless_boot` | `tests/common/faults.rs:336` | **SOUND** | Retaining the old fatal/no-controller path fails either cleanliness or boot completion; argv proves the machine really has no NVMe device. |
| `boot_partition_identity` | `tests/common/gpt.rs:168` | **SOUND** | Accepting the shifted decoy emits the forbidden device-1 claim, and the same test positively observes the exact claim on the agreeing arm and the real USB partition on this arm. |
| `hda_client_stall` | `tests/common/hda.rs:251` | **WEAK** | A double completion whose diagnostic is renamed or removed passes if resumes and aggregate counters still look healthy; the exact forbidden line has no positive control. |
| `hda_two_live_refused` | `tests/common/hda.rs:318` | **WEAK** | The kernel may print the ambiguity refusal and still bind one controller under a changed or suppressed `bound, statests=` line; null-sink availability does not prove no hardware bind occurred. |
| `iommu_discovery` | `tests/common/iommu.rs:62` | **SOUND** | Always reporting no IOMMU fails on the three present-unit arms, whose decoded fields and raw registers must move with independent QEMU knobs. |
| `iommu_context_absent` | `tests/common/iommu.rs:221` | **SOUND** | Passthrough or leaving a context present fails to produce the required real DMA fault, and a fault at another layer or for another function fails the parsed stream and reason. |
| `iommu_empty_domain` | `tests/common/iommu.rs:269` | **SOUND** | Stopping at the context walk or using passthrough yields the wrong reason or no fault; the fault must be a second-level decision inside the mapped extent. |
| `foreign_disk_untouched` | `tests/common/storage.rs:102` | **NARROW** | A stray write outside the first 1 MiB and last 4 KiB passes `write_fingerprint`; the gate covers the formatter's known footprint, not every byte implied by “untouched.” |
| `home_budget_refusal_retried` | `tests/common/storage.rs:258` | **SOUND** | Marking refused pages clean or falsely reporting the retry durable fails when the host bcachefs reader compares the exact file pattern from the NVMe image. |
| `toybox_cp_volume` | `tests/common/toybox.rs:243` | **SOUND** | Leaving the refused destination or a partial file fails the host directory walk, while the independent checker and byte comparisons catch damaged allocation state. |
| `usb_storage_gate` | `tests/common/usb.rs:295` | **SOUND** | Claiming or writing the unstamped disk fails both the designation scan and host fingerprint; the positive disk is checked in both directions with independently seeded bytes. |
| `usb_short_read` | `tests/common/usb.rs:397` | **SOUND** | Returning stale tail bytes as a successful short read makes `refused=true` false, and the named short count plus the remaining sweep and host bytes prevent a dead-device pass. |
| `usb_pool_exhausted` | `tests/common/usb.rs:639` | **SOUND** | Serving the refused disk out of another pool block changes a fingerprint region containing every block this gate would write, while the bound disk must verify normally. |
| `usb_storage_shapes` | `tests/common/usb.rs:712` | **SOUND** | Truncating and binding the 3 TB disk changes the device count, while rejecting all unusual devices fails the independently verified 4 KiB-sector arm. |
| `usb_storage_write_error` | `tests/common/usb.rs:768` | **SOUND** | Hard-wiring writes to success leaves `wr_err=0`; failing the whole disk loses the positive read, and a write that reaches the backing file fails the host fingerprint. |
| `usb_flush_optional` | `tests/common/usb.rs:915` | **SOUND** | Treating the optional-command refusal as failure loses the on-device boot/shutdown log, while swallowing a genuine failure fails the second arm's one-give-up and bounded-flush assertions. |
| `xhci_deaf_registers` | `tests/common/usb.rs:1140` | **SOUND** | Binding a disk behind the non-resetting port changes the device count; refusing immediately rather than waiting the budget and taking down the whole machine also fail explicit checks. |
| `xhci_portsc_rw1c` | `tests/common/usb.rs:1416` | **SOUND** | Disabling a port produces a refusal or a connected/enabled count mismatch, and all attached devices must also enumerate and bind. |
| `usb_transport_break` | `tests/common/usb.rs:1613` | **SOUND** | An illegal recovery command either appears by name or prevents the positive write result and host-side verification of every instructed block. |
| `xhci_full_speed_device` | `tests/common/usb.rs:1883` | **SOUND** | An unfilled descriptor buffer exposes an all-zero identity, while hard-coding the EP0 size fails because the two real devices require different values and only one may be corrected. |
| `xhci_hid_break` | `tests/common/usb.rs:2167` | **SOUND** | An illegal recovery command or a dropped requeue prevents the post-break keyboard word and pointer delta from reaching the guest; both struck devices are positively identified. |
| `usb_refused_disk_first` | `tests/common/usb.rs:2871` | **SOUND** | Leaking the refused device's pool block prevents the replacement from binding at `+0x10000` or emits the pool refusal; ordering and the boot-stick control are asserted separately. |
| `esp_filesystem` | `tests/common/volumes.rs:386` | **SOUND** | Any forbidden file or changed boot artifact is found by the host FAT reader, while allowed writes on `/log` keep refusal-all-writes from passing. |
| `writeback_durability` | `tests/common/volumes.rs:941` | **SOUND** | Closing a queue without draining loses length or bytes in the file read independently from the image, and filesystem-format damage fails the checker first. |
| `fat_backing_revoked` | `tests/common/volumes.rs:1074` | **SOUND** | Keeping the victim, exposing its freed data through the attacker, or corrupting allocation state fails the host FAT reader/checker and exact-byte controls. |
| `fsync_failed_commit` | `tests/common/volumes.rs:1154` | **SOUND** | Returning success from either fsync panics the guest and makes its exit nonzero; the host also requires evidence that the injected device refusal actually fired. |
| `fs_rename_durable` | `tests/common/volumes.rs:1483` | **SOUND** | Losing either overwrite destination or self-renamed file, changing its payload, or damaging the FAT fails the independent host-side reader/checker. |
| `fs_dirs_durable` | `tests/common/volumes.rs:1601` | **SOUND** | Leaving the removed directory, omitting or mis-typing the kept directory, or leaving contents inside it fails the host FAT directory walk. |
| `late_storage_connect` | `tests/common/volumes.rs:1688` | **SOUND** | Binding during the deliberately empty scan fails the zero-device premise, while never resuming discovery fails the later mount and on-device log checks. |
| `log_backing_read_error` | `tests/common/volumes.rs:1910` | **SOUND** | Merging a partial write into invented zeros changes the host-staged file on the image; returning successful data/write also fails the guest verdicts. |
| `boot_volume_metadata_error` | `tests/common/volumes.rs:2022` | **WEAK** | A partial adapter that maps the device refusal from `NotFound` to an unrelated error such as `PermissionDenied` passes; the gate rejects success and `NotFound` but never requires an I/O/device error. |
| `log_partition_identity` | `tests/common/volumes.rs:2416` | **SOUND** | Mounting or writing the GUID-mismatched partition creates a log name visible to the host FAT reader; the same boot must retain `/boot` and complete. |
| `log_flush_retry` | `tests/common/volumes.rs:2568` | **SOUND** | Dropping dirty pages on the budget refusal fails the exact host-read blob, while the deadman and failed-reset arms require distinct declared failure paths. |
| `wall_clock_rtc_dead` | `tests/common/wallclock.rs:423` | **SOUND** | Substituting any epoch produces a dated filename, while silence or a dead boot fails the named refusal, userland refusal, nonempty log, and completion checks. |
| `wall_clock_rtc_unstable` | `tests/common/wallclock.rs:423` | **SOUND** | Accepting an unstable RTC produces a dated filename; the helper requires the actuator-specific reason and the same kernel/userland refusal agreement as the dead arm. |
| `wall_clock_no_century` | `tests/common/wallclock.rs:488` | **SOUND** | Reading the hard-coded century register produces 2133 instead of the expected 2033 because the register is deliberately staged one century away. |

## Ranked non-SOUND findings

Worst first, ranked only by how much the tree would believe wrongly if this
gate were the sole support for its claim:

1. **`pre_idle_wedge_speaks` — VACUOUS.** The tree would claim a pre-idle
   machine stayed wedged from absences measured in a capture that necessarily
   ends at the earlier wedge marker.
2. **`foreign_disk_untouched` — NARROW.** A disk-safety interlock named
   “untouched” observes only the formatter's known head/tail footprint; a write
   elsewhere on an owner's disk is invisible.
3. **`sshd_fail_closed` — WEAK.** The authentication boundary rests on the
   absence of one listening announcement, with no attempted connection or
   socket-state oracle.
4. **`screen_log_absent` — WEAK.** The machine can still create a fallback log
   and pass by changing only its announcement; the image is never inspected.
5. **`driver_wait_refused` — WEAK.** The gate can report both bounded refusals
   yet still expose the devices, while its success text claims the boot came up
   without them.
6. **`hda_two_live_refused` — WEAK.** A controller may bind despite the
   ambiguity if the old bind-line spelling disappears.
7. **`nvme_wide_sector` — WEAK.** Work after the refusal marker is outside the
   capture used for the downstream-absence verdict.
8. **`nested_fault_is_recursive` — WEAK.** A recursive marker followed by a
   second report can escape an immediate, non-drained UART snapshot.
9. **`boot_volume_metadata_error` — WEAK.** Translating device failure to any
   non-`NotFound` error is enough, even when it is not an I/O/device error.
10. **`hda_client_stall` — WEAK.** The double-completion property is guarded by
    an uncontrolled diagnostic spelling.
11. **`screen_recoverable_untouched` — NARROW.** Two equal screenshots do not
    rule out a transient panic paint between them.

## Counts

| Verdict | Count |
|---|---:|
| SOUND | 48 |
| WEAK | 8 |
| NARROW | 2 |
| TESTED-LENIENCY | 0 |
| VACUOUS | 1 |
| **Total** | **59** |

## UNCOVERED

- `foreign_disk_untouched` has no whole-device write observation; its helper
  reads the first 1 MiB and last 4 KiB only.
- No existing assertion in `sshd_fail_closed` asks the running guest or network
  stack whether port 22 is actually closed.
- No existing assertion in `screen_recoverable_untouched` observes the panel
  continuously between its two screendumps.

## Stopping point

Stopped before priority 2 at
`tests/toyos-rust-tests/src/bin/boot_volume_metadata_error.rs`, after reading
that guest arm in full to finish the `boot_volume_metadata_error` host verdict.
The other guest-bin files were not swept, and priority 3 (remaining presence
gates in `tests/toyos.rs`) was not started.  Within priority 1 this pass covered
the absence/refusal gates listed above; it did not attempt to relabel every
generic panic scan or every positive gate that happens to contain a defensive
negative assertion.
