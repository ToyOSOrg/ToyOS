---
status: open
kind: defect
opened: 2026-09-14
---

# A reboot quiesces xHCI and nothing else; a device whose holder never exits keeps bus mastering across it

Every reset this kernel performs goes through `acpi::reboot`
(`kernel/src/drivers/acpi.rs:283`) or `acpi::shutdown` (`:325`), and both call
`stop::before_reset()` (`:313`, `:331`) before touching a register — that
`stop` is `kernel/src/drivers/xhci/stop::before_reset`, one device class.
Nothing else in the reset path — `kernel/src/arch/syscall/machine.rs`'s
`quiesce` (`:47`), which `sys_reboot` (`:113`) and `sys_shutdown` calls before
`acpi::reboot()` (`:122`) / `acpi::shutdown()` (`:108`) — tears down any other
device a process still holds.

**A PCI function's own teardown exists, and it is reached from exactly one
place.** `pcidev::release` (`kernel/src/pcidev/mod.rs:782`) calls
`tear_down` (`:796`), which disables bus mastering (`:797`), masks the
function's MSI/MSI-X entry (`:798`), drops its IOMMU domain
(`:799`, `note_user_owned(…, None)`), and only then resets the function
(`:809`). The one caller of `pcidev::release` in the whole kernel
(`rg -n pcidev::release kernel/src/`, one hit) is `kernel/src/device.rs:75`,
inside `Claim`'s `Drop` — reached when a process's PCI-function handle is
dropped, which for an ordinary claim is when the holding process's handle
table is torn down, i.e. when it exits.

**So on any boot where the claiming process never exits, a reboot or shutdown
leaves that function exactly as it was.** `netd` holds the I219's claim for
the whole life of a `lancase`-family boot and does not exit on `SYS_REBOOT` —
nothing in `quiesce` asks it to — so the syscall that resets the machine never
runs `Claim`'s `Drop` for that slot, `pcidev::release` is never called, and the
I219 warm-resets with bus mastering on, its MSI still enabled, both rings still
programmed and `IMS` still set from whatever `toyos-i219`'s `open` last wrote.

## What this costs

A machine that resets with an armed, bus-mastering NIC still holding queue
addresses from the boot that just ended hands the next boot (Ubuntu, on the
bench) a device in a state its own driver did not put it in. Distinguishing
"the kernel never got far enough to come back" from "the kernel came back but
the wire the harness polls did not" is exactly the ambiguity this leaves at
the bench: a probe that only checks whether the machine answers `ssh` again
cannot tell the two apart, and nothing tears the NIC down to make them
distinguishable.

## What would answer it

Either `quiesce` tears down every device claim before the reset it precedes —
which is the general form `issues/kernel/quiesce-runs-while-userland-still-does-io.md`
already asks for, for a different reason (in-flight I/O rather than device
state) — or the reset path calls each claimed function's own `tear_down`
directly, the way it already does for xHCI through `stop::before_reset`,
instead of relying on a `Drop` a live process is never asked to run.
