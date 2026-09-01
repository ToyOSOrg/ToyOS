# Retired redlist rows

## Result

`src/redlist.rs` contains 62 production `Standing::Retired` rows: **59 HOLDS, 1 SUSPECT, 2 FALSE**.

The unit of this audit is a row, not a test name. A live row under the same name therefore does not reopen an unrelated retired mechanism. `cargo run -- --known-red` reports 129 measurements of 74 names and, in particular, still reports live rows for `desktop_locale_detect`, `dump_nmi_probe`, `screen_blocked_dump`, and `xhci_hid_break` while the rows below retire narrower failures. The retirement-line commits cited below were read with `git blame`; every cited SHA was checked with `git cat-file -t` and resolved as `commit`. Source resolution and registration are also exercised by the root `cargo test --lib` redlist gate.

## FALSE

- **`console_locale_detect`, CI, measured 2026-08-31** (`src/redlist.rs:2516`, retired in `1b140f50f`) — FALSE. The row reports ten typed lines with no whole echo, yet its retirement is `TYPING_PACED`. The still-open `issues/build/the-console-input-path-can-stop-after-a-ps2-overflow.md:26` says ordinary queue loss shortens one line and the next recovers, while an input path that stops explains this row's exact ten-of-ten message; the same file records a shipping-kernel boot that stopped after sixteen set-1 bytes and then took none of the remaining injections. Pacing avoids provoking the open defect but does not establish that the recorded failure was the retired mechanism. **Experiment:** run `console_locale_detect` with `i8042-trace` until the ten-of-ten verdict recurs and read whether `RX_BYTES` continues rising after the first failed line.
- **`desktop_locale_detect`, CI, measured 2026-08-31** (`src/redlist.rs:2538`, retired in `1b140f50f`) — FALSE. It has the same ten-of-ten verdict and call site as the preceding row, and the open defect explicitly names both sightings as the stopped-input-path shape (`issues/build/the-console-input-path-can-stop-after-a-ps2-overflow.md:29`). `shell_type_once` now paces every burst through `await_drained` (`tests/toyos.rs:6280`), but that is avoidance, not evidence that this row's mechanism was queue accumulation. **Experiment:** reproduce this name with the drain counter armed and distinguish a rising `RX_BYTES` count from the recorded all-later-attempts `drained 0` wedge.

## SUSPECT

- **`screen_console_clear`, CI, measured 2026-08-19** (`src/redlist.rs:2262`, retired in `a3bca9522`) — SUSPECT. `console_type_line` now bounds and acknowledges every burst (`tests/toyos.rs:6097`), so a mangled command is prevented, but the retirement text itself says the sighting's capture was not kept and therefore cannot distinguish that from a lost panel update (`src/redlist.rs:2272`). The fix makes the *next* sighting discriminating; it does not settle this one. **Experiment:** repeatedly run `screen_console_clear` while retaining the verified command echo and the panel capture; a failure after the echo is confirmed isolates the graphics/update class from typing.

## HOLDS

### Host timing, harness pacing, and verdict scope

