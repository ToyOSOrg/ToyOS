---
status: open
kind: defect
opened: 2026-09-16
---

# A reset stops xHCI and nothing else: a claimed PCI function whose holder never exits is left armed when the register is written

**The reset paths, as the tree has them.** `kernel/src/drivers/acpi.rs:12-14`:
"[`reset_now`] and [`shutdown`] are the only two places this kernel writes a
register that ends the machine, and each stops every xHCI controller
([`stop::before_reset`]) before it does". Five paths reach those two:

- `sys_reboot` (`kernel/src/arch/syscall/machine.rs:113`) → `quiesce` (`:121`)
  → `acpi::reboot` (`:122`; `acpi.rs:283`) → `reset_now` (`acpi.rs:290`);
- `sys_shutdown` (`machine.rs:103`) → `quiesce` (`:107`) → `acpi::shutdown`
  (`:108`; `acpi.rs:325`);
- `deadline::expire` (`kernel/src/deadline.rs:208`) → `acpi::reset_now`
  (`:224`), no `quiesce`;
- `hardlockup::locked_up` (`kernel/src/hardlockup/mod.rs:313`) →
  `acpi::reset_now` (`:326`), no `quiesce`;
- `panic_reboot::reboot_now` (`kernel/src/panic_reboot.rs:162`) →
  `acpi::reboot` (`:167`) → `reset_now`, no `quiesce`.

`reset_now` calls `stop::before_reset` (`acpi.rs:313`) and then writes the
port (`:319`); `shutdown` calls it at `:331`. That stop is
`kernel/src/drivers/xhci/stop.rs`'s: one device class. `quiesce`
(`machine.rs:47-100`) disarms the watchdog, syncs, flushes the USB disks'
caches, waits for the log to be durable, seals `DONE` and calls
`xhci::seal_shut`; it kills no process and takes no device from one.

**A PCI function's own teardown exists, and it is reached from one place.**
`pcidev::release` (`kernel/src/pcidev/mod.rs:782`) calls `tear_down` (`:796`),
which disables bus mastering (`:797`), masks the function's MSI-X entry
(`:798`), empties its IOMMU domain (`:799`) and only then resets the function
(`:809`). The one caller of `pcidev::release` in the kernel
(`rg -n 'pcidev::release' kernel/src`, one hit) is `kernel/src/device.rs:75`,
inside `Claim`'s `Drop` — a dying process's handle table.

**So on every one of the five paths, a function whose holder is alive is left
as its holder last wrote it when the register is written.** No process is
asked to exit on any of them, so no `Claim` drops, `release` is not called,
and the function's bus-master enable, its MSI-X vector and both rings' base
addresses are whatever the driver programmed — the device may be mid-DMA into
the holder's memory at the write. This tree is in that state on every boot
that runs `netd` and then `reboot`: `system.toml` starts `netd` (`:24`) with
`devices = ["pci:1af4:1041"]` (`:70-72`), a virtio-net function it holds for
its life, and nothing between `/system/bin/reboot`'s `SYS_REBOOT` and the port
write asks it to let go.

**The I219 is the instance this file was found on, and no boot in this tree
reaches it:** the boot that puts `netd` in front of it, `tests/lancase`, is defined on
PR #442's branch `lan-metal` and on no branch that has merged. The bench boots
this was read on were of those branches. The one that motivated it — run 55's,
ended by `deadline::expire` → `reset_now` — is a path `quiesce` never runs on,
and on that boot the function had already been released: the sealed tail
reads `exit: netd pid=5 code=-1 cpu=22ms` and then
`pcidev: PCI 00:1f.6 [8086:15fc] released from slot 0` at 3.479 s
(`issues/kernel/a-120000-ms-boot-deadline-fired-132859-ms-late-on-the-t14.md`).
So the I219 went into that reset torn down — by the `Drop` a dying process
reaches and a live one does not.

**What the write does to the function is unmeasured.** The T14's register is
`0xCF9`: the kernel prints `ACPI: reset register SystemIO 0xcf9 <- 0x06` on
every boot there (`acpi.rs:271`; run 42's and run 51's kernel logs, at
0.000 s). Whether a full reset through that port clears a function's COMMAND
bus-master bit and MSI enable, or leaves them for the next operating system's
driver to find, is a platform question this tree has not asked, and nothing
here claims the next boot sees an armed device. What would measure it: a
ToyOS boot chained straight after a `sys_reboot` on which `netd` held the
function — the loop setting `BootNext` again — whose PCI enumeration prints
the function's COMMAND word and MSI control word before anything writes them,
beside the same print after a cold boot.

## What would answer it

Either `quiesce` tears down every live claim before the register — its stop
(`kernel/src/quiesce.rs`) halts every userland thread at a safe point and
tears down nothing those threads hold — or the reset path
does for each bound function what it already does for xHCI through
`stop::before_reset`: the register half of `tear_down`, bus mastering off and
the vector masked, written with no lock taken so the two bound-driven paths
can call it too, instead of relying on a `Drop` a live process is never asked
to run.
