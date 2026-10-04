---
status: none
kind: rejected
opened: 2026-09-29
---

# The loader never sets the firmware's memory-overwrite request

At `Ubuntu-6.8.0-142.142`, the x86 EFI stub runs
`efi_enable_reset_attack_mitigation()` on every boot
(`drivers/firmware/efi/libstub/x86-stub.c:971`,
`CONFIG_RESET_ATTACK_MITIGATION=y`, config line 2457), which sets
`MemoryOverwriteRequestControl`=1 under vendor
`e20939be-32d4-41be-a150-897f85d49829`
(`drivers/firmware/efi/libstub/tpm.c:20-21`) so firmware wipes RAM
on the next boot after an unclean reset, where the firmware already defines
the variable (`tpm.c:36-40`). `bootloader/src` writes no variable under this
GUID.

**Declined** (owner, 2026-10-03): **"Keep crash records"**. ToyOS does not ask
the firmware to wipe memory at reset; crash records survive a reset in RAM
(`issues/toyos-explains-itself.md`). So ToyOS stays below Linux on
this one line of
`issues/the-kernel-is-at-least-as-secure-as-linux-on-every-machine-toyos-supports.md`:
whoever resets a ToyOS machine into a boot medium of their own can read what
the last boot left in RAM.

A request already set by another system is a different matter:
`issues/a-memory-overwrite-request-ubuntu-left-set-stays-set-under-toyos.md`.