- **`cache_eviction`, CI, 2026-08-28** (`src/redlist.rs:333`, retired in `2b0933dff`) — HOLDS. The current verdict admits an over-budget sample only when `dirty == resident` and requires the last sample back within the bound (`tests/toyos.rs:10440`); the staged all-dirty producer remains registered at `tests/toyos.rs:10316`.
- **`metal_sim_null_audio`, CI, 2026-08-08** (`src/redlist.rs:428`, `f005e34f3`) — HOLDS. The null-sink wait is guest-driven in `tests/common/audio.rs:911`, so the old fixed host-wall window is absent.
- **`late_storage_connect`, CI, 2026-08-08** (`src/redlist.rs:456`, `f005e34f3`) — HOLDS. The test is an ordering stage around `scan_ports`, not a guessed boot-time margin (`tests/toyos.rs:4942`).
- **`late_storage_connect`, CI, 2026-08-09** (`src/redlist.rs:916`, `f005e34f3`) — HOLDS on the same scan-closes-window ordering; the later sighting's slower boot cannot change that staged order.
- **`hda_two_live_refused`, CI, 2026-08-08** (`src/redlist.rs:469`, `f005e34f3`) — HOLDS. It uses the same guest-driven null-sink observation as the preceding audio retirement; the test remains registered at `tests/toyos.rs:8643`.
- **`xhci_slow_connect`, CI quiet 0/5, 2026-08-08** (`src/redlist.rs:681`, `201f75d0f`) — HOLDS. The later four-run measurement is the retirement, and the timing-sensitive test is explicitly isolated as Nightly (`tests/toyos.rs:900`) with its current margin calculation in `tests/common/usb.rs:1181`.
- **`xhci_slow_connect`, CI seen, 2026-08-08** (`src/redlist.rs:693`, `201f75d0f`) — HOLDS on the same remeasurement and current Nightly ownership; no later row records this one-millisecond-clearance shape.
- **`metal_sim_pointer_churn`, CI, 2026-08-08** (`src/redlist.rs:772`, `f005e34f3`) — HOLDS. The current test paces and counts all churn cycles before taking the bound-source verdict (`tests/toyos.rs:12155`, `tests/toyos.rs:12225`).
- **`screen_pager_keys`, CI, 2026-08-09** (`src/redlist.rs:928`, `925d89314`) — HOLDS. `PAGER_ARITHMETIC` replaced the host-speed thirty-key injection with per-key guest-budgeted observation; the live test remains at `tests/toyos.rs` under the named source.
- **`screen_pager_keys`, dev host alone, 2026-08-08** (`src/redlist.rs:1179`, `925d89314`) — HOLDS on the same paced verdict; the former page-count arithmetic is no longer executable.
- **`handle_lifetime`, dev host loaded, 2026-08-15** (`src/redlist.rs:1349`, `e1642a2f3`) — HOLDS. The binary samples until two free-memory readings agree, bounded by 100 samples (`tests/toyos-rust-tests/src/bin/handle_lifetime.rs:214`).
- **`handle_lifetime`, CI, 2026-08-19** (`src/redlist.rs:1395`, `e1642a2f3`) — HOLDS on the same quiescent reading; a kernel that frees nothing stabilizes immediately and still fails the value check.
- **`desktop_typing_damage`, dev host loaded, 2026-08-06** (`src/redlist.rs:1546`, `f005e34f3`) — HOLDS. The harness waits for the new `terminal: ready` marker (`tests/toyos.rs:6373`, emitted at `userland/terminal/src/main.rs:75`) before typing.
- **`desktop_audio_client`, dev host loaded, 2026-08-06** (`src/redlist.rs:1564`, `f005e34f3`) — HOLDS on the same `terminal: ready` boundary.
- **`desktop_window_child`, dev host loaded, 2026-08-06** (`src/redlist.rs:1577`, `f005e34f3`) — HOLDS. `close_focused_window` now waits from the new capture position (`tests/toyos.rs:6456`) rather than accepting an earlier `windows=1` line; the separate live guest-silence rows remain standing.
- **`null_sink_client_exits`, CI, 2026-08-15** (`src/redlist.rs:2015`, `819f45141`) — HOLDS. `settle_null_sink_client_exits` waits for both removals before the verdict window closes (`tests/toyos.rs:2066`).
- **`null_sink_client_exits`, CI, 2026-08-16** (`src/redlist.rs:2041`, `819f45141`) — HOLDS on the same guest-liveness settle.
- **`console_locale_detect`, CI, 2026-08-20** (`src/redlist.rs:2590`, `2b0933dff`) — HOLDS. Unlike the two false rows, this capture shows a specifically mangled `locale detect` command; the currently paced and whole-echo-verified `shell_type_line` (`tests/toyos.rs:6257`) prevents that producer. The named fix `7a033450` resolves to a commit.
- **`log_conservation_smp4`, CI, 2026-08-28** (`src/redlist.rs:2978`, `c49b07c42`) — HOLDS. The test is now Nightly with `Why::Cost` (`src/tiers.rs:394`) while `smp1` and `smp8` retain the fast-lane subject shapes.
- **`sysret_ss_reload`, CI, 2026-08-29** (`src/redlist.rs:3011`, `959e139ae`) — HOLDS. The probe uses `drain_until` with a ten-second liveness ceiling (`tests/toyos.rs:8173`) rather than a fixed 500 ms sampling window.

