---
status: open
kind: defect
opened: 2026-09-29
---

# The loader never sets the firmware's memory-overwrite request

At `Ubuntu-6.8.0-142.142`, the x86 EFI stub runs
`efi_enable_reset_attack_mitigation()` on every boot
(`drivers/firmware/efi/libstub/x86-stub.c:971`), which sets
`MemoryOverwriteRequestControl`=1 so firmware wipes RAM on the next boot after
an unclean reset (`CONFIG_RESET_ATTACK_MITIGATION=y`, config line 2457).
`bootloader/src` never writes that variable.

Root `CLAUDE.md`'s firmware rule puts every UEFI call ToyOS makes in the
loader, before `ExitBootServices`: the write is a `SetVariable` there, and
belongs with
`issues/boot-media/the-loader-does-only-what-must-precede-the-handover.md`.

**Exit**: the loader sets `MemoryOverwriteRequestControl`=1 before
`ExitBootServices` on every boot.
