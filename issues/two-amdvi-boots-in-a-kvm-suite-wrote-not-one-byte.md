---
status: open
kind: defect
opened: 2026-10-28
---

# Two amdvi boots in a KVM suite wrote not one byte

A full `cargo test --test toyos-build` at `0eb2c09d0` on a Linux x86_64 KVM
host went red on `amdvi_selftest` and `amdvi_firmware_left`, and on nothing
else. Both boots used `tests/images/testcases.toml`, finished building in the
same second and timed out together after 63 s. Both reported `the console
carried: nothing at all` and `uart: nothing at all`. So the 16550 file was
empty, without even the firmware's screen-clearing escapes that the netcase
boots in
`issues/a-netcase-boot-under-host-load-said-nothing-past-the-firmwares-screen-clears.md`
wrote. Both tests passed in that host's suites at `e4bdb0a5d` and `7745af98d`.
`7745af98d` differs from `0eb2c09d0` only in a host-side wait inside
`the_log_lands_on_the_boot_stick`. That suite ran alone on the host, and its
load during these boots was not recorded. Log:
https://github.com/ToyOSOrg/evidence/tree/d06821d0cb6e754f1fec16a5921bc4378d9ba600/pr-1107/r2

Not reduced: whether QEMU or the firmware ever ran in those boots. A QMP read
of a silent guest's registers would say. No `red-run-serial` file was empty,
so the empty 16550 files were not kept.

**Exit**: a silent boot's registers read, naming where it stopped, and the fix
at that cause. If it is the netcase boots' cause, fold this record into that one.

**Owner**: the orchestrator.