### USB, console atomicity, and device accounting

- **`usb_transport_break`, CI 5/5, 2026-08-08** (`src/redlist.rs:356`, `f005e34f3`) — HOLDS. The retired driver ordering is no longer present, and the current transport test remains registered through `tests/common/usb.rs`; no later row carries the same reset-while-answerable signature.
- **`usb_transport_break`, CI seen, 2026-08-13** (`src/redlist.rs:377`, `59993dc82`) — HOLDS. `broke_on` scopes the staged disk (`tests/common/usb.rs:1682`) instead of summing another device's recovered break.
- **`xhci_hid_break`, CI 2/15, 2026-08-11** (`src/redlist.rs:945`, `a35c18257`) — HOLDS. `hid_broke_on` returns both controller/device identity fields (`tests/common/usb.rs:2191`); the later live timeout and recovery rows are distinct.
- **`xhci_hid_break`, CI seen, 2026-08-10** (`src/redlist.rs:979`, `a35c18257`) — HOLDS on the same per-device endpoint count.
- **`xhci_hid_break`, CI seen, 2026-08-12** (`src/redlist.rs:994`, `a35c18257`) — HOLDS on the same per-device endpoint count.
- **`desktop_audio_client`, CI, 2026-08-09** (`src/redlist.rs:840`, `f005e34f3`) — HOLDS. Soundd emits one `write_all` per line (`userland/soundd/src/main.rs:53`) and every holder's `ConsoleObject` buffers a whole line (`kernel/src/object/device.rs:191`); the independent `console_line_atomicity` test is registered at `tests/common/console.rs:58`.
- **`hda_tone`, dev host loaded, 2026-08-07** (`src/redlist.rs:1204`, `87d3c2346`) — HOLDS. The record splice is excluded by the same per-holder `ConsoleObject` and single `BackendGuard` path (`kernel/src/object/device.rs:197`, `kernel/src/drivers/serial.rs:91`).
- **`hda_tone`, dev host alone quiet, 2026-08-07** (`src/redlist.rs:1224`, `87d3c2346`) — HOLDS. It was only the control for the now-impossible splice and makes no independent red claim.
- **`71_macro_empty_arg`, dev host loaded, 2026-08-15** (`src/redlist.rs:1698`, `2018996f3`) — HOLDS. `common::console::verdict` filters unrelated speakers and handles unterminated program output; `c_capture_ignores_daemon_lines` exercises it (`tests/common/console.rs:371`, `tests/common/console.rs:457`).
- **`screen_diag_boot`, CI 2/2, 2026-08-15** (`src/redlist.rs:1854`, `a136c9b92`) — HOLDS. The assertion reads `LOG_ON_CONSOLE_AND_FILE` from the single declaration beside `NO_LOG_ALERT` (`tests/common/volumes.rs:2302`, `tests/common/volumes.rs:2319`).
- **`i8042_keyboard`, CI, 2026-08-19** (`src/redlist.rs:2124`, `2cbb3e0db`) — HOLDS. Every keyboard injection is bounded by `QEMU_PS2_QUEUE` and paced against guest key-event evidence (`tests/toyos.rs:7549`).
- **`i8042_no_spurious_wake`, CI, 2026-08-19** (`src/redlist.rs:2156`, `2cbb3e0db`) — HOLDS on the same queue accounting; the Pause and following key are acknowledged separately.
- **`screen_console_panic`, CI, 2026-08-19** (`src/redlist.rs:2184`, `f9b16f654`) — HOLDS. `console_type_line` acknowledges each bounded burst before pressing Enter (`tests/toyos.rs:6097`).
- **`screen_console_panic`, CI, 2026-08-23** (`src/redlist.rs:2214`, `f9b16f654`) — HOLDS on the same exact queue-width command-loss mechanism.
- **`i8042_undecoded_bytes`, dev host loaded 3/14, 2026-08-15** (`src/redlist.rs:1637`, `c185de8ab`) — HOLDS. The tally is one packed `AtomicU64` and `Counts` comes from one load (`kernel/src/drivers/i8042/tally.rs:21`, `kernel/src/drivers/i8042/tally.rs:40`); the real primitive is used by `kernel-loom/tests/i8042_tally.rs`.
- **`i8042_undecoded_bytes`, dev host loaded 6/10, 2026-08-15** (`src/redlist.rs:1734`, `c185de8ab`) — HOLDS on the same packed tally and producer-order fix.
- **`i8042_undecoded_bytes`, CI, 2026-08-16** (`src/redlist.rs:2060`, `2b0933dff`) — HOLDS. The health state distinguishes `HEALTH_MUTE_BLIND` from `HEALTH_MUTE_SAID` (`kernel/src/drivers/i8042/mod.rs:127`) and the split-burst interleaving is staged at `tests/toyos.rs:11907`.

