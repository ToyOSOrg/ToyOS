---
status: open
kind: defect
opened: 2026-09-28
---

# A failed pass can fall to an entry the firmware never boots from its order

`toyos_update::entry::after` picks the entry a failed pass hands the machine to
as "the entry the firmware would have tried after this one". It skips inactive
entries and entries naming the loader's own ESP, but not an entry whose
category is not `LOAD_OPTION_CATEGORY_BOOT`. UEFI 2.10 §3.1.3 says a
`LOAD_OPTION_CATEGORY_APP` option is "not part of the normal boot processing".
EDK2's boot manager skips one in its `BootOrder` walk
(`MdeModulePkg/Universal/BdsDxe/BdsEntry.c:402-406` at `f0064ac3af`, the pinned
OVMF), but boots it when `BootNext` names it.

OVMF writes such an entry at every boot: the Boot Manager Menu (UiApp,
`CATEGORY_APP | ACTIVE | HIDDEN`), behind the stick's entry and the Shell. In
QEMU the Shell comes first, so no test has fallen to it. On a firmware whose
first follower is an application (a setup, diagnostics or menu entry), the
loader would set `BootNext` to it. The machine would then boot into that
application, which the firmware's own order never reaches, and the fall would
end there.

This is read from the specification and EDK2's source. It is not measured.

**Exit**: `after` passes over an entry whose category is not
`LOAD_OPTION_CATEGORY_BOOT`. A host test holds that, with a mutation that
drops the check and reds it.
