---
status: open
kind: tooling
opened: 2026-09-28
---

# The loader's power-off with no entry behind it is proven by nothing

The loader's panic handler (`bootloader/src/main.rs`, `fn panic`) sets
`BootNext` to `toyos_update::entry::after`'s entry and resets, or powers the
machine off where `after` finds none. `after`'s `None` is host-tested
(`the_entry_after_is_later_in_the_order_and_boots_something_else`). The arm
that turns `None` into `ResetType::SHUTDOWN` is held by nothing but reading:
mutating `SHUTDOWN` to `WARM` reds no test.

**QEMU cannot reach the arm without overriding the firmware.** The pinned OVMF
(`edk2-gf0064ac3af`, commit `f0064ac3afa28e1aa3b6b9c22c6cf422a4bb8771`) puts
two active entries behind the boot stick's at every boot:

- **EFI Internal Shell** (`CATEGORY_BOOT`). `PlatformRegisterFvBootOption`
  (`OvmfPkg/Library/PlatformBootManagerLib/BdsPlatform.c:1712`) registers it
  with `LOAD_OPTION_ACTIVE`. `EfiBootManagerFindLoadOption`
  (`MdeModulePkg/Library/UefiBootManagerLib/BmLoadOption.c:558`) matches an
  existing option on its attributes too, so a Shell entry made inactive never
  matches, and a new active one is written.
- **The Boot Manager Menu, UiApp** (`CATEGORY_APP | ACTIVE | HIDDEN`,
  `BmBoot.c:2533`). `EfiBootManagerGetBootManagerMenu`
  (`MdeModulePkg/Universal/BdsDxe/BdsEntry.c:991`) registers it again whenever
  `BootOrder` lists none.
- `SetBootOrderFromQemu` (`BdsPlatform.c:1719`) rebuilds `BootOrder` from the
  active options alone (`CollectActiveOptions`). It puts the devices QEMU's
  `bootindex` names first, and `BootOrderComplete` appends the firmware-volume
  applications behind them.

So the stick's entry always has the Shell and UiApp behind it. The orchestrator
measured this over four passes: each time the test cleared both, the firmware
wrote them back active. It numbered the Shell 0002 and 0003 in turn, because
`BmGetFreeOptionNumber` treats as free every number `BootOrder` does not list.

**Exit**: either one of these, or the arm and its claim are deleted:

- a test on a machine whose own firmware leaves nothing active behind the
  stick's entry, judged by the power-off rather than by a reset;
- the handler's choice made a pure function that a host test holds, so that
  only the `ResetType` wiring is left unproven.