### Panic, scheduler, and machine-death mechanisms

- **`dump_nmi_probe`, CI 1/5, 2026-08-08** (`src/redlist.rs:493`, `f005e34f3`) — HOLDS. `deaf_window` spins directly on `rdtsc` to a `tsc_deadline` (`kernel/src/sched/dump.rs:240`, `kernel/src/sched/dump.rs:272`), so `u128_div_rem` cannot be sampled inside the staged loop.
- **`dump_nmi_probe`, CI 2/10, 2026-08-09** (`src/redlist.rs:815`, `f005e34f3`) — HOLDS on the same direct-TSC spin.
- **`dump_nmi_probe`, CI seen, 2026-08-09** (`src/redlist.rs:900`, `f005e34f3`) — HOLDS. Timeout recovery uses one `compare_exchange(ASKED, IDLE)` and a failed CAS continues to `request()` (`kernel/src/sched/dump.rs:299`). The live loaded-host row is a different mechanism.
- **`sched_check_build`, CI, 2026-08-16** (`src/redlist.rs:1045`, `27340c92e`) — HOLDS. Pass cost is recorded rather than panicking, and KVM is judged against its recorded baseline (`tests/common/passcost.rs:156`).
- **`sched_check_build`, dev host alone, 2026-08-15** (`src/redlist.rs:1097`, `27340c92e`) — HOLDS. The panic is absent; TCG reports against a baseline that deliberately takes no magnitude verdict (`tests/common/passcost.rs:228`).
- **`sched_check_build`, dev host loaded 6/10, 2026-08-17** (`src/redlist.rs:1126`, `6b84f32db`) — HOLDS on that same TCG `Judgement::Report` path, so host load cannot recreate the retired verdict.
- **`metal_sim_pointer_churn`, CI kernel death, 2026-08-10** (`src/redlist.rs:1008`, `5a224bd1d`) — HOLDS. `answer_steal_requests` refuses to pop the loaded context (`toyos-sched/src/cpu.rs:1884`) and every Ring-0 entry clears DF (`kernel/src/arch/entry.rs:48`). The named `5e74971e` resolves to a commit.
- **`sched_stress`, dev host loaded, 2026-08-19** (`src/redlist.rs:2454`, `b803af2d2`) — HOLDS. The run-queue handoff now calls `pop_surplus(self.cpu.loaded_key())` (`toyos-sched/src/cpu.rs:1903`), excluding the context on which the pass still stands.
- **`screen_fatal_halt`, dev host loaded, 2026-08-15** (`src/redlist.rs:1796`, `5a224bd1d`) — HOLDS. Ring-0 entries prepend `cld` and retain the `entry-df-unclean` negative control (`kernel/src/arch/entry.rs:48`, `kernel/src/arch/entry.rs:65`); `5e74971e` resolves to a commit.
- **`double_fault_stack`, dev host loaded, 2026-08-15** (`src/redlist.rs:1828`, `5a224bd1d`) — HOLDS on the same silent-reset direction-flag mechanism.
- **`diskless_boot`, dev host loaded, 2026-08-19** (`src/redlist.rs:2636`, `5a224bd1d`) — HOLDS on the same status-zero reset signature and entry `cld` fix.
- **`screen_blocked_dump`, dev host loaded, 2026-08-07** (`src/redlist.rs:1489`, `f005e34f3`) — HOLDS for its old compositor-overlay/census loss. The current open source and live rows explicitly preserve later no-verdict shapes (`issues/diagnostics/blocked-dump-cannot-fire-on-a-total-freeze.md:104`).
- **`metal_sim_window_caps`, dev host loaded, 2026-08-07** (`src/redlist.rs:1595`, `f005e34f3`) — HOLDS. The shootdown acknowledgement primitive is shared directly into Loom (`kernel-loom/src/lib.rs:152`) and `an_initiator_answers_while_it_waits` remains at `kernel-loom/tests/tlb_shootdown.rs:210`.
- **`null_sink_shipped_client`, dev host loaded, 2026-08-07** (`src/redlist.rs:1613`, `f005e34f3`) — HOLDS on the same two-initiator shootdown mechanism.
- **`panic_recovery`, CI, 2026-08-21** (`src/redlist.rs:2772`, `5f06ebb4e`) — HOLDS. Panic symbol resolution reads the running task's own record without `PROCESS_TABLE` (`kernel/src/process.rs:1606`), so the exact `<symbol unread: the process table was held>` string is unreachable.
- **`fault_gates`, CI, 2026-08-22** (`src/redlist.rs:2805`, `5f06ebb4e`) — HOLDS on the same lock-free symbol source; later task-record concession strings are distinct by construction.

