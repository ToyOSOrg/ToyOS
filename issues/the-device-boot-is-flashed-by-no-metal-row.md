---
status: open
kind: finding
opened: 2026-10-21
---

# The device boot is flashed by no metal row

`tests/images/metaldevicecase.toml`, the boot whose `usbwrite`, `usbread`,
`fbcheck`, `fbfill` and `fbread` jobs measure the stick and the framebuffer
(`tests/common/devices.rs`), was ridden by one row, `usb_reset_hands_devices_back`,
which went with the kernel's xHCI (the small-kernel track's step 10). No row
names its boot now, so its jobs' timings are taken on no machine; only its
config's job list is still checked (`the_config_runs_exactly_these_jobs`).

**Exit**: a row that rides the device boot and reports its jobs, or the boot
and its jobs deleted. Owner: `tests/toyos.rs`'s metal profile, held by the
orchestrator, which schedules the benches' boots.
