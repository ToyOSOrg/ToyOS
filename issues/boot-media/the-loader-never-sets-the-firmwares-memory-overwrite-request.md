---
status: open
kind: defect
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
on the next boot after an unclean reset. That function first reads the
existing variable and returns without writing when the firmware has none
defined (`tpm.c:36-40`), and it never inspects
the status of the `set_efi_var` call it does make (`tpm.c:42-45`).
`bootloader/src` writes no variable under this GUID at all.

Root `CLAUDE.md`'s firmware rule puts every UEFI call ToyOS makes in the
loader, before `ExitBootServices`: the write is a `SetVariable` there, and
belongs with
`issues/boot-media/the-loader-does-only-what-must-precede-the-handover.md`.

**Exit**: on firmware that already defines `MemoryOverwriteRequestControl`,
the loader sets it to 1 before `ExitBootServices` and does not act on whether
the write succeeded; on firmware that does not define it, the loader performs
no `SetVariable` call under that GUID. The guest test `mor_is_set_where_defined`
has two boots, each on a fresh OVMF `VARS`. The first plants
`MemoryOverwriteRequestControl`=0 under that GUID with `vars::plant`
(`tests/common/update.rs`), boots, and asserts `vars::live` reads it as 1;
both helpers take only the anti-rollback floor's vendor today, so they gain
the vendor as an argument. The second plants nothing and asserts no variable
exists under that GUID afterward. Deleting the loader's `SetVariable` leaves
the planted 0 and reds the first boot; dropping its read of the variable
first makes the write unconditional and reds the second.
