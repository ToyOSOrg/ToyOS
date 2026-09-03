---
status: open
kind: tooling
opened: 2026-09-03
---

# The committed tier declaration disagrees with the nightly's measured profile on 28 names, in both directions

`cargo run -- --merge-durations <dir>` over `ci.yml`'s scheduled run
`33728852421` (2026-09-03T07:35:07Z, `main`, all twelve `durations-shard-*`
artifacts) rewrites 350 rows of `tests/test-durations` and then refuses the
merge. Twelve `Tier::Fast` names are priced over `FAST_COMMIT_MS` or over the
ten-second line:

```
i8042_fadt_denial 8487        sysret_ss_reload 23838
iommu_context_absent 9424     va_exhaustion 8703
log_backing_read_error 8211   xhci_no_interrupt 8929
log_reserve_window 8161       xhci_superspeed_ports 9852
mkdir_cap 8318                xhci_two_controllers 10132
sched_check_build 8032
short_sleep_livelock 8678
```

and sixteen `Tier::Nightly` rows now measure at or under the commitment line,
which is the return direction:

```
console_line_atomicity 6684    log_conservation_smp4 6523
double_panic_names_the_fault 4849  usb_short_read 7258
esp_filesystem 7639            wall_clock_century_register 7175
fat_backing_revoked 7039       wall_clock_rtc_dead 6709
fs_rename_durable 6583         wall_clock_zone 7141
fsync_failed_commit 7881       writeback_spawn 7340
heap_ceiling_recovery 6550     xhci_slot_exhaustion 7548
idle_stack_guard 5964
locale_detect 7177
```

Site: `src/tiers.rs`'s `RELEGATED` and `tests/toyos.rs`'s tier column, against
`tests/test-durations`. Nothing tracked covers these: the two per-name records
in this area name `i8042_health` and `xhci_full_speed_device`, neither of which
is in either list, and no record addresses the return direction at all.

Exit: one landing takes the whole measured profile — `--merge-durations` on a
nightly artifact — and moves each of the 28 to the tier its price earns, so the
command exits 0 on the next nightly.