### Filesystem, network, and operation retries

- **`hda_client_stall`, CI, 2026-08-08** (`src/redlist.rs:724`, `3f739c61a`) — HOLDS. The scheduler idle loop explicitly touches no filesystem (`kernel/src/sched/driver.rs:731`), removing the lock cycle named by the row.
- **`boot_partition_identity`, dev host loaded, 2026-08-15** (`src/redlist.rs:1764`, `aa5c7be6e`) — HOLDS. `toyos::net::hangup` maps a disconnected read and `SyscallError::Gone` to `NetdNotFound` (`toyos/src/net.rs:351`); the three-path guest oracle remains at `tests/toyos-rust-tests/src/bin/netd_gone_mid_bind.rs`. The named `f12b684f` resolves to a commit.
- **`boot_partition_identity`, CI seen, 2026-08-15** (`src/redlist.rs:1881`, `aa5c7be6e`) — HOLDS on the same teardown mapping.
- **`boot_partition_identity`, CI 1/4, 2026-08-17** (`src/redlist.rs:1903`, `aa5c7be6e`) — HOLDS on the same mapping; later `Gone` vocabulary superseded the former separate send/write arms without restoring the panic.
- **`esp_filesystem`, dev host loaded seen, 2026-08-21** (`src/redlist.rs:2834`, `8479cd5cd`) — HOLDS. `object/ops.rs` retries `WouldBlock` flushes above the pinned path and bounds the loop by `block::DEADMAN` (`kernel/src/object/ops.rs:517`, `kernel/src/object/ops.rs:552`); `log_flush_retry` remains registered at `tests/common/volumes.rs:2468`.
- **`esp_filesystem`, dev host loaded 1/73, 2026-08-22** (`src/redlist.rs:2854`, `8479cd5cd`) — HOLDS on the same operation-level retry. The named historical `5479129d` resolves to a commit; the current code no longer makes a timeout consume the whole retry opportunity.

## Record-quality conclusion

The table's structural gate is doing useful work: every retired row still has a registered test and a resolving source. The weakness is semantic rather than referential. Two rows are retired against a mechanism that the tracker explicitly says does not explain their failure, and one row is retired while its own reason admits the evidence cannot choose between the fixed producer and an unfixed class. No source or issue file was edited here.
